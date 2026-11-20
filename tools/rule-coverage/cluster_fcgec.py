#!/usr/bin/env python3
"""gold 真错按 FCGEC 七类聚类：grammar_novice 兜底的规则点覆盖缺口报告。

方案 A（2026-11-20 调研定案）：FCGEC（Findings of EMNLP 2022）给出母语者
病句的七类错误类型——语序不当(IWO)/搭配不当(IWC)/成分缺失(CM)/成分赘余(CR)/
结构混乱(SC)/不合逻辑(ILL)/语意不明(AM)。本项目归因面 grammar_novice 兜底
占错误归因 50%（rule-channel README），zh grammar 规则点现仅 2 条
（ZH_DE_DA_DE / ZH_MISSING_SUBJECT，且均停用）——本报告把 true gold 的
编辑级错误按编辑形状映射到七类，再对照 rule_defs 现状出缺口优先级，
作为补规则点的靶子清单（聚类是启发式形状判断，非人工标注，
SC/ILL/AM 三者形状不可分时并入 rewrite_review 待人工细分）。

形状 → 七类映射（编辑级，句内）：
- D（fix 为空）            → 成分赘余 CR（多字：短删=冗字，长删=冗述）
- I（span 为空）           → 成分缺失 CM（漏字/漏成分）
- R 单字替换               → 虚词功能字对（的/地/得、在/再…）单列 funcword 桶
                             （规则点 ZH_DE_DA_DE 直接对应）；其余 → 搭配不当 IWC
- R 双方为同字重组         → 语序不当 IWO；句内「删除+插入」互嵌对 也判 IWO
- R 一侧包含另一侧         → 结构混乱 SC（局部增删改写）
- R 其余短替换             → 搭配不当 IWC；长替换 → rewrite_review（SC/ILL/AM）
typo / punct / 衔接一致（cohesion、consistency）不属七类，单列供覆盖率分母。

数据源与 gold_types.py 同源：~/tmp/true_gold/true_gold.jsonl 编辑记录经
~/tmp/dataset_v22 行映射回句文本；rule_defs 读 langfixer.db（运行态开关为准）。

产出：~/tmp/rule_coverage/{clustered.jsonl,report.md}；stdout 摘要。
"""
from __future__ import annotations

import argparse
import json
import sqlite3
from collections import Counter, defaultdict
from pathlib import Path

FUNCWORD_PAIRS = {("的", "地"), ("地", "的"), ("的", "得"), ("得", "的"),
                  ("地", "得"), ("得", "地"), ("在", "再"), ("再", "在")}
BUCKET_ORDER = ["成分缺失 CM", "成分赘余 CR", "搭配不当 IWC", "语序不当 IWO",
                "结构混乱 SC", "虚词功能字 funcword", "句级改写待细分 rewrite_review",
                "不合逻辑/语意不明 待细分", "衔接一致 cohesion", "错别字 spelling",
                "标点 punctuation"]
BUCKET_RULES = {  # 桶 → 应命中的规则点（rule-channel 同族约定；coverage 查 DB 现状）
    "虚词功能字 funcword": ["ZH_DE_DA_DE"],
    "成分缺失 CM": ["ZH_MISSING_SUBJECT"],
    "错别字 spelling": ["ZH_CONFUSABLE", "LLM_SPELLING"],
    "标点 punctuation": ["ZH_PUNCTUATION", "LLM_PUNCTUATION"],
}
SEVEN = ("语序不当 IWO", "搭配不当 IWC", "成分缺失 CM", "成分赘余 CR",
         "结构混乱 SC")


def op_of(rec: dict) -> tuple[str, int, int, str]:
    """统一 edit/char 两种记录为 (op, a, b, fix)；char 走 gold.op，缺失按形状推。"""
    if rec.get("unit") == "char":
        g = rec.get("gold") or {}
        a, b, fix = g.get("a", rec["pos"]), g.get("b", rec["pos"]), rec.get("fix", "")
        op = g.get("op") or ("D" if not fix else ("I" if a == b else "R"))
        if op == "S":  # 金集多字替换操作→统一按替换处理
            op = "R"
    else:
        a, b, fix = rec["a"], rec["b"], rec.get("fix", "")
        op = "D" if not fix else ("I" if a >= b else "R")
    return op, a, b, fix


def classify(rec: dict, text: str, edits: list) -> tuple[str, str]:
    """返回 (bucket, how)。edits 为同句全部编辑，供语序互嵌对判断。"""
    op, a, b, fix = op_of(rec)
    typ = rec.get("type", "")
    if typ == "punct":
        return "标点 punctuation", "gold型=punct"
    if typ == "typo":
        return "错别字 spelling", "gold型=typo"
    if typ in ("cohesion", "consistency"):
        return "衔接一致 cohesion", f"gold型={typ}"
    deleted = text[a:b]
    if op == "D":
        return ("成分赘余 CR", "纯删除·冗字" if b - a <= 2 else "纯删除·冗述")
    if op == "I":
        return ("成分缺失 CM", "插入补词/成分" if len(fix) > 1 else "插入补字")
    if op == "R":
        if len(deleted) == len(fix) == 1:
            if (deleted, fix) in FUNCWORD_PAIRS:
                return "虚词功能字 funcword", f"功能字 {deleted}↔{fix}"
            return "搭配不当 IWC", "单字替换·用字"
        if len(deleted) >= 2 and sorted(deleted) == sorted(fix):
            return "语序不当 IWO", "同字重组"
        for other in edits:
            if other is rec:
                continue
            oop, oa, ob, ofix = op_of(other)
            odel = text[oa:ob]
            if op == "D" and oop == "I" and len(deleted) >= 2 and deleted in ofix:
                return "语序不当 IWO", "句内删除↔插入互嵌"
            if op == "I" and oop == "D" and len(fix) >= 2 and fix in odel:
                return "语序不当 IWO", "句内插入↔删除互嵌"
        if len(deleted) >= 2 and (deleted in fix or fix in deleted):
            return "结构混乱 SC", "局部增删改写"
        if max(len(deleted), len(fix)) <= 8:
            return "搭配不当 IWC", "短替换·用词"
        return "句级改写待细分 rewrite_review", "长替换（SC/ILL/AM 形状不可分）"
    return "句级改写待细分 rewrite_review", f"未识别 op={op}"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--true-gold", default="~/tmp/true_gold/true_gold.jsonl")
    ap.add_argument("--v22-dir", default="~/tmp/dataset_v22")
    ap.add_argument("--db", default=str(Path(__file__).resolve().parents[2] / "langfixer.db"))
    ap.add_argument("--out-dir", default="~/tmp/rule_coverage")
    a = ap.parse_args()
    out_p = Path(a.out_dir).expanduser()
    out_p.mkdir(parents=True, exist_ok=True)

    v22 = {}
    for fp in sorted(Path(a.v22_dir).expanduser().glob("*_v22.jsonl")):
        if "review_queue" in fp.name:
            continue
        for l in open(fp, encoding="utf-8"):
            r = json.loads(l)
            v22[r["id"]] = r["src"]

    recs_by_id = defaultdict(list)
    raw = []
    for l in Path(a.true_gold).expanduser().read_text(encoding="utf-8").splitlines():
        if not l.strip():
            continue
        g = json.loads(l)
        if not g.get("type") or g["type"] == "gold-unknown":
            continue
        raw.append(g)
        recs_by_id[g["id"]].append(g)

    rows, n_miss = [], 0
    for g in raw:
        text = v22.get(g["id"])
        if text is None:
            n_miss += 1
            continue
        bucket, how = classify(g, text, recs_by_id[g["id"]])
        op, sp_a, sp_b, fix = op_of(g)
        rows.append({"id": g["id"], "op": op, "span": [sp_a, sp_b], "fix": fix,
                     "gold_type": g["type"], "unit": g.get("unit", "edit"),
                     "bucket": bucket, "how": how,
                     "deleted": text[sp_a:sp_b], "src": text})

    with (out_p / "clustered.jsonl").open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")

    def dist(sel):
        c = Counter(r["bucket"] for r in sel)
        return {k: c[k] for k in BUCKET_ORDER if c[k]}

    grammar = [r for r in rows if r["gold_type"] == "grammar"]
    struct = [r for r in rows if r["gold_type"] not in ("typo", "punct")]
    d_all, d_gram, d_struct = dist(rows), dist(grammar), dist(struct)

    db = sqlite3.connect(a.db)
    rule_state = {rid: (en, cat) for rid, en, cat in
                  db.execute("select id, enabled, category from rule_defs where lang='zh'")}

    L = []
    L.append("# grammar_novice 规则点覆盖：gold 真错 FCGEC 七类聚类报告\n")
    L.append(f"- 数据：true_gold 编辑 {len(rows):,} 条（v22 行缺失 {n_miss}）；"
             f"grammar 桶 {len(grammar):,} 条，结构类 {len(struct):,} 条\n")
    L.append("| 桶 | 全量 | grammar | 结构类 | 现成规则点（DB 状态） |")
    L.append("|---|---:|---:|---:|---|")
    for b in BUCKET_ORDER:
        ids = BUCKET_RULES.get(b, [])
        cov = "、".join(f"{i}{'✓' if rule_state.get(i, (0,))[0] else '✗停用'}"
                         for i in ids) or "**无**"
        L.append(f"| {b} | {d_all.get(b, 0):,} | {d_gram.get(b, 0):,} | "
                 f"{d_struct.get(b, 0):,} | {cov} |")
    seven_n = sum(d_struct.get(b, 0) for b in SEVEN)
    L.append(f"\n七类内合计 {seven_n:,}（结构类 {len(struct):,} 的 "
             f"{seven_n / max(1, len(struct)):.0%}）；"
             f"现成规则点可触达桶："
             + "、".join(b for b in BUCKET_RULES if d_struct.get(b)) + "。\n")
    L.append("## 缺口优先级（按 grammar 桶条数）\n")
    gap = [(b, d_gram.get(b, 0)) for b in BUCKET_ORDER if not BUCKET_RULES.get(b)]
    for b, n in sorted(gap, key=lambda x: -x[1]):
        ex = [r for r in grammar if r["bucket"] == b][:2]
        L.append(f"- **{b}**：{n:,} 条")
        for r in ex:
            s, e = r["span"]
            frag = r["src"][max(0, s - 8):e + 8]
            L.append(f"  - {r['how']}｜…{frag}…｜删「{r['deleted']}」→「{r['fix']}」")
    L.append("\n## grammar 桶形状分布（how 细分）\n")
    how_c = Counter((r["bucket"], r["how"]) for r in grammar)
    for (b, h), n in how_c.most_common(20):
        L.append(f"- {b} · {h}：{n:,}")
    (out_p / "report.md").write_text("\n".join(L) + "\n", encoding="utf-8")

    print(f"聚类 {len(rows):,} 条（grammar {len(grammar):,}）→ {out_p}")
    print("\n".join(f"  {k:34s} 全量{v:>7,} | grammar{d_gram.get(k, 0):>7,}"
                    for k, v in d_all.items()))
    print(f"report → {out_p / 'report.md'}")


if __name__ == "__main__":
    main()
