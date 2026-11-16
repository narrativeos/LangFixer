#!/usr/bin/env python3
"""ZEN 轨迹表引擎 v2（判定表 + ±6 窗口谓词；present=在场账，conflict=冲突判定）。

kind=present 行保持首版口径（补二实验2 可达性锚点，逐条输出字段不变）；
kind=conflict 行新增四谓词（table.json v2）：
  straddle_pair       窗口内同组异值代词、分居编辑位左右（他↔她/它、这↔那）
  num_same_unit_diff  窗口内数字对：同单位异数值（三年…二年 / 12次…13次）
  year_pair           窗口内双年份 token（数值不同）
  conj_pair_break     关联词前半在场、配对后半整句缺席（scope 偏离 ±6 的显式声明）

锚定模式：
  error-anchored = 编辑 span+fix 为锚（must 桶 / true_gold 金标位），窗口=src[a-6,b+6]+fix；
  standalone     = 干净语料里每个 anchor token 为锚，±6 找 partner；pair 按左锚归属去重。

三段校验（§11.14 补六）：
  ① must 桶 conflict 召回（752 条 consistency/cohesion；含 ±6 可达子集内）
  ② true_gold consistency/cohesion 编辑位锚定命中账
  ③ 干净语料打扰账：命中行占比 + 每万字命中（≤2% 闸门）

用法:
    python3 tools/zen-trajectory/trajectory.py [--queue ~/tmp/dataset_v22/review_queue_new_errors.jsonl]
        [--out ~/tmp/zen_traj/trajectory.jsonl] [--table tools/zen-trajectory/table.json]
        [--v22-glob ~/tmp/dataset_v22/*_v22.jsonl] [--true-gold ~/tmp/true_gold/true_gold.jsonl]
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
CC = ("consistency", "cohesion")


# ---------------------------------------------------------------- 谓词
def _fullmatch_group(tok: str, groups: list[str]) -> int | None:
    for gi, g in enumerate(groups):
        if re.fullmatch(g, tok):
            return gi
    return None


def pred_straddle(anchor, toks, groups, edit_at=None):
    """error-anchored: edit_at=编辑起点，pair 必须分居其左右；
    standalone: edit_at=None，pair 在锚的 ±窗口内任侧（由调用方按左锚去重）。"""
    gi = _fullmatch_group(anchor["tok"], groups)
    if gi is None:
        return []
    out = []
    for t in toks:
        if t is anchor or _fullmatch_group(t["tok"], groups) != gi:
            continue
        if t["tok"] == anchor["tok"]:
            continue
        if edit_at is None:
            out.append(t)
        else:
            l, r = (anchor, t) if anchor["a"] <= t["a"] else (t, anchor)
            if l["b"] <= edit_at < r["a"]:
                out.append(t)
    return out


def pred_num_pair(anchor, toks):
    """同单位异数值（tok 编码 num||unit）。"""
    out = []
    au, av = anchor["tok"].split("||")
    for t in toks:
        if t is anchor:
            continue
        tu, tv = t["tok"].split("||")
        if tu == au and tv != av:
            out.append(t)
    return out


def pred_year_pair(anchor, toks):
    out = []
    for t in toks:
        if t is anchor or t["tok"].split("||")[0] != anchor["tok"].split("||")[0]:
            out.append(t)
    return out


def pred_conj_break(anchor, sent, pairs):
    hit_strong = False
    for fe, partners in pairs:
        if re.fullmatch(fe, anchor["tok"]) and not re.search(partners, sent):
            if (fe, partners) not in WEAK_PAIRS:
                hit_strong = True
            return [True], hit_strong
    return [], False


# ---------------------------------------------------------------- token 化
def tokenize(kind, text, base=0):
    out = []
    if kind == "pron":
        for m in re.finditer("[他她它其这那]", text):
            out.append({"tok": m.group(0), "a": base + m.start(), "b": base + m.end()})
    elif kind == "num":
        for m in re.finditer(NUM_RE, text):
            out.append({"tok": m.group(1) + "||" + m.group(2),
                        "a": base + m.start(), "b": base + m.end()})
    elif kind == "year":
        for m in re.finditer(YEAR_RE, text):
            out.append({"tok": m.group(1) + "||年", "a": base + m.start(), "b": base + m.end()})
    elif kind == "conj":
        for fe, _ in PAIRS:
            for m in re.finditer(fe, text):
                out.append({"tok": m.group(0), "a": base + m.start(), "b": base + m.end()})
    return out


PRED_TOKEN = {"straddle_pair": "pron", "num_same_unit_diff": "num",
              "conj_pair_break": "conj", "year_pair": "year"}

# 由 load_table 注入（table.json 是唯一口径来源）
NUM_RE: str = ""
YEAR_RE: str = ""
PAIRS: list = []
WEAK_PAIRS: set = set()
GROUPS: dict = {}


def load_table(path):
    global NUM_RE, YEAR_RE, PAIRS, WEAK_PAIRS, GROUPS
    tab = json.load(open(path, encoding="utf-8"))
    present, conflict = [], []
    for r in tab["rows"]:
        if r.get("kind") == "present":
            present.append((r["id"], r["family"], re.compile(r["pattern"]),
                            r["emits"]["channel"], r["emits"]["check"]))
        else:
            r = dict(r)
            r["token"] = PRED_TOKEN[r["pred"]]
            r["channel"] = r["emits"]["channel"]
            r["check"] = r["emits"]["check"]
            if r["pred"] == "straddle_pair":
                GROUPS[r["id"]] = r["groups"]
            elif r["pred"] == "num_same_unit_diff":
                NUM_RE = r["num_re"]
            elif r["pred"] == "year_pair":
                YEAR_RE = r["year_re"]
            elif r["pred"] == "conj_pair_break":
                PAIRS = [tuple(x) for x in r["pairs"]]
                WEAK_PAIRS = {tuple(x) for x in r.get("weak_pairs", [])}
            conflict.append(r)
    return tab, present, conflict


def run_pred(row, anchor, toks, sent, edit_at=None):
    p = row["pred"]
    if p == "straddle_pair":
        return pred_straddle(anchor, toks, GROUPS[row["id"]], edit_at)
    if p == "num_same_unit_diff":
        return pred_num_pair(anchor, toks)
    if p == "year_pair":
        return pred_year_pair(anchor, toks)
    if p == "conj_pair_break":
        return pred_conj_break(anchor, sent, PAIRS)[0]
    return []


def sentence_of(src, a, b):
    l = max(src.rfind(x, 0, a) for x in "。！？\n") + 1
    r = min([x for x in [src.find(y, b) for y in "。！？\n"] if x >= 0] + [len(src)])
    return src[l:r]


def anchor_conflicts(conf_rows, src, a, b, fix, lo, hi):
    """error-anchored：窗口=src[a+lo,b+hi]+fix；pair 须分居编辑起点 a 左右
    （conj 例外取整句）。窗口外的 fix token 视作编辑右侧。"""
    w_lo, w_hi = max(0, a + lo), b + hi
    win = src[w_lo:w_hi] + fix
    sent = sentence_of(src, a, b) + fix
    toks = []
    for row in conf_rows:
        kind = row["token"]
        toks.append((row, tokenize(kind, src[w_lo:w_hi], w_lo)
                     + tokenize(kind, fix, w_hi)))
    fired = []
    for row, tk in toks:
        seen = set()
        for anc in tk:
            strong = False
            if row["pred"] == "conj_pair_break":
                hits, strong = pred_conj_break(anc, sent, PAIRS)
            else:
                hits = run_pred(row, anc, tk, sent, edit_at=a)
            for t in hits:
                pa, pb = ((anc, t) if anc["a"] <= t["a"] else (t, anc)) \
                    if isinstance(t, dict) else (anc, None)
                key = (pa["a"], pa["b"],
                       pb["a"] if pb else -1, pb["b"] if pb else -1)
                if key in seen:
                    continue
                seen.add(key)
                fired.append({"row": row["id"], "family": row["family"],
                              "anchor": anc["tok"],
                              "partner": t["tok"] if isinstance(t, dict) else "ABSENT",
                              "channel": row["channel"], "check": row["check"]})
                if strong:
                    fired.append({"row": row["id"] + "_strong",
                                  "family": row["family"],
                                  "anchor": anc["tok"], "partner": "ABSENT",
                                  "channel": row["channel"], "check": row["check"] + "（强配对义务）"})
    return fired


# ---------------------------------------------------------------- 主流程
def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--queue", default="~/tmp/dataset_v22/review_queue_new_errors.jsonl")
    ap.add_argument("--out", default="~/tmp/zen_traj/trajectory.jsonl")
    ap.add_argument("--table", default=str(HERE / "table.json"))
    ap.add_argument("--v22-glob", default="~/tmp/dataset_v22/*_v22.jsonl")
    ap.add_argument("--true-gold", default="~/tmp/true_gold/true_gold.jsonl")
    a = ap.parse_args()

    tab, present_rows, conf_rows = load_table(a.table)
    lo, hi = tab["meta"]["window"]

    id2src: dict = {}
    clean_rows: list = []
    gold_ids_cc: set = set()
    for line in open(os.path.expanduser(a.true_gold), encoding="utf-8"):
        g = json.loads(line)
        if g["type"] in CC:
            gold_ids_cc.add(g["id"])

    for fp in sorted(glob.glob(os.path.expanduser(a.v22_glob))):
        for line in open(fp, encoding="utf-8"):
            r = json.loads(line)
            id2src[r["id"]] = r["src"]
            me = r.get("model_errors")
            me = json.loads(me) if isinstance(me, str) else me
            if not any(e.get("type") in CC for e in (me or [])) and r["id"] not in gold_ids_cc:
                clean_rows.append((r["id"], r["src"]))

    # ---- ① must 桶（present 逐条口径不变 + conflicts 追加字段）
    out_p = Path(a.out).expanduser()
    out_p.parent.mkdir(parents=True, exist_ok=True)
    tot: Counter = Counter()
    reach: Counter = Counter()
    cfire: Counter = Counter()
    cfire_reach: Counter = Counter()
    fam_by_type: dict = {}
    n_out = 0
    with out_p.open("w", encoding="utf-8") as fo:
        for line in open(os.path.expanduser(a.queue), encoding="utf-8"):
            r = json.loads(line)
            if r["type"] not in CC:
                continue
            tot[r["type"]] += 1
            src, ed_a, ed_b, fix = r["src"], r["a"], r["b"], r["fix"]
            span = src[ed_a:ed_b] if ed_b > ed_a else fix
            win = src[max(0, ed_a + lo): ed_b + hi] + fix
            fires, span_fams = [], []
            for rid, fam, pat, ch, chk in present_rows:
                if pat.search(win):
                    fires.append({"row": rid, "family": fam, "channel": ch, "check": chk})
                    if pat.search(span):
                        span_fams.append(fam)
            conflicts = anchor_conflicts(conf_rows, src, ed_a, ed_b, fix, lo, hi)
            if fires:
                reach[r["type"]] += 1
                for f in fires:
                    fam_by_type.setdefault(r["type"], Counter())[f["family"]] += 1
            for pr in {cf["row"] for cf in conflicts}:
                cfire[pr] += 1
                if fires:
                    cfire_reach[pr] += 1
            fo.write(json.dumps({"id": r.get("id"), "src": src, "a": ed_a, "b": ed_b,
                                 "fix": fix, "type": r["type"], "span": span,
                                 "fires": fires, "span_visible": span_fams,
                                 "conflicts": conflicts}, ensure_ascii=False) + "\n")
            n_out += 1

    for t in CC:
        sv = sum(1 for l in open(out_p, encoding="utf-8")
                 if (r := json.loads(l))["type"] == t and r["span_visible"])
        print(f"{t} n={tot[t]} ±6窗口可达={reach[t]} ({reach[t]/max(1,tot[t]):.1%}) "
              f"span内可见={sv} ({sv/max(1,tot[t]):.1%}) "
              f"分族={dict(fam_by_type.get(t, {}))}")
    print(f"轨迹账 {n_out} 条 → {out_p}")

    n_must, n_reach = sum(tot.values()), sum(reach.values())
    show_rows = [r_["id"] for r_ in conf_rows] + \
        (["conj_break_strong"] if WEAK_PAIRS else [])
    print(f"\n[① must 桶 conflict 召回] n={n_must}（±6可达 {n_reach}）")
    for rid in show_rows:
        print(f"  {rid:<20} 命中行 {cfire[rid]:>4} ({cfire[rid]/n_must:.1%})"
              f"  可达子集内 {cfire_reach[rid]:>4} ({cfire_reach[rid]/max(1,n_reach):.1%})")

    # ---- ② true_gold 锚定
    ganchor: Counter = Counter()
    gtotal = 0
    for line in open(os.path.expanduser(a.true_gold), encoding="utf-8"):
        g = json.loads(line)
        if g["type"] not in CC:
            continue
        src = id2src.get(g["id"])
        if not src:
            continue
        if g["unit"] == "edit":
            ea, eb, fix = g["a"], g["b"], g.get("fix", "")
        else:
            ea = eb = g["pos"]
            fix = g.get("fix", "")
        gtotal += 1
        for pr in {c["row"] for c in anchor_conflicts(conf_rows, src, ea, eb, fix, lo, hi)}:
            ganchor[pr] += 1
    print(f"\n[② true_gold 锚定] consistency/cohesion 编辑 n={gtotal}")
    for rid in show_rows:
        print(f"  {rid:<20} 命中行 {ganchor[rid]:>4} ({ganchor[rid]/max(1,gtotal):.1%})")

    # ---- ③ 打扰账（standalone：每 token 为锚，±6 内找 partner，conj 取整句；
    #         pair 按左锚归属去重——只有 a 更小的锚计数）
    dpair: Counter = Counter()
    drow: Counter = Counter()
    any_hit_rows = 0
    any_hit_rows_strong = 0
    tot_chars = 0
    for _rid, src in clean_rows:
        tot_chars += len(src)
        toks_by_pred = {r_["id"]: tokenize(r_["token"], src, 0) for r_ in conf_rows}
        hits_any = False
        hits_any_s = False
        for r_ in conf_rows:
            rid = r_["id"]
            tk = toks_by_pred[rid]
            if rid == "conj_break":  # 句 scope，不用 pair 扫描
                n = 0
                ns = 0
                for anc in tk:
                    hits, strong = pred_conj_break(anc, sentence_of(src, anc["a"], anc["b"]), PAIRS)
                    n += len(hits)
                    if strong:
                        ns += 1
                if ns:
                    dpair["conj_break_strong"] += ns
                    drow["conj_break_strong"] += 1
                    hits_any_s = True
            else:
                n = 0
                for i, anc in enumerate(tk):
                    for j, t in enumerate(tk):
                        if j <= i:
                            continue
                        if t["a"] > anc["b"] + abs(hi):
                            continue
                        if run_pred(r_, anc, [anc, t], src):
                            n += 1
            if n:
                dpair[rid] += n
                drow[rid] += 1
                hits_any = True
                if rid != "conj_break":
                    hits_any_s = True
        if hits_any:
            any_hit_rows += 1
        if hits_any_s:
            any_hit_rows_strong += 1
    print(f"\n[③ 打扰账] 干净语料 n={len(clean_rows)} 行, {tot_chars} 字")
    for rid in show_rows:
        print(f"  {rid:<20} 命中 {dpair[rid]:>6} ({dpair[rid]/max(1,tot_chars)*1e4:.2f}/万字)"
              f"  命中行 {drow[rid]:>5} ({drow[rid]/max(1,len(clean_rows)):.2%})")
    print(f"  任一谓词命中行占比（全量对） = {any_hit_rows}/{len(clean_rows)}"
          f" = {any_hit_rows/max(1,len(clean_rows)):.2%}  (闸门 ≤2%)")
    print(f"  任一谓词命中行占比（强配对） = {any_hit_rows_strong}/{len(clean_rows)}"
          f" = {any_hit_rows_strong/max(1,len(clean_rows)):.2%}")


if __name__ == "__main__":
    main()
