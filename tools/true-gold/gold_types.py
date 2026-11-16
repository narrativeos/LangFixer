#!/usr/bin/env python3
"""gold 侧型标签：text → v2 型码集合（§11.14 补四处方——word_order 漏检在池侧可见）。

补四残余 gap：word_order 的 58% 漏检行 gold=1 但 LLM 缓存无检出，
wo_exempt 只触及「LLM 恰好检出纯 word_order」的可见 12 行。本脚本把
gold 侧型信息按句子文本 join 进池行，两条通道按句取并集：

1. datasets/*_gold.json 行级 error_type → NACGEC_TYPE / DATASET_DEFAULT_TYPE
   （与 eval_router_arms.py 同源同映射，不另起口径）；
2. ~/tmp/true_gold/true_gold.jsonl 编辑级 type（v2 六类 enum）经
   ~/tmp/dataset_v22 行映射回句子文本。六类 → v2 映射（代码级定案）：
       typo→spelling；grammar→grammar；punct→punctuation；
       cohesion→logic（衔接问题多表现为句间语义/逻辑断裂；semantic_dup
         亦曾考虑，取 logic 与收益矩阵 logic 行同侧）；
       consistency→reference（指代/前后一致性落 reference 侧）；
       fact→不映射（事实核查独立通道，不入 T 族路由空间；该行仍按
         gold=1 走 llm_review，不降级）；
       gold-unknown（回退字符无型回填）→ 跳过。

输出 jsonl {text, types:[v2 型码]}（仅型非空的句子），默认
~/tmp/true_gold/gold_types.jsonl。router_labels.py --gold-types 消费。
附带 join 覆盖报告：池 label=1 行命中率 + 纯 word_order 行数
（= 补四说「不可见」的那部分）+ true_gold 标签吸收数。
"""
from __future__ import annotations

import argparse
import ast
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent          # tools/true-gold → LangFixer
sys.path.insert(0, str(REPO / "feature-model"))

import router_labels as RL          # V2_PASSTHROUGH（§11.11 T 族）


def _consts_from_src(path: Path, names: tuple) -> dict:
    """eval_router_arms.py 只 import numpy 就重（venv 限定）；型映射常量用
    ast 静态抽取，保持与 eval 侧同源单一定义、不复制粘贴。"""
    tree = ast.parse(path.read_text(encoding="utf-8"))
    out = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and isinstance(node.value, ast.Dict):
            for tgt in node.targets:
                if isinstance(tgt, ast.Name) and tgt.id in names:
                    out[tgt.id] = ast.literal_eval(node.value)
    if set(out) != set(names):
        sys.exit(f"{path} 中未找到常量 {set(names) - set(out)}（改名需同步）")
    return out


_C = _consts_from_src(REPO / "feature-model" / "eval_router_arms.py",
                      ("NACGEC_TYPE", "DATASET_DEFAULT_TYPE"))
NACGEC_TYPE = _C["NACGEC_TYPE"]
DATASET_DEFAULT_TYPE = _C["DATASET_DEFAULT_TYPE"]

# 六类 enum → v2 型码（fact/gold-unknown 不在表内 = 有意不映射，见 docstring）
GOLD6_TO_V2 = {"typo": "spelling", "grammar": "grammar", "punct": "punctuation",
               "cohesion": "logic", "consistency": "reference"}

# 行级 error_type 数据源：datasets/ 下 list 形的 *_gold.json（dict 形跳过）
LIST_DS = ["cctc", "cscd", "cscdns", "ctc21", "jfleg", "midu", "mucgec",
           "nacgec", "nlpcc23", "sig13", "sig14"]


def add(text_map: dict, text: str, typ: str) -> None:
    if typ in RL.V2_PASSTHROUGH:
        text_map.setdefault(text, set()).add(typ)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--true-gold", default="~/tmp/true_gold/true_gold.jsonl")
    ap.add_argument("--v22-dir", default="~/tmp/dataset_v22")
    ap.add_argument("--pool-dir", default=str(HERE.parent.parent.parent / "jev-gec" / "data" / "pools" / "zh_v2"))
    ap.add_argument("--out", default="~/tmp/true_gold/gold_types.jsonl")
    a = ap.parse_args()
    out_p = Path(a.out).expanduser()
    out_p.parent.mkdir(parents=True, exist_ok=True)

    text_map: dict[str, set] = {}

    # 通道 1：datasets 行级 error_type（与 eval 侧 load_eval 同映射）
    ds_texts = set()
    for nm in LIST_DS:
        p = REPO / "datasets" / f"{nm}_gold.json"
        if not p.is_file():
            continue
        for e in json.load(open(p, encoding="utf-8")):
            if not isinstance(e, dict) or not e.get("edits"):
                continue          # 无编辑/「正确」行不产型
            ds_texts.add(e["src"])
            et = e.get("error_type")
            typ = NACGEC_TYPE.get(et, DATASET_DEFAULT_TYPE.get(nm, "untyped")) \
                if et else DATASET_DEFAULT_TYPE.get(nm, "untyped")
            add(text_map, e["src"], typ)
    n_ds = len(text_map)

    # 通道 2：true_gold 编辑级型（v22 行 id → src 文本）
    v22 = {}
    for fp in sorted(Path(a.v22_dir).expanduser().glob("*_v22.jsonl")):
        if "review_queue" in fp.name:
            continue
        for l in open(fp, encoding="utf-8"):
            r = json.loads(l)
            v22[r["id"]] = r["src"]
    n_absorb = n_skip_fact = n_skip_unknown = n_miss_row = 0
    for l in Path(a.true_gold).expanduser().read_text(encoding="utf-8").splitlines():
        if not l.strip():
            continue
        g = json.loads(l)
        t = g.get("type", "")
        if t == "fact":
            n_skip_fact += 1
            continue
        if not t or t == "gold-unknown":
            n_skip_unknown += 1
            continue
        text = v22.get(g["id"])
        if text is None:
            n_miss_row += 1
            continue
        add(text_map, text, GOLD6_TO_V2[t])
        n_absorb += 1

    with out_p.open("w", encoding="utf-8") as f:
        for text, ts in sorted(text_map.items()):
            f.write(json.dumps({"text": text, "types": sorted(ts)},
                               ensure_ascii=False) + "\n")

    # ── join 覆盖报告 ──
    print(f"gold_types → {out_p}：{len(text_map):,} 句有型"
          f"（datasets 行级通道 {n_ds:,}，true_gold 吸收 {n_absorb:,} 条编辑；"
          f"fact 不映射 {n_skip_fact:,}，gold-unknown 跳过 {n_skip_unknown:,}"
          f"{f'，v22 行缺失 {n_miss_row:,}' if n_miss_row else ''}）")
    pool_p = Path(a.pool_dir).expanduser() / "train.jsonl"
    if pool_p.is_file():
        n1 = n_join = n_wo_only = 0
        dist: dict[str, int] = {}
        for l in pool_p.read_text(encoding="utf-8").splitlines():
            r = json.loads(l)
            if int(r.get("label", 0)) != 1:
                continue
            n1 += 1
            ts = text_map.get(r["text"])
            if not ts:
                continue
            n_join += 1
            for t in ts:
                dist[t] = dist.get(t, 0) + 1
            if ts == {"word_order"}:
                n_wo_only += 1
        print(f"池 train label=1 {n1:,} 句：文本 join 命中 {n_join:,}"
              f"（{n_join/max(1,n1):.1%}），纯 word_order {n_wo_only} 句"
              f"（补四不可见部分的本体）")
        print("池侧型分布:", json.dumps(dict(sorted(dist.items(), key=lambda x: -x[1])),
                                        ensure_ascii=False))


if __name__ == "__main__":
    main()
