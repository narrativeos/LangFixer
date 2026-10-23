#!/usr/bin/env python3
"""构建 L3 特征模型的 CEC 混合训练池（工作流 D）：

数据源（twnlp HuggingFace，均 MIT；经 hf-mirror 镜像下载到 l3_pool_raw/，不入库）：
- `twnlp/csc_data/train.txt`（382K 拼写平行对：W271K/Medical/Lemon/ECSpell/CSCD-train）
- `twnlp/cgc_data/train_CGED.txt` + `train_FCGEC.txt`（57.8K 语法平行对）
- `twnlp/lang8_hsk/train.para`（1.57M 学员作文/HSK 混合平行对）

标签构造（与 CEC ELECTRA 门控的 has_error 二分类同构，但适配聚合特征）：
- **互补单侧分配**：每对平行句只贡献一侧——50% 概率正例 = 源句（有错），
  50% 概率负例 = 纠正句（干净）。若同对双侧都入池（src=正 / ref=负），两句近乎
  重复而标签相反，聚合特征无法区分 → 模型退化为先验（P≈0.5，已实验验证）；
  单侧分配消除矛盾对，配合 `rare_bigram` 特征（错别字制造干净语料中不存在的字对）
  可把句级 F1 从 ≈0.66 提到 ≈0.97（20K CSC 对实验）
- 句级任务类型由字级 diff 推导：spelling（仅单字替换）/ grammar（含多字/增删）/ mixed
- error_type 归并到现有模型类型词表：spelling→collocation（与 confusable/variant 同头）、
  grammar/mixed→grammar

分层 dev（CEC val_task_stratified.jsonl 思路）：
- spelling / grammar / mixed 各 2500 对 → 源句作正例；另 2500 对（不相交）→ 纠正句作负例
- 其余对按互补单侧分配进 train（默认规模 ≈ 50 万对 → 50 万行）

背景表（l3_bg/zh_bigrams.json，不入库）：
- 全部采样对的 ref（干净侧）字对频率 top-500K → `l3_finetune.rare_bigram` 特征的
  成员判定表；训练/推理（server.py）共用，`feature-model/l3_bg/` 已被 .gitignore 覆盖

去污染（从 train 与 dev 中剔除）：
- MuCGEC `filter_sentences.txt`（7137，与 MuCGEC 评测集重叠）
- MuCGEC dev/test 源句、CSCD 4 测试集源句、NaCGEC 源句（本仓库评测基准）
- CTCDataset 原生集源句：cscdns / nlpcc23 / cctc / sig13 / sig14 / midu / ctc21
  （`datasets/prepare_ctc.py` 转换，重叠审计见 `datasets/ctc_audit.py`）
- lang8_hsk 的 valid.para / MuCGEC.input / nlpcc18.input 是 CEC 自己的评测输入，
  本脚本只使用 train.para，天然不混入

用法：
    python prepare_l3_pool.py --out data/zh_pool
    python prepare_l3_pool.py --out data/zh_pool --csc 100000 --lang8 350000 --dev-per-type 2500
"""

from __future__ import annotations

import argparse
import json
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(REPO, "datasets"))
import cec_common as C  # noqa: E402  （download / detokenize / diff 工具）

RAW = os.path.join(HERE, "l3_pool_raw")

# 语料 → (HF 数据集, 文件, 任务类型上限)
CORPORA = {
    "csc": ("twnlp/csc_data", "train.txt", "spelling"),
    "cged": ("twnlp/cgc_data", "train_CGED.txt", "grammar"),
    "fcgec": ("twnlp/cgc_data", "train_FCGEC.txt", "grammar"),
    "lang8": ("twnlp/lang8_hsk", "train.para", "mixed"),
}

# 类型 → 现有模型类型词表（与 prepare_data.py 的 RULE_TO_TYPE 语义对齐）
TYPE_TO_ERROR = {"spelling": "collocation", "grammar": "grammar", "mixed": "grammar"}


def load_blocklist() -> set[str]:
    """去污染块表：MuCGEC 全评测集 + CSCD/NaCGEC 基准源句。"""
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


def pair_task_type(op_edits: list[tuple]) -> str:
    """字级 diff → 句级任务类型：spelling / grammar / mixed。"""
    has_sub1 = False
    has_struct = False
    for op, aw, bw, _span in op_edits:
        if op == "S" and len(aw) == 1 and len(bw) == 1:
            has_sub1 = True
        else:
            has_struct = True
    if has_sub1 and has_struct:
        return "mixed"
    return "spelling" if has_sub1 else "grammar"


def iter_pairs(dataset: str, filename: str, wanted: int, total: int, rng: random.Random):
    """流式读取平行对文件；wanted>0 时按随机行号采样 wanted 行。
    产出 (src, ref, edits)；仅保留有编辑的对。"""
    path = os.path.join(RAW, filename)
    C.download(dataset, filename, path)
    if wanted and wanted < total:
        wanted_lines = set(rng.sample(range(total), wanted))
    else:
        wanted_lines = None
    with open(path, encoding="utf-8") as f:
        for i, line in enumerate(f):
            if wanted_lines is not None and i not in wanted_lines:
                continue
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 2:
                continue
            src = C.detokenize(parts[0])
            ref = C.detokenize(parts[1])
            if not src.strip() or src == ref:
                continue
            edits = C.A.diff_spans(src, ref)
            if not edits:
                continue
            yield src, ref, edits


def count_lines(filename: str) -> int:
    p = os.path.join(RAW, filename)
    if not os.path.exists(p):
        dataset, _fn, _cap = next(c for c in CORPORA.values() if c[1] == filename)
        C.download(dataset, filename, p)
    with open(p, "rb") as f:
        return sum(1 for _ in f)


def main() -> None:
    ap = argparse.ArgumentParser(description="CEC 混合训练池（L3 句级门控重训练）")
    ap.add_argument("--out", required=True, help="输出目录（写 train.jsonl / dev.jsonl）")
    ap.add_argument("--csc", type=int, default=100_000, help="csc_data 采样行数（0=全部）")
    ap.add_argument("--lang8", type=int, default=350_000, help="lang8_hsk 采样行数（0=全部）")
    ap.add_argument("--dev-per-type", type=int, default=2500, help="分层 dev 每类型正/负例数")
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()
    rng = random.Random(args.seed)

    block = load_blocklist()
    print(f"[pool] 去污染块表: {len(block)} 句")

    # 收集全部采样对（去污染后），再统一做 dev 分层与互补单侧分配
    pairs: list[tuple[str, str, str, str]] = []  # (src, ref, task, source)
    for name, (dataset, filename, _cap) in CORPORA.items():
        total = count_lines(filename)
        wanted = {"csc": args.csc, "lang8": args.lang8}.get(name, 0)
        n_pairs = 0
        for src, ref, edits in iter_pairs(dataset, filename, wanted, total, rng):
            if src in block or ref in block:
                continue
            pairs.append((src, ref, pair_task_type(edits), name))
            n_pairs += 1
        print(f"[pool] {name}: {n_pairs} 对（文件 {total} 行，采样 {wanted or '全部'}）")

    # 背景表：全部采样对的 ref（干净侧）字对 top-500K
    from collections import Counter

    bg = Counter()
    for _src, ref, _t, _s in pairs:
        for i in range(len(ref) - 1):
            bg[ref[i:i + 2]] += 1
    bg_dir = os.path.join(HERE, "l3_bg")
    os.makedirs(bg_dir, exist_ok=True)
    bg_path = os.path.join(bg_dir, "zh_bigrams.json")
    with open(bg_path, "w", encoding="utf-8") as f:
        json.dump([b for b, _c in bg.most_common(500_000)], f, ensure_ascii=False)
    print(f"[pool] 背景表 {bg_path}: {len(bg)} 个 distinct bigram → top-500K")

    # 分层 dev：每类型 2500 对 → 源句作正例；另 2500 对（不相交）→ 纠正句作负例
    by_task: dict[str, list[tuple[str, str, str, str]]] = {}
    for p in pairs:
        by_task.setdefault(p[2], []).append(p)
    dev: list[dict] = []
    train_pairs: list[tuple[str, str, str, str]] = []
    for task in ("spelling", "grammar", "mixed"):
        tp = by_task.get(task, [])
        rng.shuffle(tp)
        cut = args.dev_per_type
        dev_pairs, neg_pairs, rest = tp[:cut], tp[cut:2 * cut], tp[2 * cut:]
        for src, _ref, _t, source in dev_pairs:
            dev.append({"text": src, "label": 1, "error_type": TYPE_TO_ERROR[task],
                        "spans": [], "source": source, "split": "dev"})
        for _src, ref, _t, source in neg_pairs:
            dev.append({"text": ref, "label": 0, "error_type": "",
                        "spans": [], "source": source, "split": "dev"})
        train_pairs += rest

    # train：互补单侧分配（每对 50% 概率只取 src / 只取 ref）
    train: list[dict] = []
    for src, ref, task, source in train_pairs:
        if rng.random() < 0.5:
            train.append({"text": src, "label": 1, "error_type": TYPE_TO_ERROR[task],
                          "spans": [], "source": source, "split": "train"})
        else:
            train.append({"text": ref, "label": 0, "error_type": "",
                          "spans": [], "source": source, "split": "train"})
    rng.shuffle(train)
    rng.shuffle(dev)

    os.makedirs(args.out, exist_ok=True)
    for name, part in (("train", train), ("dev", dev)):
        path = os.path.join(args.out, f"{name}.jsonl")
        with open(path, "w", encoding="utf-8") as f:
            for r in part:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        n_pos = sum(r["label"] for r in part)
        print(f"[pool] {path}: {len(part)} 句（正 {n_pos} / 负 {len(part) - n_pos}）")

    dist = Counter(r["error_type"] for r in dev if r["label"] == 1)
    print(f"[pool] dev 正例类型分布: {dict(dist)}")
    src_dist = Counter(r["source"] for r in train)
    print(f"[pool] train 来源分布: {dict(src_dist)}")


if __name__ == "__main__":
    main()