#!/usr/bin/env python3
"""重建 zh char 3-gram clean 频率表（方案 B：表扩充）。

背景：旧表仅 3.4 万条（≈1.4k clean 句），正常句大面积 OOV → 灰区率 91.5%。
本脚本从本地 CEC 平行对池的 **ref（纠正后干净）侧** 重建，规模 ~100 万对。

数据源（tab 分隔 `src \t ref`，取 ref 侧；由 prepare_l3_pool.py 下载，不入库）：
- l3_pool_raw/train.txt        CSC 拼写平行对（382K，全量）
- l3_pool_raw/train_CGED.txt   语法平行对（全量）
- l3_pool_raw/train_FCGEC.txt  语法平行对（全量）
- l3_pool_raw/train.para       lang8_hsk（1.57M，按 --lang8 采样，默认 600K）
- l3_data/zh/confirmed.jsonl   真实用户 clean 句（label=0，min_count=1 直接入表）

去污染：prepare_l3_pool.load_blocklist()（MuCGEC 全评测集 + CSCD/NaCGEC 基准源句）
——评测句入表会让 MuCGEC 漏斗评测虚高，必须剔除。

抽取规则与 server._ngram_oov_ratio 完全一致：去非中文、句内滑窗（3-gram / 2-gram）。

剪枝：trigram `count >= min_count` 后按频次取 top-N；bigram（加权 OOV 的
「两个组成 bigram 都高频 → OOV 权重 0.4」判定表）取 top-K。

输出（原子写，tmp + os.replace）：
- ngram_table.json        {tri: count}
- bigram_table_zh.json    [bg, ...]（zh-only 抽取，与 ngram 规则一致；
                          不复用 l3_bg/zh_bigrams.json——那份是原文含标点抽取，规则不同）

报告：clean holdout（3% 池内句，未入表）OOV 覆盖率 / confirmed 真实用户句 OOV /
MuCGEC gold src OOV（应保持高位 = 区分度）。

用法：
    python3 build_ngram_table.py                 # 默认参数
    python3 build_ngram_table.py --top 1500000   # 更大表
"""

from __future__ import annotations

import argparse
import gzip
import heapq
import json
import os
import random
import sys
import time
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO / "datasets"))

import cec_common as C  # noqa: E402  （detokenize，与 prepare_l3_pool 一致）
from prepare_l3_pool import load_blocklist  # noqa: E402  （去污染块表）

POOL = HERE / "l3_pool_raw"
# (文件, 是否按 --lang8 采样)
FILES = [
    ("train.txt", False),
    ("train_CGED.txt", False),
    ("train_FCGEC.txt", False),
    ("train.para", True),
]


def zh_chars(s: str) -> list[str]:
    return [c for c in s if "\u4e00" <= c <= "\u9fff"]


def count_ngrams(s: str, tri: Counter, bi: Counter) -> None:
    """与 server._ngram_oov_ratio 同规则：去非中文、句内滑窗。"""
    zc = zh_chars(s)
    for i in range(len(zc) - 2):
        tri[zc[i] + zc[i + 1] + zc[i + 2]] += 1
    for i in range(len(zc) - 1):
        bi[zc[i] + zc[i + 1]] += 1


def oov_ratio(s: str, table: dict, bg: set[str] | None = None,
              weighted: bool = False) -> float:
    zc = zh_chars(s)
    if len(zc) < 3:
        return 0.0
    total = 0
    w = 0.0
    for i in range(len(zc) - 2):
        tri = zc[i] + zc[i + 1] + zc[i + 2]
        total += 1
        if tri not in table:
            if weighted and bg is not None and (zc[i] + zc[i + 1]) in bg and (zc[i + 1] + zc[i + 2]) in bg:
                w += 0.4
            else:
                w += 1.0
    return w / total if total else 0.0


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--top", type=int, default=1_000_000, help="trigram 保留 top-N")
    ap.add_argument("--min-count", type=int, default=2, help="trigram 最小频次")
    ap.add_argument("--bigram-top", type=int, default=300_000)
    ap.add_argument("--bigram-min-count", type=int, default=5)
    ap.add_argument("--lang8", type=int, default=600_000, help="train.para 采样行数（0=全量）")
    ap.add_argument("--holdout-p", type=float, default=0.03, help="holdout 句比例（不入表）")
    ap.add_argument("--extra-clean", default="",
                    help="额外 clean 语料（.jsonl / .jsonl.gz / .txt）：JSON 取 target（回退 "
                         "text/ref/src 之外的干净侧），纯文本取整行；只入干净侧，不产生错误侧")
    ap.add_argument("--extra-clean-max", type=int, default=0, help="额外语料最多取多少句（0=全量）")
    ap.add_argument("--seed", type=int, default=20261018)
    args = ap.parse_args()

    t0 = time.time()
    rng = random.Random(args.seed)
    block = load_blocklist()
    print(f"去污染块表: {len(block)} 句")

    tri: Counter = Counter()
    bi: Counter = Counter()
    holdout: list[str] = []
    n_pairs = n_blocked = n_short = 0

    for fname, sampleable in FILES:
        path = POOL / fname
        if not path.is_file():
            print(f"!! 缺文件 {path}（先跑 prepare_l3_pool.py 下载）", file=sys.stderr)
            sys.exit(1)
        p = 1.0
        if sampleable and args.lang8:
            total_lines = sum(1 for _ in open(path, encoding="utf-8"))
            p = min(1.0, args.lang8 / total_lines)
        kept = 0
        with open(path, encoding="utf-8") as f:
            for ln in f:
                if p < 1.0 and rng.random() > p:
                    continue
                parts = ln.rstrip("\n").split("\t")
                if len(parts) < 2:
                    continue
                ref = C.detokenize(parts[1])
                if ref in block:
                    n_blocked += 1
                    continue
                if len(zh_chars(ref)) < 3:
                    n_short += 1
                    continue
                if rng.random() < args.holdout_p:
                    if len(holdout) < 20000:
                        holdout.append(ref)
                    continue
                count_ngrams(ref, tri, bi)
                n_pairs += 1
                kept += 1
        print(f"  {fname}: 入表 {kept} 句（采样率 {p:.2f}）")

    # ── 额外 clean 语料（只取干净侧；例：midu2022 初赛伪数据的 target 侧）──
    # 该语料 100% 带注入错误（S/D/I=45/32/23，单字替换同音仅 24.5%），错误侧不入表；
    # 干净侧是 96 万句去重的真实场景长文（p50 44 字），与现有池零重叠 → 补原生域覆盖。
    extra_holdout: list[str] = []
    n_extra = n_extra_dup = n_extra_ph = 0
    if args.extra_clean:
        xpath = Path(args.extra_clean).expanduser()
        if not xpath.is_file():
            print(f"!! 缺文件 {xpath}", file=sys.stderr)
            sys.exit(1)
        opener = (lambda p: gzip.open(p, "rt", encoding="utf-8")) \
            if xpath.suffix == ".gz" else (lambda p: open(p, encoding="utf-8"))
        seen_extra: set[str] = set()
        with opener(xpath) as f:
            for ln in f:
                if args.extra_clean_max and n_extra >= args.extra_clean_max:
                    break
                ln = ln.strip()
                if not ln:
                    continue
                if ln.startswith("{"):
                    try:
                        row = json.loads(ln)
                    except json.JSONDecodeError:
                        continue
                    t = str(row.get("target") or row.get("ref") or row.get("text") or "")
                else:
                    t = ln.split("\t")[-1]
                t = C.detokenize(t.strip())
                if "ued" in t:            # midu 脱敏占位符（uedaa / ued73 …）
                    n_extra_ph += 1
                    continue
                if t in block:
                    n_blocked += 1
                    continue
                if len(zh_chars(t)) < 3:
                    n_short += 1
                    continue
                if t in seen_extra:
                    n_extra_dup += 1
                    continue
                seen_extra.add(t)
                if rng.random() < args.holdout_p:
                    if len(extra_holdout) < 20000:
                        extra_holdout.append(t)
                    continue
                count_ngrams(t, tri, bi)
                n_extra += 1
                n_pairs += 1
        print(f"  extra-clean {xpath.name}: 入表 {n_extra} 句"
              f"（去重剔除 {n_extra_dup}，占位符 {n_extra_ph}，holdout {len(extra_holdout)}）")

    # confirmed.jsonl clean 句：单独收集（先测覆盖率，再入表）
    confirmed: list[str] = []
    conf_path = HERE / "l3_data" / "zh" / "confirmed.jsonl"
    if conf_path.is_file():
        with open(conf_path, encoding="utf-8") as f:
            for ln in f:
                ln = ln.strip()
                if not ln:
                    continue
                try:
                    row = json.loads(ln)
                except json.JSONDecodeError:
                    continue
                if int(row.get("label", 1)) == 0:
                    t = str(row.get("text", ""))
                    if len(zh_chars(t)) >= 3 and t not in block:
                        confirmed.append(t)
    print(f"池内 clean 句: {n_pairs}（块表剔除 {n_blocked}，短句 {n_short}）"
          f"；holdout {len(holdout)}；confirmed clean {len(confirmed)}")
    print(f"unique trigram: {len(tri)}  unique bigram: {len(bi)}")

    # ── 覆盖率测量（min_count 过滤前的全量计数，作为表覆盖上界参考） ──
    bg_all = set(bi)

    def report(name: str, sents: list[str]) -> None:
        if not sents:
            return
        rs = [oov_ratio(s, tri) for s in sents]
        rs2 = [oov_ratio(s, tri, bg_all, weighted=True) for s in sents]
        n = len(rs)
        print(f"  [{name}] n={n} 平均OOV率(未加权)={sum(rs)/n:.3f} "
              f"(加权)={sum(rs2)/n:.3f} "
              f">10%: {sum(1 for r in rs if r > 0.10)/n:.1%} "
              f">30%: {sum(1 for r in rs if r > 0.30)/n:.1%}")

    gold_sents: list[str] = []
    gold_path = REPO / "datasets" / "mucgec_gold.json"
    if gold_path.is_file():
        gold_sents = [e["src"] for e in json.load(open(gold_path, encoding="utf-8"))]
    print("覆盖率（全量计数口径）:")
    report("clean holdout", holdout[:5000])
    report("extra-clean holdout", extra_holdout[:5000])
    report("confirmed 真实用户句", confirmed)
    report("MuCGEC gold src（应高）", gold_sents[:5000])

    # ── 剪枝 ──
    filt = [(k, v) for k, v in tri.items() if v >= args.min_count]
    print(f"min_count>={args.min_count}: {len(filt)} trigram")
    top = heapq.nlargest(args.top, filt, key=lambda kv: kv[1])
    table = dict(top)
    if top:
        print(f"top-{args.top}: {len(table)} trigram，最低频次 {top[-1][1]}")

    # confirmed 句补入（用户确认 clean → 直接信任，count=1）；其 bigram 也计入
    for s in confirmed:
        count_ngrams(s, tri, bi)
    added = 0
    for s in confirmed:
        zc = zh_chars(s)
        for i in range(len(zc) - 2):
            t3 = zc[i] + zc[i + 1] + zc[i + 2]
            if t3 not in table:
                table[t3] = 1
                added += 1
    print(f"confirmed 补入 trigram: {added}")

    btop = [k for k, v in bi.most_common() if v >= args.bigram_min_count][: args.bigram_top]

    # ── 原子写 ──
    def atomic_write(path: Path, obj) -> None:
        tmp = Path(str(path) + ".tmp")
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(obj, f, ensure_ascii=False)
        os.replace(tmp, path)

    tri_path = Path(os.environ.get("L3_NGRAM_OUT", str(HERE / "ngram_table.json")))
    bg_path = Path(os.environ.get("L3_BIGRAM_OUT", str(HERE / "bigram_table_zh.json")))
    atomic_write(tri_path, table)
    atomic_write(bg_path, btop)
    print(f"写出 {tri_path}: {len(table)} 条, {tri_path.stat().st_size/1e6:.1f} MB")
    print(f"写出 {bg_path}: {len(btop)} 条, {bg_path.stat().st_size/1e6:.1f} MB")

    # ── 入表后终检（最终表 + 最终 bigram，加权口径 + 短句平滑） ──
    bgset = set(btop)

    def final_oov(s: str) -> float:
        zc = zh_chars(s)
        if len(zc) < 3:
            return 0.0
        total = 0
        w = 0.0
        for i in range(len(zc) - 2):
            t3 = zc[i] + zc[i + 1] + zc[i + 2]
            total += 1
            if t3 not in table:
                w += 0.4 if (zc[i] + zc[i + 1]) in bgset and (zc[i + 1] + zc[i + 2]) in bgset else 1.0
        return w / (total + 2.0)

    print("终检（最终表，加权+平滑）:")
    for name, sents in (("clean holdout", holdout[:5000]),
                        ("confirmed 真实用户句", confirmed),
                        ("MuCGEC gold src（应高）", gold_sents[:5000])):
        if not sents:
            continue
        rs = [final_oov(s) for s in sents]
        n = len(rs)
        print(f"  [{name}] n={n} 加权OOV均值={sum(rs)/n:.3f} "
              f">10%: {sum(1 for r in rs if r > 0.10)/n:.1%} "
              f">30%: {sum(1 for r in rs if r > 0.30)/n:.1%}")
    print(f"耗时 {time.time()-t0:.1f}s")


if __name__ == "__main__":
    main()
