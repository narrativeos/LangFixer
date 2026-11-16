#!/usr/bin/env python3
"""Cycle 2 消费端：生产日志 → 按型收益矩阵（EVOLUTION.md §11.14 Cycle 2）。

写侧机件已在产（不需要改一行 Rust）：
- detect_history.errors_json   每句检出（Error.source 标来源层；缺省=rule）
- detect_history.layers_json   该句各层参与情况 {layer,status,found}
  （llm 层 status ∈ ran/partial=复查过；skipped=路由未复查；unavailable=端点故障）
- feedback_log                 用户采纳/放弃留痕（adopt/reject，source=llm 即模型臂）

分臂（§11.14 补四）：矩阵按臂拆行——llm 臂（source=='llm'）照旧出
whitelist/yield；规则臂（source 缺省或非 llm）入矩阵但**不出白名单、不算
yield**（规则免费，打扰账由 rule_probe.json 离线 EV 替代：每千字命中字符）。

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
    ap.add_argument("--rule-probe", default="~/tmp/true_gold/rule_probe.json",
                    help="规则臂离线 EV（rule_probe.py 产物；存在则嵌入 matrix.json）")
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
    events_by_arm: dict[str, dict[int, list[dict]]] = {"llm": {}, "rule": {}}
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
        for arm in events_by_arm:
            evs = [e for e in errors if (e.get("source") == "llm") == (arm == "llm")]
            if evs:
                events_by_arm[arm][r["id"]] = [
                    {"type": map_cec_type(e.get("error_type", "")),
                     "original": e.get("original", ""),
                     "correction": e.get("correction") or "",
                     "created_at": r["created_at"], "sentence": r["text"],
                     "adopted": False, "rejected": False} for e in evs]
        if events_by_arm["llm"].get(r["id"]):
            review_rows += not any(
                l.get("layer") == "llm" for l in layers)   # 无 layers 账的旧行有 llm 检出→补记复查

    # ── feedback_log：采纳/放弃账，按臂分账，(original, 时间窗) 回指检出 ──
    q = "SELECT * FROM feedback_log WHERE lang=? AND action IN ('adopt','reject')"
    args = [a.lang]
    if a.since:
        q += " AND created_at>=?"
        args.append(a.since)
    by_arm: dict[str, dict[str, dict]] = {"llm": {}, "rule": {}}
    matched = unmatched = 0
    book: list[dict] = []

    def cell(arm: str, t: str) -> dict:
        return by_arm[arm].setdefault(
            t, {"detected": 0, "adopted": 0, "rejected": 0,
                "fb_adopt": 0, "fb_reject": 0})

    for arm, evs_by_row in events_by_arm.items():
        for evs in evs_by_row.values():
            for e in evs:
                cell(arm, e["type"])["detected"] += 1
    for r in con.execute(q, args):
        arm = "llm" if r["source"] == "llm" else "rule"
        t = map_cec_type(r["error_type"])
        cell(arm, t)["fb_" + r["action"]] += 1
        cands = [e for evs in events_by_arm[arm].values() for e in evs
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
            if r["action"] == "reject" and arm == "llm":   # 错题本只收模型臂负例
                book.append({"type": t, "original": r["original"],
                             "correction": r["correction"], "sentence": hit["sentence"],
                             "at": r["created_at"]})
        else:
            unmatched += 1
    for arm, evs_by_row in events_by_arm.items():
        for evs in evs_by_row.values():
            for e in evs:
                c = cell(arm, e["type"])
                c["adopted"] += e["adopted"]
                c["rejected"] += e["rejected"]

    # ── 矩阵与门槛（whitelist/yield 只出自 llm 臂）──
    matrix: dict[str, dict] = {}
    for arm, by_type in by_arm.items():
        for t, c in by_type.items():
            n_fb = c["fb_adopt"] + c["fb_reject"]
            prec = c["fb_adopt"] / n_fb if n_fb else None
            yld = (c["adopted"] * 1000 / review_rows
                   if review_rows and arm == "llm" else None)
            matrix[arm + "/" + t] = {**c, "arm": arm, "n_feedback": n_fb,
                                     "precision": prec,
                                     "yield_per_1k_reviews": yld,
                                     "thin": n_fb < a.min_n}
    whitelist = [t for key, m in matrix.items()
                 if key.startswith("llm/") and m["precision"] is not None
                 and m["n_feedback"] >= a.min_n
                 and m["precision"] >= a.min_precision for t in [key[4:]]]

    # 规则臂离线 EV（前置扣除的机读证据；缺件则如实缺席）
    rule_arm_offline = None
    rp = Path(a.rule_probe).expanduser()
    if rp.is_file():
        rule_arm_offline = json.load(open(rp, encoding="utf-8"))

    meta = {"db": str(db_p), "lang": a.lang, "since": a.since or None,
            "detect_rows": n_rows, "llm_review_rows": review_rows,
            "feedback_matched": matched, "feedback_unmatched": unmatched,
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "llm_review_statuses_counted": sorted(LLM_REVIEWED),
            "window_days": WINDOW_DAYS,
            "arms": sorted(events_by_arm),
            "rule_arm_cells": sum(1 for k in matrix if k.startswith("rule/"))}
    json.dump({"meta": meta, "matrix": matrix,
               "rule_arm_offline": rule_arm_offline},
              open(out / "matrix.json", "w", encoding="utf-8"),
              ensure_ascii=False, indent=1)
    with (out / "error_book.jsonl").open("w", encoding="utf-8") as f:
        for e in book:
            f.write(json.dumps(e, ensure_ascii=False) + "\n")

    # ── 报告（分臂两表）──
    llm_types = [k[4:] for k in matrix if k.startswith("llm/")]
    rule_types = [k[5:] for k in matrix if k.startswith("rule/")]
    lines = [f"# Cycle 2 按型收益矩阵（{a.lang}，账至 {meta['generated_at']}）\n",
             f"- 检测行 {n_rows:,}；**LLM 复查句 {review_rows:,}**（收益分母）",
             f"- 反馈回指：命中 {matched} / 落空 {unmatched}（无外键，按 original+{WINDOW_DAYS} 天窗）\n",
             "## llm 臂（模型预算：whitelist/yield 只出自此臂）\n",
             "| 型 | 检出 | 用户反馈 | adopt | reject | precision | 每千句复查换回 | 判定 |",
             "|---|---|---|---|---|---|---|---|"]
    for t in sorted(llm_types, key=lambda x: -matrix["llm/" + x]["detected"]):
        m = matrix["llm/" + t]
        prec = "—" if m["precision"] is None else f"{m['precision']:.1%}"
        yld = "—" if m["yield_per_1k_reviews"] is None else f"{m['yield_per_1k_reviews']:.1f}"
        verdict = "入白名单" if t in whitelist else ("样本薄，不判定" if m["thin"] else "不达门槛")
        lines.append(f"| {t} | {m['detected']} | {m['n_feedback']} | {m['fb_adopt']} "
                     f"| {m['fb_reject']} | {prec} | {yld} | {verdict} |")
    lines += ["\n## 规则臂（非 llm 臂：缺省 rule，含少量 feature/knowledge；免费前置，"
              "不出白名单、不算 yield，打扰账见离线 EV）\n",
              "| 型 | 检出 | 用户反馈 | adopt | reject | precision |",
              "|---|---|---|---|---|---|"]
    for t in sorted(rule_types, key=lambda x: -matrix["rule/" + x]["detected"]):
        m = matrix["rule/" + t]
        prec = "—" if m["precision"] is None else f"{m['precision']:.1%}"
        lines.append(f"| {t} | {m['detected']} | {m['n_feedback']} "
                     f"| {m['fb_adopt']} | {m['fb_reject']} | {prec} |")
    if not rule_types:
        lines.append("| （dev 库规则臂零反馈/零检出——生产账接通后补） | | | | | |")
    if rule_arm_offline:
        rmo = rule_arm_offline
        lines += ["\n### 规则臂离线 EV（rule_probe.py，生产检出的离线替身）\n",
                  f"- 语料 {rmo['meta']['docs']:,} docs / {rmo['meta']['chars']:,} 字符；"
                  f"recall vs 原始金 {rmo['recall_vs_origin_gold']['rate']:.1%}；"
                  f"precision vs true_gold {rmo['precision_vs_true_gold']['rate']:.1%}",
                  "- 定案档（可前置扣除）打扰 {:.1f}/万字；提示档 {:.1f}/万字".format(
                      rmo["tiers"]["定案"]["per_10k_chars"],
                      rmo["tiers"]["提示"]["per_10k_chars"])]
    else:
        lines.append("\n> 规则臂离线 EV 缺席（rule_probe.json 不存在——先跑 "
                     "tools/type-layer-experiments/rule_probe.py）。")
    lines += ["\n> 消费口径：`--hit-types` 白名单用达门槛型；低 precision 型不降级"
              "（§13 #12——按型条件化走预算权重/Cycle 3 型头，不走静态开关）。"
              "\n> 前置扣除：规则臂定案档命中的字符不进 LLM 预算（eval "
              "`--rule-subtract` 已量化残差曲线）。"]
    (out / "matrix.md").write_text("\n".join(lines), encoding="utf-8")
    print(f"检测行 {n_rows:,}，LLM 复查句 {review_rows:,}，"
          f"反馈回指 命中 {matched}/落空 {unmatched}")
    for arm, types in (("llm", llm_types), ("rule", rule_types)):
        print(f"[{arm} 臂]")
        for t in sorted(types, key=lambda x: -matrix[f"{arm}/{x}"]["detected"]):
            m = matrix[f"{arm}/{t}"]
            prec = "—" if m["precision"] is None else format(m["precision"], ".2f")
            print(f"  {t:<24} 检出 {m['detected']:<6} 反馈 {m['n_feedback']:<5}"
                  f"precision {prec}" + ("  [薄]" if m["thin"] else ""))
        if not types:
            print("  （零检出）")
    print(f"白名单(llm 臂): {','.join(whitelist) if whitelist else '(暂无达标型)'}")
    print(f"→ {out / 'matrix.json'} / matrix.md / error_book.jsonl")


if __name__ == "__main__":
    main()
