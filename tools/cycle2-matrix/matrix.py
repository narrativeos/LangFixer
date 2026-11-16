#!/usr/bin/env python3
"""Cycle 2 消费端：生产日志 → 按型收益矩阵（EVOLUTION.md §11.14 Cycle 2）。

写侧机件已在产（不需要改一行 Rust）：
- detect_history.errors_json   每句检出（Error.source 标来源层；缺省=rule）
- detect_history.layers_json   该句各层参与情况 {layer,status,found}
  （llm 层 status ∈ ran/partial=复查过；skipped=路由未复查；unavailable=端点故障）
- feedback_log                 用户采纳/放弃留痕（adopt/reject，source=llm 即模型臂）

本脚本把三本账 join 成收益矩阵，按型给路由预算的 conditioning 信号：
  precision(型) = adopt/(adopt+reject)（该型检出被用户认可的比例 = P(改得对)）
  yield(型)     = adopted / LLM复查句数 ×1000（每千句复查预算换回的真修正）
并输出：
- matrix.json        矩阵 + join 质量 + 元数据（路由按型条件化的机读输入）
- matrix.md          人读报告（含 thin-sample 标注）
- error_book.jsonl   被拒 LLM 检出（错题本负例原料，含原句）
- whitelist 串       达门槛型清单，直接喂 router_labels.py --hit-types

已知口径损耗（如实报）：feedback_log 无 detect 外键，靠 (original, 时间窗)
回指检出；无窗可配的 adopt 匹配不上诉求 100%，报 matched/unmatched 两列。

用法:
    python3 tools/cycle2-matrix/matrix.py [--db langfixer.db] [--lang zh]
        [--since 2026-11-01] [--out-dir ~/tmp/cycle2] [--min-n 30] [--min-precision 0.5]
"""
from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "feature-model"))
from router_labels import map_cec_type  # noqa: E402  型码归一，与标签机件同源

LLM_REVIEWED = {"ran", "partial"}       # 该句 LLM 预算确实花掉
WINDOW_DAYS = 14                       # adopt 回指检出的时间窗


def parse_dt(s: str) -> datetime | None:
    for fmt in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"):
        try:
            return datetime.strptime(s[:19], fmt)
        except ValueError:
            continue
    return None


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--db", default=str(REPO / "langfixer.db"),
                    help="生产 SQLite（默认仓内 dev 实例；生产实例路径不同时显式传）")
    ap.add_argument("--lang", default="zh")
    ap.add_argument("--since", default="", help="只计该时间后的行（增量账）")
    ap.add_argument("--out-dir", default="~/tmp/cycle2")
    ap.add_argument("--min-n", type=int, default=30, help="白名单门槛：adopt+reject ≥")
    ap.add_argument("--min-precision", type=float, default=0.5, help="白名单门槛：precision ≥")
    a = ap.parse_args()
    db_p = Path(a.db).expanduser()
    if not db_p.is_file():
        sys.exit(f"找不到数据库：{db_p}")
    out = Path(a.out_dir).expanduser()
    out.mkdir(parents=True, exist_ok=True)
    con = sqlite3.connect(f"file:{db_p}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row

    # ── detect_history：句级复查账 + 检出事件 ──
    q = ("SELECT id, text, errors_json, layers_json, created_at FROM detect_history "
         "WHERE lang=?")
    args: list = [a.lang]
    if a.since:
        q += " AND created_at>=?"
        args.append(a.since)
    review_rows = 0          # LLM 复查过的句数（收益分母）
    llm_events: dict[int, list[dict]] = {}   # detect_id → llm 检出事件
    n_rows = 0
    for r in con.execute(q, args):
        n_rows += 1
        try:
            layers = json.loads(r["layers_json"]) if r["layers_json"] else []
        except json.JSONDecodeError:
            layers = []
        if any(l.get("layer") == "llm" and l.get("status") in LLM_REVIEWED for l in layers):
            review_rows += 1
        try:
            errors = json.loads(r["errors_json"])
        except json.JSONDecodeError:
            continue
        evs = [e for e in errors if e.get("source") == "llm"]
        if evs:
            llm_events[r["id"]] = [{"type": map_cec_type(e.get("error_type", "")),
                                    "original": e.get("original", ""),
                                    "correction": e.get("correction") or "",
                                    "created_at": r["created_at"], "sentence": r["text"],
                                    "adopted": False, "rejected": False} for e in evs]
            review_rows += not any(
                l.get("layer") == "llm" for l in layers)   # 无 layers 账的旧行有 llm 检出→补记复查

    # ── feedback_log：采纳/放弃账，按 (original, 时间窗) 回指检出 ──
    q = "SELECT * FROM feedback_log WHERE lang=? AND source='llm' AND action IN ('adopt','reject')"
    args = [a.lang]
    if a.since:
        q += " AND created_at>=?"
        args.append(a.since)
    by_type: dict[str, dict] = {}
    matched = unmatched = 0
    book: list[dict] = []

    def cell(t: str) -> dict:
        return by_type.setdefault(
            t, {"detected": 0, "adopted": 0, "rejected": 0,
                "fb_adopt": 0, "fb_reject": 0})

    for evs in llm_events.values():
        for e in evs:
            cell(e["type"])["detected"] += 1
    for r in con.execute(q, args):
        t = map_cec_type(r["error_type"])
        cell(t)["fb_" + r["action"]] += 1
        cands = [e for evs in llm_events.values() for e in evs
                 if e["original"] == r["original"] and e["type"] == t
                 and not e["adopted"] and not e["rejected"]]
        hit = None
        fb_t = parse_dt(r["created_at"])
        for e in sorted(cands, key=lambda x: x["created_at"], reverse=True):
            dt = parse_dt(e["created_at"])
            if fb_t and dt and fb_t >= dt and (fb_t - dt).days <= WINDOW_DAYS:
                hit = e
                break
        if hit is None and cands:          # 时间账不可解析时按最近一条回指（计损耗）
            hit = sorted(cands, key=lambda x: x["created_at"], reverse=True)[0]
        if hit is not None:
            hit["adopted" if r["action"] == "adopt" else "rejected"] = True
            matched += 1
            if r["action"] == "reject":
                book.append({"type": t, "original": r["original"],
                             "correction": r["correction"], "sentence": hit["sentence"],
                             "at": r["created_at"]})
        else:
            unmatched += 1
    for evs in llm_events.values():
        for e in evs:
            c = cell(e["type"])
            c["adopted"] += e["adopted"]
            c["rejected"] += e["rejected"]

    # ── 矩阵与门槛 ──
    matrix: dict[str, dict] = {}
    whitelist: list[str] = []
    for t, c in by_type.items():
        n_fb = c["fb_adopt"] + c["fb_reject"]
        prec = c["fb_adopt"] / n_fb if n_fb else None
        yld = c["adopted"] * 1000 / review_rows if review_rows else None
        matrix[t] = {**c, "n_feedback": n_fb, "precision": prec,
                     "yield_per_1k_reviews": yld, "thin": n_fb < a.min_n}
        if prec is not None and n_fb >= a.min_n and prec >= a.min_precision:
            whitelist.append(t)

    meta = {"db": str(db_p), "lang": a.lang, "since": a.since or None,
            "detect_rows": n_rows, "llm_review_rows": review_rows,
            "feedback_matched": matched, "feedback_unmatched": unmatched,
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "llm_review_statuses_counted": sorted(LLM_REVIEWED),
            "window_days": WINDOW_DAYS}
    json.dump({"meta": meta, "matrix": matrix},
              open(out / "matrix.json", "w", encoding="utf-8"),
              ensure_ascii=False, indent=1)
    with (out / "error_book.jsonl").open("w", encoding="utf-8") as f:
        for e in book:
            f.write(json.dumps(e, ensure_ascii=False) + "\n")

    # ── 报告 ──
    lines = [f"# Cycle 2 按型收益矩阵（{a.lang}，账至 {meta['generated_at']}）\n",
             f"- 检测行 {n_rows:,}；**LLM 复查句 {review_rows:,}**（收益分母）",
             f"- 反馈回指：命中 {matched} / 落空 {unmatched}（无外键，按 original+{WINDOW_DAYS} 天窗）\n",
             "| 型 | 检出 | 用户反馈 | adopt | reject | precision | 每千句复查换回 | 判定 |",
             "|---|---|---|---|---|---|---|---|"]
    for t in sorted(by_type, key=lambda x: -by_type[x]["detected"]):
        m = matrix[t]
        prec = "—" if m["precision"] is None else f"{m['precision']:.1%}"
        yld = "—" if m["yield_per_1k_reviews"] is None else f"{m['yield_per_1k_reviews']:.1f}"
        verdict = "入白名单" if t in whitelist else ("样本薄，不判定" if m["thin"] else "不达门槛")
        lines.append(f"| {t} | {m['detected']} | {m['n_feedback']} | {m['fb_adopt']} "
                     f"| {m['fb_reject']} | {prec} | {yld} | {verdict} |")
    lines += ["\n> 消费口径：`--hit-types` 白名单用达门槛型；低 precision 型不降级"
              "（§13 #12——按型条件化走预算权重/Cycle 3 型头，不走静态开关）。"]
    (out / "matrix.md").write_text("\n".join(lines), encoding="utf-8")
    print(f"检测行 {n_rows:,}，LLM 复查句 {review_rows:,}，"
          f"反馈回指 命中 {matched}/落空 {unmatched}")
    for t in sorted(by_type, key=lambda x: -by_type[x]["detected"]):
        m = matrix[t]
        prec = "—" if m["precision"] is None else format(m["precision"], ".2f")
        print(f"  {t:<24} 检出 {m['detected']:<6} 反馈 {m['n_feedback']:<5}"
              f"precision {prec}" + ("  [薄]" if m["thin"] else ""))
    print(f"白名单: {','.join(whitelist) if whitelist else '(暂无达标型)'}")
    print(f"→ {out / 'matrix.json'} / matrix.md / error_book.jsonl")


if __name__ == "__main__":
    main()
