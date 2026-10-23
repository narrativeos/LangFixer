#!/usr/bin/env python3
"""生成 MuCGEC gold 编辑 JSON（datasets/mucgec_gold.json）：
每条源句的各参考修订（「没有错误」跳过）的字级编辑（去重），含：
- span：编辑在源句中的字节区间（UTF-8；S/D 为源块区间，I 为插入点零宽 span）
- cat：analyze_mucgec 的细分类（punct / char-sub / word-sub / deletion / insertion）
- cat_rule：规则引擎可及性分类
  - punct     标点编辑（当前本地规则仅覆盖 ASCII→全角）
  - particle  的/地/得、在/再、象/像/向 等依赖语境的虚词对（POS/LLM 路径）
  - variant   繁体→简体（词表可 100% 安全命中）
  - confusable 同音/形近对（拼音相同；词表可命中，需人工筛选安全性）
  - other     其余（词义替换/增删/长块替换，本地规则不可达）
供 gec-zh/tests/mucgec_eval.rs 使用。

与 make_gold.py 的差异：去重键含 span（同一错字在句中出现多次时各记一条）。
需要 pypinyin（datasets/.venv）。用法：`./.venv/bin/python make_gold_zh.py`
"""
import json
import os

import analyze_mucgec as A
from pypinyin import lazy_pinyin

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "mucgec_gold.json")

# 依赖语境（POS/句法）才能判定的虚词对 → 本地词表不可达
PARTICLE_PAIRS = {
    ("的", "地"), ("的", "得"), ("地", "的"), ("地", "得"), ("得", "的"), ("得", "地"),
    ("在", "再"), ("再", "在"),
    ("象", "像"), ("像", "象"), ("向", "像"), ("像", "向"),
    ("即", "既"), ("既", "即"),
}

# 数据中出现的繁体字（→ 对应简体）；判定 variant 用
TRADITIONAL = set("時質別訪從輸嵗歡團為涼爲個們對問學說話語讀寫聽見進東車馬鳥魚鳥點")


def py(s: str):
    return [x for x in lazy_pinyin(s) if x]


def cat_rule(op: str, aw: str, bw: str, cat: str) -> str:
    if cat == "punct":
        return "punct"
    if op in ("D", "I"):
        return "other"
    if (aw, bw) in PARTICLE_PAIRS:
        return "particle"
    if len(aw) <= 4 and py(aw) == py(bw):
        if any(c in TRADITIONAL for c in aw) and not any(c in TRADITIONAL for c in bw):
            return "variant"
        return "confusable"
    return "other"


def char_span_to_bytes(s: str, i1: int, i2: int):
    return [len(s[:i1].encode("utf-8")), len(s[:i2].encode("utf-8"))]


def main():
    out = []
    for src, refs in A.load_dev():
        seen = set()
        edits = []
        for ref in refs:
            for op, aw, bw, cspan in A.diff_spans(src, ref):
                key = (op, aw, bw, tuple(cspan))
                if key in seen:
                    continue
                seen.add(key)
                cat = A.classify(aw, bw)
                edits.append({
                    "op": op,
                    "src": aw,
                    "ref": bw,
                    "span": char_span_to_bytes(src, cspan[0], cspan[1]),
                    "cat": cat,
                    "cat_rule": cat_rule(op, aw, bw, cat),
                })
        edits.sort(key=lambda e: e["span"][0])
        out.append({"split": "dev", "src": src, "edits": edits})

    with open(OUT, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False)

    from collections import Counter

    c = Counter(e["cat_rule"] for item in out for e in item["edits"])
    n_edit = sum(1 for item in out if item["edits"])
    print(f"句子: {len(out)}（含编辑 {n_edit}）→ {OUT}")
    for cat, n in c.most_common():
        print(f"  {cat}: {n}")


if __name__ == "__main__":
    main()
