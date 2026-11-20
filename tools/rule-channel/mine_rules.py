#!/usr/bin/env python3
"""规则族量化：对 rule-candidates 的召回 + 全语料点火率（打扰率）。

用法：python3 mine_rules.py [--candidates ~/tmp/true_gold/rule_candidates.jsonl]
      [--labels ~/tmp/dataset_v22] [--out ~/tmp/true_gold]

产出：
- rule_family_map.jsonl  每条 candidate 命中了哪些族（运行时提示索引）
- rules_report.md        族×（候选召回 / 语料点火 / 非候选打扰）表 + 去留结论
"""
from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path

from rules import FAMILIES, punct_convention_ok


def cand_args(x, src_by_id):
    src = src_by_id.get(x["id"], "")
    return src, x["pos"], x["gold"].get("ref") or ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidates", default="~/tmp/true_gold/rule_candidates.jsonl")
    ap.add_argument("--labels", default="~/tmp/dataset_v22")
    ap.add_argument("--true-gold", default="~/tmp/true_gold/true_gold.jsonl",
                    help="已标位置不计打扰（它们本来就带标签，不需问用户）")
    ap.add_argument("--out", default="~/tmp/true_gold")
    a = ap.parse_args()
    out = Path(a.out).expanduser()

    src_by_id, all_srcs = {}, []
    for fp in sorted(Path(a.labels).expanduser().glob("*_v22.jsonl")):
        if "review_queue" in fp.name:
            continue
        for line in open(fp, encoding="utf-8"):
            r = json.loads(line)
            src_by_id[r["id"]] = r["src"]
            all_srcs.append((r["id"], r["src"]))
    cands = [json.loads(l) for l in open(Path(a.candidates).expanduser(), encoding="utf-8")]
    cand_iids = {f"{x['id']}#{x['pos']}" for x in cands}
    cand_at = {}
    for x in cands:
        cand_at.setdefault(x["id"], set()).add(x["pos"])

    fam_cand = Counter()
    with open(out / "rule_family_map.jsonl", "w", encoding="utf-8") as f:
        for x in cands:
            src, pos, ref = cand_args(x, src_by_id)
            conv = punct_convention_ok(src)
            fams = [k for k, fn in FAMILIES.items()
                    if (not k.startswith("punct") or conv) and fn(src, pos, ref)]
            for k in fams:
                fam_cand[k] += 1
            f.write(json.dumps({"iid": f"{x['id']}#{x['pos']}", "families": fams},
                               ensure_ascii=False) + "\n")
    uncov = sum(1 for line in open(out / "rule_family_map.jsonl", encoding="utf-8")
                if not json.loads(line)["families"])

    fam_all = Counter()
    total_chars = 0
    labeled = set()
    tp = Path(a.true_gold).expanduser()
    if tp.exists():
        for line in open(tp, encoding="utf-8"):
            y = json.loads(line)
            if y["unit"] == "edit":
                labeled.update((y["id"], p) for p in range(y["a"], y["b"]))
            else:
                labeled.add((y["id"], y["pos"]))
    fam_labeled = Counter()
    for rid, src in all_srcs:
        total_chars += len(src)
        conv = punct_convention_ok(src)
        for pos in range(len(src)):
            for k, fn in FAMILIES.items():
                if k.startswith("punct") and not conv:
                    continue
                if fn(src, pos, ""):
                    fam_all[k] += 1
                    if (rid, pos) in labeled:
                        fam_labeled[k] += 1
    noise = {k: fam_all[k] - fam_cand[k] - fam_labeled[k] for k in FAMILIES}

    lines = ["# 规则族量化报告（rule-channel）", "",
             f"- 候选 {len(cands):,} 条 · 语料 {total_chars:,} 字符 · 规则族 {len(FAMILIES)}", "",
             "| 族 | 候选命中 | 召回 | 语料点火 | 已标免问 | 纯打扰/万字 | 去留 |", "|---|---|---|---|---|---|---|"]
    for k in FAMILIES:
        rec = fam_cand[k] / len(cands) if cands else 0
        nz = noise[k] / total_chars * 1e4
        verdict = "留（未来文本要点）" if nz < 50 else "暂缓"
        lines.append(f"| {k} | {fam_cand[k]:,} | {rec:.1%} | {fam_all[k]:,} | {fam_labeled[k]:,} | "
                     f"{nz:.2f} | {verdict} |")
    lines += ["", f"规则族完全未覆盖的候选：**{uncov:,}**（{uncov/len(cands):.1%}）。",
              "结论：本语料 mark 桶的主体是漏字/多字/搭配/语序类字符问题，静态规则",
              "够不着——规则通道定位调整为**未来文本的高敏感事实核查要点**",
              "（时间/地点/标点惯例），对本语料的回收只作副产物；",
              "mark 主体等下一轮 rollout 与知识源通道（Tolaria 路线）。"]
    (out / "rules_report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
