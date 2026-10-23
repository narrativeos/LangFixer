#!/usr/bin/env python3
"""生成 NaCGEC 的 gold 编辑 JSON（datasets/nacgec_gold.json）：

从 [twnlp/cgc_data](https://huggingface.co/datasets/twnlp/cgc_data)（MIT）下载
`nacgec_all_type.json`（NaCGEC 评测基准，6369 条，每条含 2 个参考修订 +
`error_type` 语法错误类型标签），转成与 mucgec_gold.json 同构的 gold 格式
（多参考字级编辑去重 + 字节 span + cat/cat_rule；`error_type` 随句保留，
供分层分析——Rust 侧反序列化忽略该字段），供：
- `gec-zh/tests/cscd_eval.rs` 的 `nacgec_benchmark_report`（规则层召回/误报）
- `score_f05.py`（LLM/L3 路径 F0.5，ChERRANT 口径）

需要 pypinyin + opencc（datasets/.venv）。用法：`./.venv/bin/python prepare_nacgec.py`
"""
import json
import os

import cec_common as C

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "nacgec", "nacgec_all_type.json")
OUT = os.path.join(HERE, "nacgec_gold.json")


def main():
    path = C.download("twnlp/cgc_data", "nacgec_all_type.json", RAW)
    with open(path, encoding="utf-8") as f:
        raw = json.load(f)

    out = []
    n_edit = 0
    for entry in raw:
        src = entry["source"]
        refs = [r for r in entry.get("target", []) if r and r != src]
        seen = set()
        edits = []
        for ref in refs:
            for e in C.diff_edits(src, ref):
                key = (e["op"], e["src"], e["ref"], tuple(e["span"]))
                if key in seen:
                    continue
                seen.add(key)
                edits.append(e)
        edits.sort(key=lambda e: e["span"][0])
        out.append({
            "split": "nacgec",
            "src": src,
            "error_type": entry.get("error_type", ""),
            "edits": edits,
        })
        if edits:
            n_edit += 1

    with open(OUT, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False)

    from collections import Counter

    c = Counter(e["cat_rule"] for item in out for e in item["edits"])
    t = Counter(item["error_type"] for item in out if item["error_type"])
    print(f"→ {OUT}（{len(out)} 句，含编辑 {n_edit}）")
    for cat, n in c.most_common():
        print(f"  {cat}: {n}")
    print("  error_type 分布 Top 10:")
    for et, n in t.most_common(10):
        print(f"    {et}: {n}")


if __name__ == "__main__":
    main()