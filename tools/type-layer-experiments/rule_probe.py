#!/usr/bin/env python3
"""规则臂离线 EV 探针（§11.14 补二定案口径 → cycle2-matrix 机读块）。

与 experiments.py 实验3 同一本 rule_offline（不另起账），差别在**定案档位**：

- 定案档 = confusables（含 2 条条件条）+ anomaly strong 条 → 可作前置扣除；
- 提示档 = punct + anomaly weak 条 → 只进打扰账，不扣预算；
- punct 按**文档标点风格门**：全角占比 ≥0.5 的文档才启用（评测语料通篇
  半角 → 门关，punct 臂归零；补二 legacy 口径的 7,587 命中/2 中是门开账）。

账面（对 dataset_v22 语料 = train 侧标签批）：
- recall vs 原始金集字符；precision vs true_gold 字符（含漏标真错）；
- 打扰：命中字符 / 万字；按 true_gold 型分型的 TP 分布。

输出 ~/tmp/true_gold/rule_probe.json（matrix.py 嵌入为 rule_arm_offline）。
"""
from __future__ import annotations

import json
import os
import sys
from collections import Counter, defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from rule_offline import rule_hits, fullwidth_share  # noqa: E402

HOME = os.path.expanduser("~")
V22 = f"{HOME}/tmp/dataset_v22"
TG = f"{HOME}/tmp/true_gold"


def main() -> None:
    tg_chars: dict[str, set] = defaultdict(set)
    tg_type: dict[str, dict] = defaultdict(dict)
    for line in open(f"{TG}/true_gold.jsonl", encoding="utf-8"):
        g = json.loads(line)
        if g.get("unit") == "edit":
            span = range(g["a"], g["b"])
        else:
            span = range(g["pos"], g["pos"] + 1)
        tg_chars[g["id"]].update(span)
        for x in span:
            tg_type[g["id"]][x] = g.get("type", "?")

    gold_hit = tp_chars = hit_chars = total_chars = 0
    gold_chars = 0
    docs = doc_punct_on = 0
    tier_acc: dict[str, Counter] = {t: Counter() for t in ("定案", "提示")}
    by_type_tp: Counter = Counter()
    for f in sorted(os.listdir(V22)):
        if not f.endswith("_v22.jsonl"):
            continue
        for line in open(os.path.join(V22, f), encoding="utf-8"):
            r = json.loads(line)
            src = r["src"]
            docs += 1
            total_chars += len(src)
            if fullwidth_share(src) >= 0.5:
                doc_punct_on += 1
            goldc: set[int] = set()
            for e in r["gold_edits"]:
                goldc.update(range(e["a"], e["b"]))
            hitc: set[int] = set()
            tier_c: dict[str, set] = {"定案": set(), "提示": set()}
            for a, b, kind, tier in rule_hits(src, punct_mode="gated"):
                hitc.update(range(a, b))
                tier_c[tier].update(range(a, b))
            hit_chars += len(hitc)
            gold_chars += len(goldc)
            gold_hit += len(goldc & hitc)
            tgc = tg_chars.get(r["id"], set())
            tp_chars += len(hitc & tgc)
            for tier, s in tier_c.items():
                tier_acc[tier]["hits"] += len(s)
                tier_acc[tier]["gold_tp"] += len(s & goldc)
                tier_acc[tier]["tg_tp"] += len(s & tgc)
                for x in s & tgc:
                    by_type_tp[tg_type[r["id"]][x]] += 1
    out = {
        "meta": {"corpus": "dataset_v22 (train 侧标签批)", "docs": docs,
                 "chars": total_chars, "punct_style_gate": "全角≥0.5 启用",
                 "punct_active_docs": doc_punct_on,
                 "note": "POS 路径规则未复刻=下界；补二 legacy 账见 §11.14 补二/实验3",
                 "generated_by": "tools/type-layer-experiments/rule_probe.py"},
        "recall_vs_origin_gold": {"hit_chars": gold_hit, "gold_chars": gold_chars,
                                  "rate": gold_hit / max(1, gold_chars)},
        "precision_vs_true_gold": {"tp_chars": tp_chars, "hit_chars": hit_chars,
                                   "rate": tp_chars / max(1, hit_chars)},
        "tiers": {t: {**c, "per_10k_chars": 1e4 * c["hits"] / max(1, total_chars)}
                  for t, c in tier_acc.items()},
        "tp_by_true_gold_type": dict(by_type_tp.most_common()),
    }
    p = Path(f"{TG}/rule_probe.json")
    json.dump(out, open(p, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"docs={docs:,}（风格门开 {doc_punct_on}）chars={total_chars:,}")
    print(f"recall={gold_hit}/{gold_chars}={out['recall_vs_origin_gold']['rate']:.1%}  "
          f"precision(vs true_gold)={tp_chars}/{hit_chars}"
          f"={out['precision_vs_true_gold']['rate']:.1%}")
    for t, c in tier_acc.items():
        print(f"[{t}档] 命中 {c['hits']:,} 字符（每万字 {c['hits']*1e4/total_chars:.1f}）"
              f" ∩原始金 {c['gold_tp']} ∩true_gold {c['tg_tp']}")
    print("TP 按型:", dict(by_type_tp.most_common(8)))
    print(f"→ {p}")


if __name__ == "__main__":
    main()
