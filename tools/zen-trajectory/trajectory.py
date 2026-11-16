#!/usr/bin/env python3
"""ZEN 轨迹表首版引擎（decision-tables + ±6 窗口谓词，§11.14 补二定案）。

形态=「轻量账本 + 判定表」：table.json 声明行（族 → 谓词 → 通道），本引擎只做
「谓词在编辑 span ±6 窗口内是否在场」的命中账，不做冲突判定（v2）。对 must 桶
（review_queue_new_errors.jsonl 的 consistency/cohesion）复算补二实验2 可达性
锚点（±6：consistency 52.3% / cohesion 42.4%），并落逐条轨迹账。

不可达一半如实缺席（留 LLM/语篇回归层）；打扰率 ≤2% 闸门要等生产接线后按
「ZEN 通道产出错误/全部产出错误」实测，本脚本不算（离线无分母）。

用法:
    python3 tools/zen-trajectory/trajectory.py [--queue ~/tmp/dataset_v22/review_queue_new_errors.jsonl]
        [--out ~/tmp/zen_traj/trajectory.jsonl] [--table tools/zen-trajectory/table.json]
"""
from __future__ import annotations

import argparse
import json
import os
import re
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--queue", default="~/tmp/dataset_v22/review_queue_new_errors.jsonl")
    ap.add_argument("--out", default="~/tmp/zen_traj/trajectory.jsonl")
    ap.add_argument("--table", default=str(HERE / "table.json"))
    a = ap.parse_args()

    tab = json.load(open(a.table, encoding="utf-8"))
    lo, hi = tab["meta"]["window"]
    rows = [(r["id"], r["family"], re.compile(r["pattern"]),
             r["emits"]["channel"], r["emits"]["check"]) for r in tab["rows"]]

    out_p = Path(a.out).expanduser()
    out_p.parent.mkdir(parents=True, exist_ok=True)
    tot: Counter = Counter()
    reach: Counter = Counter()
    fam_cnt: Counter = Counter()
    fam_by_type: dict = {}
    n_out = 0
    with out_p.open("w", encoding="utf-8") as fo:
        for line in open(os.path.expanduser(a.queue), encoding="utf-8"):
            r = json.loads(line)
            if r["type"] not in ("consistency", "cohesion"):
                continue
            tot[r["type"]] += 1
            src, ed_a, ed_b, fix = r["src"], r["a"], r["b"], r["fix"]
            span = src[ed_a:ed_b] if ed_b > ed_a else fix
            win = src[max(0, ed_a + lo): ed_b + hi] + fix
            fires = []
            span_fams = []
            for rid, fam, pat, ch, chk in rows:
                if pat.search(win):
                    fires.append({"row": rid, "family": fam,
                                  "channel": ch, "check": chk})
                    if pat.search(span):
                        span_fams.append(fam)
            if fires:
                reach[r["type"]] += 1
                for f in fires:
                    fam_cnt[f["family"]] += 1
                    fam_by_type.setdefault(r["type"], Counter())[f["family"]] += 1
            fo.write(json.dumps({"id": r.get("id"), "src": src, "a": ed_a, "b": ed_b,
                                 "fix": fix, "type": r["type"], "span": span,
                                 "fires": fires, "span_visible": span_fams},
                                ensure_ascii=False) + "\n")
            n_out += 1

    for t in ("consistency", "cohesion"):
        sv = sum(1 for l in open(out_p, encoding="utf-8")
                 if (r := json.loads(l))["type"] == t and r["span_visible"])
        print(f"{t} n={tot[t]} ±6窗口可达={reach[t]} ({reach[t]/max(1,tot[t]):.1%}) "
              f"span内可见={sv} ({sv/max(1,tot[t]):.1%}) "
              f"分族={dict(fam_by_type.get(t, {}))}")
    print(f"轨迹账 {n_out} 条 → {out_p}")


if __name__ == "__main__":
    main()
