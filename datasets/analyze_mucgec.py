#!/usr/bin/env python3
"""MuCGEC 分析：字级 diff（源句 vs 各参考修订）+ 编辑分类分布报告。

数据格式（`mucgec/MuCGEC_dev.txt`）：`id \\t 原句 \\t 参考1 \\t 参考2 ...`
- 参考列为「没有错误」表示该修订者认为句子无误（跳过）
- diff 粒度为字符（中文 GEC 评测以字级为准，见 ChERRANT）

用法：`python3 analyze_mucgec.py`（在 datasets/ 目录下）
"""
import os
from collections import Counter
from difflib import SequenceMatcher

HERE = os.path.dirname(os.path.abspath(__file__))
DEV = os.path.join(HERE, "mucgec", "MuCGEC_dev.txt")

PUNCT = set("，。！？；：、“”‘’《》〈〉（）【】—…·,.!?;:()[]{}-—'\"/\\_~@#")


def is_punct(s: str) -> bool:
    return bool(s) and all(c in PUNCT for c in s)


def classify(aw: str, bw: str) -> str:
    """细分类（cat）：punct / char-sub / word-sub / deletion / insertion"""
    if not aw:
        return "insertion"
    if not bw:
        return "deletion"
    if is_punct(aw) and is_punct(bw):
        return "punct"
    if len(aw) == 1 and len(bw) == 1:
        return "char-sub"
    return "word-sub"


def diff_spans(s: str, ref: str):
    """字符级 diff → [(op, aw, bw, char_span)]；char_span 为源句字符下标区间。"""
    edits = []
    sm = SequenceMatcher(None, list(s), list(ref), autojunk=False)
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            continue
        op = {"replace": "S", "delete": "D", "insert": "I"}[tag]
        if tag == "replace":
            edits.append(("S", s[i1:i2], ref[j1:j2], [i1, i2]))
        elif tag == "delete":
            edits.append(("D", s[i1:i2], "", [i1, i2]))
        else:
            p = i1 if i1 < len(s) else len(s)
            edits.append(("I", "", ref[j1:j2], [p, p]))
    return edits


def load_dev():
    items = []
    with open(DEV, encoding="utf-8") as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 2:
                continue
            _id, src = parts[0], parts[1]
            refs = [r for r in parts[2:] if r != "没有错误"]
            items.append((src, refs))
    return items


def main():
    items = load_dev()
    cat_c = Counter()
    pair_c = Counter()
    n_edit_sent = 0
    n_no_ref = 0
    for src, refs in items:
        if not refs:
            n_no_ref += 1
            continue
        seen = set()
        has_edit = False
        for ref in refs:
            for op, aw, bw, _span in diff_spans(src, ref):
                if (op, aw, bw) in seen:
                    continue
                seen.add((op, aw, bw))
                has_edit = True
                cat = classify(aw, bw)
                cat_c[cat] += 1
                if op == "S":
                    pair_c[f"{aw}→{bw}"] += 1
                elif op == "D":
                    pair_c[f"[删]{aw}"] += 1
                else:
                    pair_c[f"[增]{bw}"] += 1
        if has_edit:
            n_edit_sent += 1

    print(f"句子: {len(items)}（含编辑 {n_edit_sent}，全部参考均为「没有错误」{n_no_ref}）")
    print("\n=== 编辑分类分布（跨参考去重后） ===")
    for cat, n in cat_c.most_common():
        print(f"  {cat:<12} {n}")
    print("\n=== 高频编辑对 Top 40 ===")
    for pair, n in pair_c.most_common(40):
        print(f"  {pair}  x{n}")


if __name__ == "__main__":
    main()
