#!/usr/bin/env python3
"""构建 Kev 微调语料（gate_ft）——中文 GEC 门控层微调试点。

设计原则（EVOLUTION.md §11.5/§11.6）：
- 测试集冻结 = gate_eval/zh_gate_1k.jsonl（996 句），训练/开发集与之
  **文本级零重叠**；验收必须回到 gate_eval 跑，不许用训练侧数字；
- 问题 schema 与 gate_eval.py 完全一致（两选项 choice，A=有错 B=干净），
  训完的 checkpoint 用 kev.serve 起服务后可直接被 gate_eval.py 打分；
- dev 切分用于温度拟合与早停观察；train 类 balanced（clean 全量 +
  error 随机下采样到同量）；
- 数据格式 = kev 训练 JSONL（README「By Hand」：state + questions + label）。

用法：.venv_gate/bin/python gate_ft_build.py
输出：gate_ft/train.jsonl  gate_ft/dev.jsonl  gate_ft/manifest.json
"""
from __future__ import annotations

import json
import os
import random
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
DATASETS = os.path.join(HERE, "..", "datasets")
OUT = os.path.join(HERE, "gate_ft")
SEED = 20261019

# 冻结测试集（绝不训练）
GATE_EVAL = os.path.join(HERE, "gate_eval", "zh_gate_1k.jsonl")

# 与 gate_eval.py QUESTIONS 完全一致
Q = {
    "type": "choice",
    "instructions": "Does the state text contain a language error that needs correction (typo, grammar, punctuation, or word usage)?",
    "criteria": {
        "A": "The state text contains a language error that needs correction.",
        "B": "The state text is correct and needs no correction.",
    },
}

POOLS = [  # (文件, 源名)
    ("cscd_gold.json", "cscd"),
    ("cscdns_gold.json", "cscdns"),
    ("nlpcc23_gold.json", "nlpcc23"),
    ("cctc_gold.json", "cctc"),
    ("nacgec_gold.json", "nacgec"),
    ("mucgec_gold.json", "mucgec"),  # 对旧 sklearn 是污染源；对 kev 微调无害（与 gate_eval 零重叠即可）
]

DEV_QUOTA = {  # 每源 (error, clean) 配额 → dev 共 1000
    "cscd": (90, 90), "cscdns": (180, 180), "nlpcc23": (60, 60),
    "cctc": (80, 80), "nacgec": (70, 40), "mucgec": (20, 50),
}

frozen = {json.loads(l)["text"] for l in open(GATE_EVAL, encoding="utf-8")}

rng = random.Random(SEED)
by_src_label: dict[tuple[str, int], list[str]] = defaultdict(list)
seen: set[str] = set(frozen)
for fname, source in POOLS:
    for it in json.load(open(os.path.join(DATASETS, fname))):
        t = it.get("src", "").strip()
        label = 1 if it.get("edits") else 0
        if len(t) < 4 or t in seen:
            continue
        seen.add(t)
        by_src_label[(source, label)].append(t)

def rec(text: str, label: int) -> dict:
    q = dict(Q)
    q["label"] = "A" if label else "B"
    return {"state": text, "questions": {"has_error": q}}

def take(key: tuple[str, int], n: int) -> list[str]:
    pool = by_src_label[key]
    n = min(n, len(pool))
    picked = rng.sample(pool, n)
    for t in picked:
        pool.remove(t)
    return picked

dev_rows, dev_stat = [], defaultdict(lambda: [0, 0])
for source, (ne, nc) in DEV_QUOTA.items():
    for t in take((source, 1), ne):
        dev_rows.append(rec(t, 1)); dev_stat[source][0] += 1
    for t in take((source, 0), nc):
        dev_rows.append(rec(t, 0)); dev_stat[source][1] += 1
rng.shuffle(dev_rows)

clean_left = sum(len(v) for (s, l), v in by_src_label.items() if l == 0)
err_left = sum(len(v) for (s, l), v in by_src_label.items() if l == 1)
n_err = min(err_left, clean_left)  # 平衡：error 下采样到 clean 同量
train_rows = []
for (source, label), pool in sorted(by_src_label.items()):
    if label == 0:
        for t in pool:
            train_rows.append(rec(t, 0))
    else:
        for t in rng.sample(pool, min(n_err // 6, len(pool))):  # 每源均摊，防 nacgec 独大
            train_rows.append(rec(t, 1))
# 均摊后若仍有缺口，从剩余 error 全池补齐到 n_err
have_err = sum(1 for r in train_rows if r["questions"]["has_error"]["label"] == "A")
rest = [t for (s, l), pool in by_src_label.items() if l == 1 for t in pool
        if t not in {r["state"] for r in train_rows}]
for t in rng.sample(rest, max(0, n_err - have_err)):
    train_rows.append(rec(t, 1))
rng.shuffle(train_rows)

os.makedirs(OUT, exist_ok=True)
for name, rows in (("train.jsonl", train_rows), ("dev.jsonl", dev_rows)):
    with open(os.path.join(OUT, name), "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")

n_tr_err = sum(1 for r in train_rows if r["questions"]["has_error"]["label"] == "A")
n_dev_err = sum(1 for r in dev_rows if r["questions"]["has_error"]["label"] == "A")
manifest = {
    "seed": SEED, "test": "gate_eval/zh_gate_1k.jsonl (frozen, 996)",
    "train": len(train_rows), "train_error": n_tr_err, "train_clean": len(train_rows) - n_tr_err,
    "dev": len(dev_rows), "dev_error": n_dev_err, "dev_clean": len(dev_rows) - n_dev_err,
    "dev_by_source": {s: {"error": e, "clean": c} for s, (e, c) in sorted(dev_stat.items())},
    "overlap_with_gate_eval": 0,
}
json.dump(manifest, open(os.path.join(OUT, "manifest.json"), "w"), indent=2, ensure_ascii=False)
print(json.dumps(manifest, indent=2, ensure_ascii=False))
# 泄漏自检
dev_texts = {r["state"] for r in dev_rows} | {r["state"] for r in train_rows}
assert not (dev_texts & frozen), "LEAK: train/dev 与 gate_eval 重叠！"
print("[gate_ft_build] 泄漏自检通过：train+dev 与 gate_eval 零重叠")
