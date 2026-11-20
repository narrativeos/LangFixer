#!/usr/bin/env python3
"""灰区句批量复查：把 P3 决策表要的 feedback_recheck 样本用现有闭环凑起来。

背景（方案 B，2026-11-20 定案）：复查样本仅 6 对（recheck_found 1 :
recheck_clean 5），灰区决策表 P3 需 ≥20。单句「LLM 复查」闭环已在
POST /gec/recheck 落地（复用 ③-T targeted_llm_review，复查结论自动落
feedback_log 的 recheck_found/recheck_clean，训练池 build_domain_pool
按 feedback_recheck 收录）——缺的只是把历史灰区句**批量**送进去。

本脚本从 detect_history 按 id 倒序找含「句级判断」条目（决策模型留下的
灰区句）、且 feedback_log 里尚无复查记录的句子，逐条 POST /gec/recheck。

前置：api-server 在跑且 LLM 已配置（否则复查路由回 503，脚本会明确报）。

用法：
  python3 tools/recheck-batch/batch_recheck.py --dry-run          # 只看候选
  python3 tools/recheck-batch/batch_recheck.py                    # 凑到 target
  python3 tools/recheck-batch/batch_recheck.py --target 30 --server http://127.0.0.1:4801
  --token 单机形态免鉴权；网关形态传 Bearer。
"""
from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def sentence_of(text: str, span: list[int]) -> str | None:
    """errors_json 的 span 是全文字节偏移（shared-model Error.span 口径）。"""
    try:
        return text.encode("utf-8")[span[0]:span[1]].decode("utf-8")
    except (UnicodeDecodeError, IndexError):
        return None


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--db", default=str(REPO / "langfixer.db"))
    ap.add_argument("--server", default="http://127.0.0.1:4801")
    ap.add_argument("--lang", default="zh")
    ap.add_argument("--target", type=int, default=20, help="feedback_recheck 样本目标条数")
    ap.add_argument("--token", default=None, help="网关形态 Bearer；单机形态留空")
    ap.add_argument("--token-db", default=None,
                    help="从该 auth.db 的 gw_meta.admin_token 读超管密钥（明文不进命令行）")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    db = sqlite3.connect(a.db)
    if a.token is None and a.token_db:
        row = sqlite3.connect(a.token_db).execute(
            "select value from gw_meta where key='admin_token'").fetchone()
        if not row:
            sys.exit(f"{a.token_db} 里没有 gw_meta.admin_token（未生成超管密钥？）")
        a.token = row[0]
    n_have = db.execute(
        "select count(*) from feedback_log where action in ('recheck_found','recheck_clean')"
    ).fetchone()[0]
    done = {r[0] for r in db.execute(
        "select original from feedback_log where action like 'recheck_%'"
    )}
    print(f"feedback_recheck 现有样本 {n_have} 条，目标 {a.target}，需补 {max(0, a.target - n_have)}")

    seen: set[str] = set()
    candidates: list[tuple[int, str, list[int], str]] = []
    cur = db.execute(
        "select id, text, errors_json from detect_history where lang=? order by id desc",
        (a.lang,),
    )
    skipped_bad = 0
    for row_id, text, errors_json in cur:
        try:
            errs = json.loads(errors_json)
        except Exception:
            skipped_bad += 1
            continue
        for e in errs if isinstance(errs, list) else []:
            if not isinstance(e, dict):
                continue
            if "句级判断" not in str(e.get("message", "")):
                continue
            sent = sentence_of(text, e.get("span", [0, 0]))
            if not sent or len(sent.strip()) < 8 or sent in done or sent in seen:
                continue
            seen.add(sent)
            candidates.append((row_id, text, e["span"], sent))
    print(f"候选灰区句 {len(candidates):,} 条（句级判断条目去重后；坏 errors_json {skipped_bad}）")
    if a.dry_run:
        for row_id, _, _, sent in candidates[:20]:
            print(f"  #{row_id} {sent[:56]}")
        return
    if not candidates:
        print("没有可复查的新句；先去标注模式跑检测或降低 --target。")
        return

    need = max(0, a.target - n_have)
    n_found = n_clean = 0
    for i, (row_id, text, span, sent) in enumerate(candidates):
        if n_found + n_clean >= need:
            break
        req = urllib.request.Request(
            a.server.rstrip("/") + "/gec/recheck",
            data=json.dumps({"lang": a.lang, "text": text, "span": span}).encode(),
            headers={"Content-Type": "application/json",
                     **({"Authorization": f"Bearer {a.token}"} if a.token else {})},
        )
        try:
            with urllib.request.urlopen(req, timeout=300) as resp:
                got = json.loads(resp.read())
        except urllib.error.HTTPError as e:
            body = e.read().decode("utf-8", "replace")[:200]
            print(f"HTTP {e.code} @#{row_id}：{body}", file=sys.stderr)
            if e.code == 503:
                print("AI 复查未配置——先在设置里配 LLM，再重跑本脚本。", file=sys.stderr)
                sys.exit(2)
            continue
        except urllib.error.URLError as e:
            print(f"连不上 {a.server}（api-server 在跑吗？）：{e}", file=sys.stderr)
            sys.exit(2)
        errs = got.get("errors", [])
        if errs:
            n_found += 1
            mark = f"found×{len(errs)} {errs[0].get('error_type', '')}"
        else:
            n_clean += 1
            mark = "clean"
        print(f"[{n_found + n_clean}/{need}] #{row_id} {mark}｜{sent[:40]}")
    print(f"复查完成：确证 {n_found} / 句级误报 {n_clean}；"
          f"现共 {n_have + n_found + n_clean} 条 feedback_recheck 样本")


if __name__ == "__main__":
    main()
