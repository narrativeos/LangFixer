#!/usr/bin/env python3
"""把 GEC gold 数据（jfleg_gold.json / mucgec_gold.json）转成 vegaml 训练/评测格式。

gold 格式（datasets/make_gold.py / make_gold_zh.py 产出）：
    [{"split": "dev", "src": "...", "edits": [{"op","src","ref","span","cat","cat_rule"}]}]

输出（每行一条 JSON）：
    {"text": "...", "label": 1|0, "error_type": "...", "spans": [[s,e],...], "split": "dev"}

- 有编辑的句子 → label=1，error_type 由 cat_rule/cat 归并（与 Rust 侧 map_vegaml_type 对齐）
- 无编辑的句子 → label=0（干净句，供二分类负样本）
- 按 split 切分：train 用 gold 的 train（如有），否则 dev 的 80%；eval 用其余

用法：
    python prepare_data.py --gold ../datasets/jfleg_gold.json --lang en --out data/en/
    python prepare_data.py --gold ../datasets/mucgec_gold.json --lang zh --out data/zh/
"""

from __future__ import annotations

import argparse
import json
import os
import random
import sys

# cat_rule / cat → vegaml error_type（与 gec-core/src/decision.rs 的 map_vegaml_type 对齐）
RULE_TO_VEGAML: dict[str, str] = {
    # 英文（JFLEG cat_rule 谱系）
    "tense": "word_order",
    "agreement": "word_order",
    "article": "collocation",
    "preposition": "collocation",
    "adverb": "collocation",
    "comparative": "word_order",
    "gerund-inf": "word_order",
    "spelling": "collocation",
    "other": "grammar",
    # 中文（MuCGEC cat_rule 谱系）
    "punct": "structure",
    "particle": "collocation",
    "variant": "collocation",
    "confusable": "collocation",
}


def map_error_type(cat_rule: str, cat: str) -> str:
    """cat_rule 优先，其次 cat；未知归 grammar。"""
    for key in (cat_rule, cat):
        if key in RULE_TO_VEGAML:
            return RULE_TO_VEGAML[key]
    return "grammar"


def convert(gold: list[dict], split_override: str | None = None) -> list[dict]:
    out = []
    for item in gold:
        src = item.get("src", "")
        edits = item.get("edits", [])
        if not src.strip():
            continue
        if edits:
            # 主错误类型：取出现次数最多的映射结果
            types = [map_error_type(e.get("cat_rule", ""), e.get("cat", "")) for e in edits]
            etype = max(set(types), key=types.count)
            out.append(
                {
                    "text": src,
                    "label": 1,
                    "error_type": etype,
                    "spans": [[int(e["span"][0]), int(e["span"][1])] for e in edits],
                    "split": split_override or item.get("split", "dev"),
                }
            )
        else:
            out.append(
                {
                    "text": src,
                    "label": 0,
                    "error_type": "",
                    "spans": [],
                    "split": split_override or item.get("split", "dev"),
                }
            )
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--gold", required=True, help="gold JSON 路径（如 datasets/jfleg_gold.json）")
    ap.add_argument("--lang", required=True, choices=["en", "zh"])
    ap.add_argument("--out", required=True, help="输出目录（写 train.jsonl / dev.jsonl）")
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    with open(args.gold, encoding="utf-8") as f:
        gold = json.load(f)

    rows = convert(gold)
    n_err = sum(r["label"] for r in rows)
    print(f"[prepare_data] {args.gold}: {len(rows)} 句（错误 {n_err} / 干净 {len(rows) - n_err}）")

    # 切分：gold 自带 train 则直接用；否则按 split 分组，dev 80/20 切
    splits = {}
    for r in rows:
        splits.setdefault(r["split"], []).append(r)
    if "train" in splits:
        train, dev = splits["train"], splits.get("dev", [])
    else:
        rng = random.Random(args.seed)
        pool = splits.get("dev", rows)
        rng.shuffle(pool)
        cut = max(1, int(len(pool) * 0.8))
        train, dev = pool[:cut], pool[cut:]
        for r in train + dev:
            r["split"] = "dev" if r in dev else "train"

    os.makedirs(args.out, exist_ok=True)
    for name, part in (("train", train), ("dev", dev)):
        path = os.path.join(args.out, f"{name}.jsonl")
        with open(path, "w", encoding="utf-8") as f:
            for r in part:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        print(f"[prepare_data] {path}: {len(part)} 句")

    # 类型分布报告
    dist: dict[str, int] = {}
    for r in rows:
        if r["label"]:
            dist[r["error_type"]] = dist.get(r["error_type"], 0) + 1
    print("[prepare_data] 错误类型分布:", dict(sorted(dist.items(), key=lambda kv: -kv[1])))


if __name__ == "__main__":
    main()
