#!/usr/bin/env python3
"""Fβ 计分器（ChERRANT 字级口径）：LLM / L3 路径在 CSCD / NaCGEC / MuCGEC
gold 上的统一评测。

输入为 gold 格式 JSON（`make_gold_zh.py` / `prepare_cscd.py` / `prepare_nacgec.py`
产出：`[{split, src, edits: [{op, src, ref, span, ...}]}]`）：
- `--gold`：gold 编辑
- `--pred`：预测编辑（同构；句级对齐按 `src` 原文匹配）
- `--beta`：Fβ 的 β（默认 0.5 → F0.5，CEC 的 NaCGEC 口径；
  CSCD 的 Correction F1 用 `--beta 1`）

计分口径（字级，与 ChERRANT 一致）：
- 一条预测编辑为 TP ⟺ 存在 gold 编辑与其 **span 相同且替换内容相同**
  （op 相同；插入为插入点零宽 span）
- Precision = TP/预测数，Recall = TP/gold 数，Fβ = (1+β²)PR / (β²P + R)
- 另报**句级门控**（该句有无编辑）的 TP/FP/FN/TN 与 F1——
  对应 ELECTRA 字级门控的句级指标（CEC README_ELECTRA 口径）

用法：
  ./.venv/bin/python score_f05.py --gold cscd_gold.json --pred pred.json --beta 1
  ./.venv/bin/python score_f05.py --gold nacgec_gold.json --pred pred.json --beta 0.5
  ./.venv/bin/python score_f05.py --selftest
"""
import argparse
import json
import os


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def edit_key(e):
    return (e["op"], e.get("ref", ""), tuple(e["span"]))


def fb_score(tp, n_pred, n_gold, beta):
    b2 = beta * beta
    p = tp / n_pred if n_pred else 0.0
    r = tp / n_gold if n_gold else 0.0
    f = (1 + b2) * p * r / (b2 * p + r) if (b2 * p + r) else 0.0
    return p, r, f


def score(gold, pred, beta, verbose=False):
    """按 split 统计编辑级 P/R/Fβ 与句级门控。"""
    gold_by_src = {}
    for item in gold:
        gold_by_src.setdefault(item["src"], []).append(item)

    per_split = {}
    gate = {}  # split -> [tp, fp, fn, tn]
    n_unmatched = 0
    counted_srcs = set()  # 已计过 gold 数的 src（同一 src 的重复预测不重复累加）

    for item in pred:
        split = item.get("split", "?")
        stats = per_split.setdefault(split, [0, 0, 0])  # tp, n_pred, n_gold
        g_items = gold_by_src.get(item["src"])
        if g_items is None:
            n_unmatched += 1
            g_items = []
        g_keys = [edit_key(e) for g in g_items for e in g["edits"]]
        stats[1] += len(item["edits"])
        if item["src"] not in counted_srcs:
            counted_srcs.add(item["src"])
            stats[2] += len(g_keys)
        for e in item["edits"]:
            if edit_key(e) in g_keys:
                stats[0] += 1
        g_has = bool(g_keys)
        p_has = bool(item["edits"])
        g = gate.setdefault(split, [0, 0, 0, 0])
        if g_has and p_has:
            g[0] += 1
        elif p_has:
            g[1] += 1
            if verbose:
                print(f"  [FP句 {split}] {item['src'][:40]}")
        elif g_has:
            g[2] += 1
            if verbose:
                print(f"  [FN句 {split}] {item['src'][:40]}")
        else:
            g[3] += 1

    # gold 中有、pred 未覆盖的句子（按 src 对齐缺失 → 全算 FN 句）；
    # 第一循环已计过 gold 数的 src 不再重复累加
    gold_edits_by_src = {}
    gold_has_by_src = {}
    gold_split_by_src = {}
    for item in gold:
        gold_edits_by_src[item["src"]] = gold_edits_by_src.get(item["src"], 0) + len(item["edits"])
        gold_has_by_src[item["src"]] = gold_has_by_src.get(item["src"], False) or bool(item["edits"])
        gold_split_by_src[item["src"]] = item.get("split", "?")
    for src in gold_edits_by_src:
        if src in counted_srcs:
            continue
        split = gold_split_by_src.get(src, "?")
        stats = per_split.setdefault(split, [0, 0, 0])
        stats[2] += gold_edits_by_src[src]
        g = gate.setdefault(split, [0, 0, 0, 0])
        if gold_has_by_src[src]:
            g[2] += 1

    print(f"\n=== F{beta:g} 计分（编辑级） ===")
    tot = [0, 0, 0]
    for split in sorted(per_split):
        tp, np_, ng = per_split[split]
        p, r, f = fb_score(tp, np_, ng, beta)
        tot[0] += tp
        tot[1] += np_
        tot[2] += ng
        print(f"[{split:<8}] TP {tp:<5} 预测 {np_:<5} gold {ng:<5} "
              f"P {p:.3f}  R {r:.3f}  F{beta:g} {f:.3f}")
    p, r, f = fb_score(tot[0], tot[1], tot[2], beta)
    print(f"[{'总体':<8}] TP {tot[0]:<5} 预测 {tot[1]:<5} gold {tot[2]:<5} "
          f"P {p:.3f}  R {r:.3f}  F{beta:g} {f:.3f}")

    print(f"\n=== 句级门控（该句有无编辑） ===")
    t = [0, 0, 0, 0]
    for split in sorted(gate):
        tp, fp, fn, tn = gate[split]
        for i in range(4):
            t[i] += gate[split][i]
        n = tp + fp + fn + tn
        prec = tp / (tp + fp) if tp + fp else 0.0
        rec = tp / (tp + fn) if tp + fn else 0.0
        f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
        print(f"[{split:<8}] 句 {n:<5} TP {tp:<5} FP {fp:<4} FN {fn:<4} TN {tn:<5} "
              f"门控F1 {f1:.3f}")
    prec = t[0] / (t[0] + t[1]) if t[0] + t[1] else 0.0
    rec = t[0] / (t[0] + t[2]) if t[0] + t[2] else 0.0
    f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
    print(f"[{'总体':<8}] 句 {sum(t):<5} TP {t[0]:<5} FP {t[1]:<4} FN {t[2]:<4} TN {t[3]:<5} "
          f"门控F1 {f1:.3f}")
    if n_unmatched:
        print(f"\n注意：{n_unmatched} 条预测句在 gold 中无对应 src（未计入）")


def selftest():
    """合成数据自检：验证 TP/FP/FN 与 Fβ 计算。"""
    import tempfile

    gold = [
        {"split": "t", "src": "他作天很冷", "edits": [
            {"op": "S", "src": "作", "ref": "做", "span": [6, 9]},
            {"op": "S", "src": "天", "ref": "天", "span": [9, 12]},
        ]},
        {"split": "t", "src": "无误句", "edits": []},
    ]
    pred = [
        {"split": "t", "src": "他作天很冷", "edits": [
            {"op": "S", "src": "作", "ref": "做", "span": [6, 9]},
            {"op": "S", "src": "作", "ref": "坐", "span": [6, 9]},
            {"op": "I", "src": "", "ref": "很", "span": [9, 9]},
        ]},
        {"split": "t", "src": "无误句", "edits": [
            {"op": "S", "src": "无", "ref": "有", "span": [0, 3]},
        ]},
    ]
    with tempfile.TemporaryDirectory() as d:
        gp, pp = os.path.join(d, "g.json"), os.path.join(d, "p.json")
        with open(gp, "w", encoding="utf-8") as f:
            json.dump(gold, f, ensure_ascii=False)
        with open(pp, "w", encoding="utf-8") as f:
            json.dump(pred, f, ensure_ascii=False)
        # gold: 2 编辑；pred: 3 编辑（TP=1）；句级：句1 TP、句2 FP
        score(gold, pred, beta=0.5)
    tp, np_, ng = 1, 4, 2
    p, r, f = fb_score(tp, np_, ng, 0.5)
    assert abs(p - 0.25) < 1e-9 and abs(r - 0.5) < 1e-9
    assert abs(f - (1.25 * 0.25 * 0.5) / (0.25 * 0.25 + 0.5)) < 1e-9
    print(f"\nselftest 通过（F0.5 = {f:.4f}）")


def main():
    ap = argparse.ArgumentParser(description="Fβ 计分器（ChERRANT 字级口径）")
    ap.add_argument("--gold", help="gold JSON（gold 格式）")
    ap.add_argument("--pred", help="预测 JSON（gold 格式，按 src 对齐）")
    ap.add_argument("--beta", type=float, default=0.5,
                    help="Fβ 的 β（NaCGEC 用 0.5；CSCD Correction F1 用 1）")
    ap.add_argument("--verbose", action="store_true", help="打印误报句明细")
    ap.add_argument("--selftest", action="store_true", help="合成数据自检")
    args = ap.parse_args()
    if args.selftest:
        selftest()
        return
    if not args.gold or not args.pred:
        ap.error("需要 --gold 与 --pred（或 --selftest）")
    score(load(args.gold), load(args.pred), args.beta, args.verbose)


if __name__ == "__main__":
    main()