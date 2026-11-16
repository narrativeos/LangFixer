#!/usr/bin/env python3
"""v2 级 true gold 合并器：共识 ∪ 已采纳模型新增 ∪ 金集真错 − 金集噪声。

输入（缺省即按 rollout 落盘位置，裁决 journal 为空时仍出「按桶默认」版）：
- ~/tmp/dataset_v22/*_v22.jsonl      标注（逐条口径：编辑字符 ∩ 行 gold 字符集）
- ~/tmp/dataset_v22/adjudications.jsonl  模型新增裁决（adopt/reject，可缺）
- ~/tmp/dataset_v21/*_v21.jsonl      仅为回退条目回填 fix/type
- ~/tmp/grey_zone/gold_grey_chars.jsonl  灰区分区（回退/问了被否/门控未见）
- ~/tmp/gold_adjudicate/gold_verdicts_journal.jsonl  金集侧裁决（可缺）

输出 ~/tmp/true_gold/：
- true_gold.jsonl  action=label 的行；unit=edit 带 a/b/fix/type，unit=char 是灰区字符。
- excluded.jsonl   action=noise（金集噪声候选）与 reject（模型新增被否）。
- rule_candidates.jsonl  门控未见 + 手动 mark → 规则引擎素材。
- stats.json       统一字符口径的覆盖。

分区→默认判（与 gold-adjudicate 页一致，已验证 manual 桶为空）：
回退→real，问了被否→noise，门控未见→mark；journal 有裁决则以裁决为准。
逐条口径守则：不信任何行级计数字段，一切现算。
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

PART_DEFAULT = {"回退": ("real", "gold-real-default"),
                "问了被否": ("noise", "gold-noise-default"),
                "门控未见": ("mark", "gate-unseen-mark")}


def load_rows(d: Path, ver: str) -> dict:
    rows = {}
    for fp in sorted(d.glob(f"*_{ver}.jsonl")):
        if "review_queue" in fp.name:
            continue
        with open(fp, encoding="utf-8") as f:
            for line in f:
                r = json.loads(line)
                rows[r["id"]] = r
    if not rows:
        sys.exit(f"{d} 下没有 {ver} 标注文件")
    return rows


def load_journal(p: Path) -> dict:
    v = {}
    if p.exists():
        with open(p, encoding="utf-8") as f:
            for line in f:
                rec = json.loads(line)
                v[rec["iid"]] = rec["verdict"]      # 后写覆盖前写
    return v


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--v22-dir", default="~/tmp/dataset_v22")
    ap.add_argument("--v21-dir", default="~/tmp/dataset_v21")
    ap.add_argument("--grey", default="~/tmp/grey_zone/gold_grey_chars.jsonl")
    ap.add_argument("--model-state", default=None, help="模型侧裁决目录（默认 = v22-dir）")
    ap.add_argument("--gold-state", default="~/tmp/gold_adjudicate")
    ap.add_argument("--out", default="~/tmp/true_gold")
    a = ap.parse_args()
    v22d = Path(a.v22_dir).expanduser()
    out = Path(a.out).expanduser(); out.mkdir(parents=True, exist_ok=True)
    v22 = load_rows(v22d, "v22")
    v21 = load_rows(Path(a.v21_dir).expanduser(), "v21")
    model_v = load_journal(Path(a.model_state or a.v22_dir).expanduser() / "adjudications.jsonl")
    gold_v = load_journal(Path(a.gold_state).expanduser() / "gold_verdicts_journal.jsonl")

    gold, excl, rules = [], [], []
    st = {"consensus_edits": 0, "model_new_pending": 0, "model_new_adopted": 0,
          "model_new_rejected": 0, "gold_real": 0, "noise": 0, "rule": 0,
          "gold_chars": 0, "labeled_chars": 0, "gold_labeled_chars": 0, "type_dist": {}}
    lab_chars, lab_gold = set(), set()
    for rid, r in v22.items():
        src, gset = r["src"], set()
        for g in r["gold_edits"]:
            gset.update(range(g["a"], max(g["b"], g["a"] + 1)))
        st["gold_chars"] += len(gset)
        for e in r["model_errors"]:
            es = set(range(e["a"], e["b"]))
            iid = f"{rid}#{e['a']}#{e['b']}"
            if es & gset:                       # 逐条口径：命中 gold = 共识
                st["consensus_edits"] += 1
                gold.append({"id": rid, "unit": "edit", "a": e["a"], "b": e["b"],
                             "fix": e.get("fix", ""), "type": e.get("type", ""),
                             "action": "label", "source": "consensus"})
                lab_chars.update((rid, c) for c in es)
                lab_gold.update((rid, c) for c in es & gset)
            else:
                verdict = model_v.get(iid)
                if verdict == "adopt":
                    st["model_new_adopted"] += 1
                    gold.append({"id": rid, "unit": "edit", "a": e["a"], "b": e["b"],
                                 "fix": e.get("fix", ""), "type": e.get("type", ""),
                                 "action": "label", "source": "model-adopted"})
                    lab_chars.update((rid, c) for c in es)  # 新增在 gold 字符之外
                elif verdict == "reject":
                    st["model_new_rejected"] += 1
                    excl.append({"iid": iid, "action": "reject", "source": "model-new"})
                else:
                    st["model_new_pending"] += 1
        for e in r["model_errors"]:             # 类型分布按共识/采纳编辑
            if (set(range(e["a"], e["b"])) & gset
                    or model_v.get(f"{rid}#{e['a']}#{e['b']}") == "adopt"):
                t = e.get("type", "") or "unknown"
                st["type_dist"][t] = st["type_dist"].get(t, 0) + 1

    with open(Path(a.grey).expanduser(), encoding="utf-8") as f:
        for line in f:
            x = json.loads(line)
            x["iid"] = f"{x['id']}#{x['pos']}"   # 与 gold-adjudicate 页同构
            verdict, src_tag = PART_DEFAULT[x["partition"]]
            if x["iid"] in gold_v:
                verdict, src_tag = gold_v[x["iid"]], "gold-adjudicated"
            g = x["gold"]
            if verdict == "real":               # 回退：从 v21 重叠 edit 回填 fix/type
                fx, ty = "", ""
                if x["id"] in v21:
                    for e21 in v21[x["id"]]["model_errors"]:
                        if e21["a"] <= x["pos"] < e21["b"]:
                            fx, ty = e21.get("fix", ""), e21.get("type", "")
                            break
                gold.append({"id": x["id"], "unit": "char", "pos": x["pos"],
                             "fix": fx if fx else g["ref"], "type": ty or "gold-unknown",
                             "gold": g, "action": "label", "source": src_tag})
                st["gold_real"] += 1
                lab_chars.add((x["id"], x["pos"]))
                lab_gold.add((x["id"], x["pos"]))
            elif verdict == "noise":
                excl.append({"iid": x["iid"], "id": x["id"], "pos": x["pos"], "gold": g,
                             "action": "noise", "source": src_tag})
                st["noise"] += 1
            else:                               # mark → 规则通道
                rules.append({"id": x["id"], "pos": x["pos"], "set": x["set"], "char": x["char"],
                              "ctx": x["ctx"], "gold": g, "action": "rule-candidate"})
                st["rule"] += 1
    st["labeled_chars"] = len(lab_chars)
    st["gold_labeled_chars"] = len(lab_gold)
    for name, rows in (("true_gold.jsonl", gold), ("excluded.jsonl", excl),
                       ("rule_candidates.jsonl", rules)):
        with open(out / name, "w", encoding="utf-8") as f:
            for y in rows:
                f.write(json.dumps(y, ensure_ascii=False) + "\n")
    st["coverage"] = st["gold_labeled_chars"] / st["gold_chars"] if st["gold_chars"] else 0
    json.dump(st, open(out / "stats.json", "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"标签行 {len(gold):,}（共识 {st['consensus_edits']:,} + 采纳 {st['model_new_adopted']:,}"
          f" + 金集真错 {st['gold_real']:,}）")
    print(f"gold 字符覆盖 {st['gold_labeled_chars']:,}/{st['gold_chars']:,} = {st['coverage']:.1%}"
          f"（另标 gold 外字符 {st['labeled_chars'] - st['gold_labeled_chars']:,}）")
    print(f"待抽检模型新增 {st['model_new_pending']:,} · 金集噪声 {st['noise']:,}"
          f" · 规则候选 {st['rule']:,}")
    print("类型分布:", json.dumps(st["type_dist"], ensure_ascii=False))


if __name__ == "__main__":
    main()
