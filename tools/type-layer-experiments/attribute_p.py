#!/usr/bin/env python3
"""p̂(t) 归因账：按门控 label 侧分列的 p̂ + wo_exempt 判定表（§11.14 补五后续）。

回答三个问题（eval 重赛的补充分析，不重训任何东西）：
1. 每型 p̂(t) = P(LLM 缓存 span 覆盖 | gold span 存在)，按 train 池行
   label=1（门控看见）/ label=0（门控未见但 gold 有）两侧分列——
   label=0 侧 p̂ 是第二召回通道的期望收益上界；
2. word_order 的 p̂ 真值 vs 全体先验：显著低 → 标签侧 wo_exempt 降级开关
   可退役（EV 表达已自动降权，§13 #12 立场）；
3. 测量面覆盖率（train 池 ∩ gold_counts ∩ gain cache）与饥饿型清单。

重合口径与 eval_router_arms llm_cache_load 同源（original 全串 find）。
用法:
    python3 tools/type-layer-experiments/attribute_p.py \
        [--gain /tmp/router_llm_arm_train.jsonl] [--counts ~/tmp/true_gold/gold_counts.jsonl]
"""
from __future__ import annotations

import argparse
import json
import os
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
POOL = REPO.parent / "jev-gec" / "data" / "pools" / "zh_v2" / "train.jsonl"
MIN_N = 50


def find_all(text: str, frag: str):
    i = text.find(frag)
    while i >= 0:
        yield i
        i = text.find(frag, i + 1)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--gain", default="/tmp/router_llm_arm_train.jsonl")
    ap.add_argument("--counts", default="~/tmp/true_gold/gold_counts.jsonl")
    ap.add_argument("--out", default="~/tmp/true_gold/p_attribution.json")
    a = ap.parse_args()

    gc = {}
    for l in Path(a.counts).expanduser().read_text(encoding="utf-8").splitlines():
        if l.strip():
            r = json.loads(l)
            gc[r["text"]] = r
    gain: dict[str, list[tuple[int, int]]] = defaultdict(list)
    for l in Path(a.gain).expanduser().read_text(encoding="utf-8").splitlines():
        if not l.strip():
            continue
        r = json.loads(l)
        t = r.get("text", "")
        for it in r.get("errors", []):
            frag = it.get("original", "")
            if frag:
                gain[t].extend((m, m + len(frag)) for m in find_all(t, frag))

    tot: dict = defaultdict(lambda: [0, 0])   # type → [side0, side1]
    cov: dict = defaultdict(lambda: [0, 0])
    n_meas = [0, 0]
    pool = json.load(open(POOL, encoding="utf-8")) if POOL.suffix == ".json" else \
        [json.loads(l) for l in open(POOL, encoding="utf-8")]
    for r in pool:
        t = r["text"]
        if t not in gc or t not in gain:
            continue
        side = int(bool(r.get("label")))
        n_meas[side] += 1
        gs = gain[t]
        for a0, b0, ty in gc[t].get("spans", []):
            tot[ty][side] += 1
            if any(s <= b0 and a0 <= e for s, e in gs):
                cov[ty][side] += 1

    all_t = sum(sum(v) for v in tot.values())
    all_c = sum(sum(v) for v in cov.values())
    all_p = all_c / max(1, all_t)
    rows = []
    for ty in sorted(tot, key=lambda x: -(cov[x][0] + cov[x][1]) / max(1, tot[x][0] + tot[x][1])):
        n_all = tot[ty][0] + tot[ty][1]
        p = (cov[ty][0] + cov[ty][1]) / max(1, n_all)
        rows.append({
            "type": ty, "n_span": n_all, "n_side0_gate盲": tot[ty][0],
            "n_side1_gate见": tot[ty][1],
            "p_hat": p, "p_side0": cov[ty][0] / max(1, tot[ty][0]),
            "p_side1": cov[ty][1] / max(1, tot[ty][1]),
            "measured": n_all >= MIN_N,
        })
    wo = next((x for x in rows if x["type"] == "word_order"), None)
    wo_verdict = ("无数据" if wo is None else
                  "starved（span 不足，先验不变）" if not wo["measured"] else
                  f"实测 p̂={wo['p_hat']:.3f} vs 先验 {all_p:.3f} → " +
                  ("显著低，wo_exempt 可退役（EV 已自动降权）"
                   if wo["p_hat"] < 0.9 * all_p else "不低于先验，wo_exempt 保留"))
    out = {"meta": {"gain_cache": a.gain, "counts": a.counts,
                    "n_meas_side0_gate盲": n_meas[0], "n_meas_side1_gate见": n_meas[1],
                    "all_p": all_p, "min_n": MIN_N},
           "by_type": rows, "word_order_verdict": wo_verdict}
    Path(a.out).expanduser().write_text(
        json.dumps(out, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"测量面：label=1 侧 {n_meas[1]:,} 行 / label=0 侧 {n_meas[0]:,} 行；"
          f"全体先验 p̂={all_p:.3f}")
    print(f"{'型':<12} {'span':>6} {'p̂':>6} {'p̂|门见':>7} {'p̂|门盲':>7} 判定")
    for x in rows:
        print(f"{x['type']:<12} {x['n_span']:>6} {x['p_hat']:>6.3f} "
              f"{x['p_side1']:>7.3f} {x['p_side0']:>7.3f} "
              f"{'' if x['measured'] else '⚠先验'}")
    print(f"word_order: {wo_verdict}")
    print(f"→ {Path(a.out).expanduser()}")


if __name__ == "__main__":
    main()
