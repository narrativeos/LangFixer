#!/usr/bin/env python3
"""生产 accepted edits 的 I 型存量盘点（PLAN_v2 §6.6，臂 D 监督源取证）。

来源：root langfixer.db detect_history 里 corrected_text≠text 的行（用户真实采纳修订，
带原文坐标）。逐行 SequenceMatcher 对齐出净修订 → op：S(replace)/D(delete)/
I(单字插入=漏字位，a=b=原文坐标)/M(多字插入)。产 I 位 JSONL 供 jev-gec 池接入，
顺带去重、报 op 分布/虚词比/超长损失/与池&北极星零污染断言。

用法：python3 datasets/extract_prod_accepted.py --out /tmp/prod_accepted.jsonl
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sqlite3
import sys
from difflib import SequenceMatcher
from pathlib import Path

FUNC = set("的地得是在和与及或有了为等使将把被让向从对以之其此该个中上下一也都还要能会")
CJK = lambda c: "\u4e00" <= c <= "\u9fff"
norm = lambda s: re.sub(r"\s+", "", s)


def align_ops(src: str, dst: str) -> list[dict]:
    ops = []
    for tag, i1, i2, j1, j2 in SequenceMatcher(None, src, dst).get_opcodes():
        if tag == "equal":
            continue
        if tag == "insert":
            ref = dst[j1:j2]
            ops.append({"a": i1, "b": i1, "ref": ref,
                        "op": "I" if len(ref) == 1 else "M"})
        elif tag == "delete":
            ops.append({"a": i1, "b": i2, "ref": src[i1:i2], "op": "D"})
        else:
            ops.append({"a": i1, "b": i2, "ref": dst[j1:j2], "op": "S"})
    return ops


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default="langfixer.db")
    ap.add_argument("--out", default="/tmp/prod_accepted.jsonl")
    ap.add_argument("--pool", default="../jev-gec/data/pools/zh_v3")
    ap.add_argument("--gray", default="/tmp/gray380.jsonl")
    a = ap.parse_args()

    con = sqlite3.connect(a.db)
    rows = con.execute("select id, text, corrected_text from detect_history "
                       "where coalesce(corrected_text,'')<>'' order by id").fetchall()
    rows += [(10**9 + r[0], r[1], r[2]) for r in con.execute(
        "select rowid, original, correction from mistakes where coalesce(correction,'')<>''")]

    seen = {norm(t) for t in Path(a.pool, "train.jsonl").read_text(encoding="utf-8").split() or []}
    # 去重基线用整行 jsonl 解析（按 text 字段），避免空白分词假象
    seen = {norm(json.loads(l)["text"]) for l in Path(a.pool, "train.jsonl").read_text(encoding="utf-8").splitlines() if l.strip()}
    seen |= {norm(json.loads(l)["text"]) for l in Path(a.pool, "dev.jsonl").read_text(encoding="utf-8").splitlines() if l.strip()}
    gray = {norm(json.loads(l)["text"]) for l in Path(a.gray).read_text(encoding="utf-8").splitlines() if l.strip()}

    out, n_same, n_pool_hit, n_gray_hit, n_edit_rows = [], 0, 0, 0, 0
    opc: dict[str, int] = {}
    for rid, text, corrected in rows:
        if not text:
            continue
        if norm(text) in gray:
            n_gray_hit += 1
            continue
        if text == corrected or norm(text) == norm(corrected):
            n_same += 1
            continue
        n_edit_rows += 1
        if norm(text) in seen:
            n_pool_hit += 1
            continue
        ops = align_ops(text, corrected)
        for op in ops:
            opc[op["op"]] = opc.get(op["op"], 0) + 1
        i_ops = [o for o in ops if o["op"] == "I"]
        if not i_ops:
            continue
        key = hashlib.md5((norm(text) + json.dumps([[o["a"], o["ref"]] for o in i_ops],
                                                    ensure_ascii=False)).encode()).hexdigest()
        out.append({"key": key, "id": f"prod-{rid}", "text": text,
                    "corrected": corrected, "i_edits": i_ops,
                    "all_ops": [{k: o[k] for k in ("a", "b", "ref", "op")} for o in ops]})

    i_pos = [(r, o) for r in out for o in r["i_edits"]]
    long_loss = sum(1 for r, o in i_pos if len(r["text"]) > 126)
    uniq_keys = {r["key"] for r in out}
    f_share = sum(1 for _, o in i_pos if o["ref"] in FUNC) / max(len(i_pos), 1)
    top = sorted(__import__("collections").Counter(o["ref"] for _, o in i_pos).items(),
                 key=lambda kv: -kv[1])[:10]
    Path(a.out).write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in out) + "\n",
                           encoding="utf-8")
    print(f"[prod] 采纳行 {len(rows)-n_same}（其中原样采纳 {n_same}）· 修订行 {n_edit_rows}")
    print(f"[prod] op 分布 {opc}")
    print(f"[prod] 含 I 行 {len(out)}（去重 key {len(uniq_keys)}）· I 位 {len(i_pos)} · 虚词比 {f_share:.3f} · top10 {top}")
    print(f"[prod] 超长(>126字)句带 I 位 {long_loss} · 池重合剔除 {n_pool_hit} · gray380 命中 {n_gray_hit}")
    print(f"[prod] → {a.out}")


if __name__ == "__main__":
    main()
