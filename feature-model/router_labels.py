#!/usr/bin/env python3
"""路由标签准备（EVOLUTION.md §11.10 标签配方 / §11.11 路由口径）。

在 jev-gec zh_v2 池（或同构 jsonl）上产出**句级路由标签**，schema 对齐 zh_v2 池
行（{id, text, label, source, origin, lang, split}），追加路由字段：

    route_label: llm_review | rule_decide | skip
    signals:     [rule_hit | lexical_hit | llm_hit | floor] 的任一子集
    error_types: v2 类型码列表（§11.11 T 族；来自 LLM 检出，缺 LLM 时为 []）

标签配方（§11.10：必须 gold ∪ LLM ∪ 词典 ∪ 采样地板取并集，不能只取 LLM——
纯 LLM 边际标签会把路由器盲区继承给模型，双盲锁死）：

- gold label=1（有错句）：规则定案档 pattern 覆盖 → `rule_decide`（零成本定案，
  不进可学习路由目标）；否则 `llm_review`——**gold 侧永不 skip**；
- LLM 臂在该句检出错误 → `llm_review`（llm_hit；经 --llm-out 缓存文件，
  缓存由真实 LLM 臂跑批产生，位置与 gold/采纳位置重合口径在缓存生成侧记账）；
- 词典/繁简 pattern 命中（含 advisory）→ `llm_review`（lexical 兜底，
  e65ae3c 词表信号立场）；
- 干净剩余：按句序每 --floor-every 句强制复查 1 句（floor，对齐 docjob.rs
  采样地板「每 N 个低风险块仍深度复查 1 个」），其余 `skip`
  （仅预算模式；**如实记 skipped，不算覆盖率**）。

可选 LLM 臂直跑（--llm-run，端点取 LLM_BASE_URL/LLM_MODEL/LLM_API_KEY，
prompt 用 prompts/zh_detect_prompt.json 的 JSON 契约）；默认不跑（成本高），
此时 error_types 为空、route 字段仅由 gold∪lexical∪floor 决定。

用法:
    cd feature-model
    python3 router_labels.py                       # zh_v2 train+dev 全量，无 LLM
    python3 router_labels.py --split train --cap 3000 --seed 20261115
    LLM_BASE_URL=... LLM_MODEL=... LLM_API_KEY=... python3 router_labels.py --llm-run --llm-limit 500

输出默认 /tmp/router_labels_{split}.jsonl（生成物不入库）。
"""
from __future__ import annotations

import argparse
import json
import os
import random
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))

import correct  # noqa: E402

# zh_v2 池默认位置（jev-gec 与本仓同级，同 build_pool.py 的 ROOT.parent 惯例）
DEFAULT_POOL_DIR = REPO.parent / "jev-gec" / "data" / "pools" / "zh_v2"

# CEC 教学分类 → v2 类型码（与 gec-core/src/llm.rs map_llm_error_type 同步，§11.11）
CEC_TO_V2 = {
    "": "grammar",
    "错别字": "spelling",
    "词语搭配错误": "collocation",
    "词性错误": "pos_error",
    "语序错误": "word_order",
    "成分残缺": "missing_component",
    "成分赘余": "redundant_component",
    "关联词使用错误": "conjunction",
    "指代不明": "reference",
    "语义逻辑不通": "logic",
    "标点": "punctuation",
}
# 内部细类型码原样透传集合（自训 COT 模型直接输出内部码）
V2_PASSTHROUGH = {
    "spelling", "extra_char", "missing_char", "collocation", "pos_error", "word_order",
    "missing_component", "redundant_component", "blend", "conjunction", "reference",
    "logic", "semantic_dup", "punctuation", "numeral", "quantity_unit", "serial_no",
    "grammar", "anomaly",
}


def map_cec_type(raw: str) -> str:
    raw = (raw or "").strip()
    if raw in V2_PASSTHROUGH:
        return raw
    return CEC_TO_V2.get(raw, raw or "grammar")


def load_jsonl(path: Path) -> list[dict]:
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def stratified_cap(rows: list[dict], cap: int, seed: int) -> list[dict]:
    """按 (source, label) 分层等比抽样；各层至少 1 条。"""
    if cap <= 0 or cap >= len(rows):
        return rows
    rng = random.Random(seed)
    strata: dict[tuple, list[dict]] = {}
    for r in rows:
        strata.setdefault((r.get("source", "?"), int(r.get("label", 0))), []).append(r)
    out: list[dict] = []
    for key in sorted(strata):
        xs = strata[key]
        k = max(1, round(cap * len(xs) / len(rows)))
        out.extend(rng.sample(xs, min(k, len(xs))))
    rng.shuffle(out)
    return out[:cap]


def _dump_cache(path: str, rows: list[dict], out: dict[str, list[dict]]) -> None:
    with open(path, "w", encoding="utf-8") as f:
        for r in rows:
            if r["id"] in out:
                f.write(json.dumps({"id": r["id"], "text": r["text"],
                                    "errors": out[r["id"]]},
                                   ensure_ascii=False) + "\n")


def run_llm_arm(rows: list[dict], limit: int, cache_out: str,
                seed_out: dict[str, list[dict]] | None = None,
                workers: int = 1) -> dict[str, list[dict]]:
    """真实 LLM 臂直跑（cost 高，仅抽样）；返回 {id: [{error_type, original}]}，
    并把原始检出同步落盘 cache_out（供 eval_router_arms.py --llm-cache 非 oracle 口径）。
    seed_out 已有的 id 直接跳过（断点续跑）；请求失败的 id 不记入结果
    （区分“查实无错”与“未查实”，非 oracle 评测时失败行不假装干净）。
    workers>1 时并发发请求（服务端为代理，吞吐受上游限制）。"""
    import threading
    from concurrent.futures import ThreadPoolExecutor, as_completed
    base = os.environ.get("LLM_BASE_URL", "").rstrip("/")
    model = os.environ.get("LLM_MODEL", "")
    key = os.environ.get("LLM_API_KEY", "")
    if not base or not model:
        sys.exit("--llm-run 需要 LLM_BASE_URL / LLM_MODEL（/ LLM_API_KEY）")
    prompt = json.load(open(REPO / "prompts" / "zh_detect_prompt.json", encoding="utf-8"))
    system, user_t = prompt["system"], prompt.get("user", "{{text}}")
    out: dict[str, list[dict]] = dict(seed_out or {})
    lock = threading.Lock()
    n_done = n_fail = 0
    todo = [r for r in rows[:limit] if r["id"] not in out]

    def one(r: dict) -> tuple[str, list[dict] | None]:
        body = json.dumps({
            "model": model, "temperature": 0,
            "messages": [{"role": "system", "content": system},
                         {"role": "user", "content": user_t.replace("{{text}}", r["text"])}],
        }).encode()
        req = urllib.request.Request(
            base + "/chat/completions",
            data=body, headers={"Content-Type": "application/json",
                                "Authorization": f"Bearer {key}"})
        # 服务端 502/500 阵发浪：重试 3 次退避，标签制备可穿越阵发故障
        last: Exception | None = None
        for attempt in range(3):
            try:
                with urllib.request.urlopen(req, timeout=120) as resp:
                    content = json.loads(resp.read())["choices"][0]["message"]["content"]
                items = json.loads(content[content.find("["):content.rfind("]") + 1])
                return r["id"], [{"error_type": map_cec_type(it.get("error_type", "")),
                                  "original": it.get("original", "")}
                                 for it in items if isinstance(it, dict)]
            except Exception as e:  # noqa: BLE001
                last = e
                time.sleep(3 * (attempt + 1))
        print(f"WARN llm row {r['id']} failed after 3 attempts: {last}",
              file=sys.stderr)
        return r["id"], None

    with ThreadPoolExecutor(max_workers=max(1, workers)) as ex:
        futs = [ex.submit(one, r) for r in todo]
        for fut in as_completed(futs):
            rid, errors = fut.result()
            with lock:
                if errors is not None:
                    out[rid] = errors
                else:
                    n_fail += 1
                n_done += 1
                if n_done % 50 == 0:
                    print(f"[llm] {n_done}/{len(todo)}（失败 {n_fail}）",
                          file=sys.stderr)
                if cache_out and n_done % 25 == 0:
                    _dump_cache(cache_out, rows[:limit], out)
    if cache_out:
        _dump_cache(cache_out, rows[:limit], out)
    if n_fail:
        print(f"[llm] 失败 {n_fail} 行（未记入缓存，评测时按未查实处理）",
              file=sys.stderr)
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pool-dir", default=str(DEFAULT_POOL_DIR),
                    help="jev zh_v2 池目录（train.jsonl / dev.jsonl）")
    ap.add_argument("--split", default="train,dev")
    ap.add_argument("--cap", type=int, default=0, help="每 split 分层抽样上限（0=全量）")
    ap.add_argument("--seed", type=int, default=20261115)
    ap.add_argument("--floor-every", type=int, default=3,
                    help="干净剩余句按序每 N 句强制 1 句复查（0=关闭）；对齐 docjob 采样地板")
    ap.add_argument("--llm-out", default="", help="LLM 臂检出缓存 jsonl（{id, errors}）；有则并入标签")
    ap.add_argument("--llm-run", action="store_true", help="直跑 LLM 臂（需 LLM_* 环境变量）")
    ap.add_argument("--llm-limit", type=int, default=500)
    ap.add_argument("--llm-workers", type=int, default=1,
                    help="--llm-run 并发请求数（单流实测 ~9s/行，串行 1200 行≈ 3h）")
    ap.add_argument("--llm-cache-out", default="/tmp/router_llm_arm_{split}.jsonl",
                    help="--llm-run 直跑结果的检出缓存落盘（eval 侧 --llm-cache 可用；支持 {split}）")
    ap.add_argument("--out", default="", help="输出路径模板（含 {split} 占位；默认 /tmp/router_labels_{split}.jsonl）")
    args = ap.parse_args()

    # 词典/pattern 信号与生产同源（correct.py + lexicons/zh_gec_lexicon.json）
    correct.load_confusables(str(REPO / "lexicons" / "zh_gec_lexicon.json"))

    llm_cache: dict[str, list[dict]] = {}
    if args.llm_out:
        for r in load_jsonl(Path(args.llm_out)):
            llm_cache[r["id"]] = r.get("errors", [])

    for split in [s.strip() for s in args.split.split(",") if s.strip()]:
        pool_p = Path(args.pool_dir) / f"{split}.jsonl"
        rows = stratified_cap(load_jsonl(pool_p), args.cap, args.seed)
        if args.llm_run:
            cache_p = Path(args.llm_cache_out.format(split=split))
            if cache_p.exists():
                for r in load_jsonl(cache_p):
                    llm_cache.setdefault(r["id"], r.get("errors", []))
            seed = {r["id"]: llm_cache[r["id"]] for r in rows
                    if r["id"] in llm_cache}
            if seed:
                print(f"[llm] 续跑：跳过已查实 {len(seed)} 行", file=sys.stderr)
            llm_cache.update(run_llm_arm(rows, args.llm_limit,
                                         cache_p, seed_out=seed,
                                         workers=args.llm_workers))

        out_p = Path(args.out.format(split=split) if args.out
                     else f"/tmp/router_labels_{split}.jsonl")
        stat = {"llm_review": 0, "rule_decide": 0, "skip": 0,
                "sig_rule": 0, "sig_lex": 0, "sig_llm": 0, "sig_floor": 0}
        with out_p.open("w", encoding="utf-8") as f:
            for i, r in enumerate(rows):
                text = r["text"]
                hits = correct.find_zh_hits(text)
                safe = [h for h in hits if not getattr(h, "advisory", False)]
                errors = llm_cache.get(r["id"], []) if llm_cache else []

                signals: list[str] = []
                if safe:
                    signals.append("rule_hit")
                if hits and not safe:
                    signals.append("lexical_hit")
                if errors:
                    signals.append("llm_hit")

                if int(r.get("label", 0)) == 1:
                    route = "rule_decide" if safe else "llm_review"
                elif errors or hits:
                    route = "llm_review"
                elif args.floor_every > 0 and i % args.floor_every == 0:
                    route, _ = "llm_review", signals.append("floor")
                else:
                    route = "skip"

                if errors:
                    types = sorted({e["error_type"] for e in errors
                                    if e.get("error_type") in V2_PASSTHROUGH})
                else:
                    types = []
                f.write(json.dumps({**r, "route_label": route,
                                    "signals": signals, "error_types": types},
                                   ensure_ascii=False) + "\n")
                stat[route] += 1
                stat["sig_rule"] += bool(safe)
                stat["sig_lex"] += bool(hits and not safe)
                stat["sig_llm"] += bool(errors)
                stat["sig_floor"] += "floor" in signals

        n = sum(stat[k] for k in ("llm_review", "rule_decide", "skip"))
        print(f"[{split}] n={n} → {out_p}")
        print(f"  llm_review {stat['llm_review']} ({stat['llm_review']/max(1,n):.1%}) | "
              f"rule_decide {stat['rule_decide']} | skip {stat['skip']} "
              f"(skipped 如实入账，不算覆盖率)")
        print(f"  signals: rule {stat['sig_rule']} / lexical {stat['sig_lex']} / "
              f"llm {stat['sig_llm']} / floor {stat['sig_floor']}"
              + ("" if llm_cache or args.llm_run else "  [LLM 臂未跑：标签=gold∪lexical∪floor]"))


if __name__ == "__main__":
    main()
