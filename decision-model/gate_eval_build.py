#!/usr/bin/env python3
"""构建中文门控评测集（决策模型 A/B 用）。

设计原则（EVOLUTION.md §11 教训）：
- 与旧决策模型训练池（MuC dev 1137 句）**零重叠**；
- 负类充足（旧模型只有 4% 干净句）：500 有错 / 500 干净；
- 多领域混合：CSCD（含 sig15/med/law 领域切片）、CSCD-NS、NLPCC23、CCTC；
- 新候选臂（Laya/Kev）预训练语料与这些池无关 → 无泄漏。

用法：.venv/bin/python gate_eval_build.py
输出：gate_eval/zh_gate_1k.jsonl  {text, label, source, split}
"""
from __future__ import annotations

import json
import os
import random
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
DATASETS = os.path.join(HERE, "..", "datasets")
OUT = os.path.join(HERE, "gate_eval")
SEED = 20261018

# 污染源：旧决策模型训练/评测池（MuC dev 全池）
contaminated = {it["src"].strip() for it in json.load(open(os.path.join(DATASETS, "mucgec_gold.json")))}

def load(fname: str) -> list[dict]:
    return json.load(open(os.path.join(DATASETS, fname)))

def candidates(items: list[dict], source: str) -> list[dict]:
    out = []
    for it in items:
        t = it.get("src", "").strip()
        if len(t) < 4 or t in contaminated:
            continue
        out.append({"text": t, "label": 1 if it.get("edits") else 0,
                    "source": source, "split": it.get("split", source)})
    return out

rng = random.Random(SEED)
pools: dict[tuple[str, int], list[dict]] = {}  # (source, per_label_quota)

# 各源配额（每类 label 各取一半）
plan = [("cscd_gold.json", 150, "cscd"), ("cscdns_gold.json", 150, "cscdns"),
        ("nlpcc23_gold.json", 100, "nlpcc23"), ("cctc_gold.json", 100, "cctc")]

seen: set[str] = set()
selected: list[dict] = []
for fname, quota, source in plan:
    cands = candidates(load(fname), source)
    by_label = defaultdict(list)
    for c in cands:
        if c["text"] not in seen:
            by_label[c["label"]].append(c)
    for label in (1, 0):
        bucket = by_label[label]
        # cscd 按领域分层
        if source == "cscd":
            by_split = defaultdict(list)
            for c in bucket:
                by_split[c["split"]].append(c)
            picked = []
            per = max(1, quota // max(len(by_split), 1))
            for _, lst in sorted(by_split.items()):
                rng.shuffle(lst)
                picked.extend(lst[:per])
            picked = picked[:quota]
        else:
            rng.shuffle(bucket)
            picked = bucket[:quota]
        seen.update(c["text"] for c in picked)
        selected.extend(picked)

rng.shuffle(selected)
os.makedirs(OUT, exist_ok=True)
out_path = os.path.join(OUT, "zh_gate_1k.jsonl")
with open(out_path, "w", encoding="utf-8") as f:
    for c in selected:
        f.write(json.dumps(c, ensure_ascii=False) + "\n")

n1 = sum(c["label"] for c in selected)
print(f"[gate_eval_build] → {out_path}")
print(f"  total={len(selected)} 有错={n1} 干净={len(selected)-n1}")
src = defaultdict(lambda: [0, 0])
for c in selected:
    src[c["source"]][c["label"]] += 1
for s, (e, cl) in sorted(src.items()):
    print(f"  {s}: 有错={e} 干净={cl}")
