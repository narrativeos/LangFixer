#!/usr/bin/env python3
"""生成 CSCD / 泛化性基准的 gold 编辑 JSON（datasets/cscd_gold.json）：

从 [twnlp/csc_data](https://huggingface.co/datasets/twnlp/csc_data)（MIT）下载
4 个测试文件（字级切分或连续文本，`源句 \\t 纠正句`），转成与 mucgec_gold.json
同构的 gold 格式（字级编辑 + 字节 span + cat/cat_rule），供
`gec-zh/tests/cscd_eval.rs` 跑规则层召回/误报、`score_f05.py` 跑 LLM/L3 路径。

| 文件 | split | 句数 | 定位 |
|------|-------|------|------|
| test_odw.txt | cscd | 500 | CSCD（拼写主基准，Correction F1） |
| test_sig_15.txt | sig15 | 1100 | SIGHAN-2015（泛化性） |
| test_med.txt | med | 500 | EC-Med（泛化性，医疗） |
| test_law.txt | law | 500 | EC-LAW（泛化性，法律） |

需要 pypinyin + opencc（datasets/.venv）。用法：`./.venv/bin/python prepare_cscd.py`
"""
import json
import os

import cec_common as C

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "cscd")
OUT = os.path.join(HERE, "cscd_gold.json")

# 文件名 → split 名
SPLITS = {
    "test_odw.txt": "cscd",
    "test_sig_15.txt": "sig15",
    "test_med.txt": "med",
    "test_law.txt": "law",
}


def main():
    out = []
    for filename, split in SPLITS.items():
        path = C.download("twnlp/csc_data", filename, os.path.join(RAW, filename))
        n = n_edit = 0
        with open(path, encoding="utf-8") as f:
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) < 2:
                    continue
                src = C.detokenize(parts[0])
                ref = C.detokenize(parts[1])
                edits = C.diff_edits(src, ref) if src != ref else []
                out.append({"split": split, "src": src, "edits": edits})
                n += 1
                if edits:
                    n_edit += 1
        print(f"[{split}] {filename}: {n} 句（含编辑 {n_edit}）")

    with open(OUT, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False)

    from collections import Counter

    c = Counter(e["cat_rule"] for item in out for e in item["edits"])
    print(f"\n→ {OUT}（{len(out)} 句）")
    for cat, n in c.most_common():
        print(f"  {cat}: {n}")


if __name__ == "__main__":
    main()