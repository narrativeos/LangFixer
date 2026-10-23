#!/usr/bin/env python3
"""准备「用户采纳」平行语料资产（数据沉淀管线 P3）。

输入（api-server / desktop 采纳与放弃时自动沉淀，见 routes/l3.rs）：
- `l3_data/<lang>/adopted.jsonl`：句级 `src→dst` 平行对（用户确认的纠正，
  去重键 src|dst，上限 20000；编辑密度 >0.4 打 `weak` = 多半是用户改写）
- `l3_data/<lang>/rejected.jsonl`：片段级负偏好（被放弃的修正；偏好/DPO 材料）

治理（与 prepare_l3_pool 去污染同源）：
- 剔除 `weak` 句对（默认；`--include-weak` 可保留并另记）
- 剔除基准块表句（`load_blocklist()`：MuCGEC/CSCD/NaCGEC/CCTC/NLPCC23 gold src，
  防止评测集泄漏进训练资产）
- md5 稳定切分：`md5(src)` 决定 holdout 归属（同一句对在重跑/增量后切分不变），
  默认 20% → `adopted_holdout.jsonl`，其余 → `adopted_train.jsonl`
  （holdout 供未来自定义模型评测；train 供 SFT/继续预训练）

用法：
    python prepare_adopted.py --lang zh
    python prepare_adopted.py --lang zh --holdout 0.2 --include-weak
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)


def load_blocklist() -> set[str]:
    """去污染块表（与 prepare_l3_pool.load_blocklist 同源，无重依赖版本）：
    MuCGEC 全评测集 + CSCD/NaCGEC 基准 + CTCDataset 原生集
    （cscdns/nlpcc23/cctc/sig13/sig14/midu/ctc21）gold 源句。"""
    block: set[str] = set()
    ds = os.path.join(REPO, "datasets")
    p = os.path.join(ds, "mucgec/filter_sentences.txt")
    if os.path.exists(p):
        with open(p, encoding="utf-8") as f:
            block |= {ln.strip() for ln in f if ln.strip()}
    for name, idx in (("mucgec/MuCGEC_dev.txt", 1), ("mucgec/MuCGEC_test.txt", 0)):
        p = os.path.join(ds, name)
        if os.path.exists(p):
            with open(p, encoding="utf-8") as f:
                for ln in f:
                    parts = ln.rstrip("\n").split("\t")
                    if len(parts) > idx and parts[idx].strip():
                        block.add(parts[idx].strip())
    for name in ("cscd_gold.json", "nacgec_gold.json",
                 "cscdns_gold.json", "nlpcc23_gold.json", "cctc_gold.json",
                 "sig13_gold.json", "sig14_gold.json",
                 "midu_gold.json", "ctc21_gold.json"):
        p = os.path.join(ds, name)
        if os.path.exists(p):
            with open(p, encoding="utf-8") as f:
                block |= {item["src"] for item in json.load(f) if item.get("src")}
    return block


def read_jsonl(path: str) -> list[dict]:
    out: list[dict] = []
    if not os.path.exists(path):
        return out
    with open(path, encoding="utf-8") as f:
        for ln in f:
            ln = ln.strip()
            if not ln:
                continue
            try:
                v = json.loads(ln)
            except json.JSONDecodeError:
                continue
            if isinstance(v, dict):
                out.append(v)
    return out


def md5_holdout(src: str, frac: float) -> bool:
    """md5 稳定切分：同一 src 永远同一侧（增量追加不改变已有归属）。"""
    h = int(hashlib.md5(src.encode("utf-8")).hexdigest()[:8], 16)
    return h % 100000 < int(frac * 100000)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--lang", default="zh", choices=["zh", "en"])
    ap.add_argument("--data-dir", default=os.path.join(HERE, "l3_data"))
    ap.add_argument("--holdout", type=float, default=0.2)
    ap.add_argument("--include-weak", action="store_true",
                    help="保留 weak 句对（默认剔除：编辑密度过高，多半是用户改写）")
    args = ap.parse_args()

    lang_dir = os.path.join(args.data_dir, args.lang)
    adopted = read_jsonl(os.path.join(lang_dir, "adopted.jsonl"))
    rejected = read_jsonl(os.path.join(lang_dir, "rejected.jsonl"))

    block = load_blocklist()
    seen: set[tuple[str, str]] = set()
    train: list[dict] = []
    holdout: list[dict] = []
    n_weak = n_blocked = n_dup = 0
    for v in adopted:
        src, dst = v.get("src", ""), v.get("dst", "")
        if not src or not dst or src == dst:
            continue
        key = (src, dst)
        if key in seen:
            n_dup += 1
            continue
        seen.add(key)
        if v.get("weak") and not args.include_weak:
            n_weak += 1
            continue
        if src in block:
            n_blocked += 1
            continue
        (holdout if md5_holdout(src, args.holdout) else train).append(v)

    for name, rows in (("adopted_train.jsonl", train), ("adopted_holdout.jsonl", holdout)):
        with open(os.path.join(lang_dir, name), "w", encoding="utf-8") as f:
            for v in rows:
                f.write(json.dumps(v, ensure_ascii=False) + "\n")

    print(json.dumps({
        "lang": args.lang,
        "adopted_total": len(adopted),
        "train": len(train),
        "holdout": len(holdout),
        "dropped_weak": n_weak,
        "dropped_blocklist": n_blocked,
        "dropped_dup": n_dup,
        "rejected_total": len(rejected),
    }, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
