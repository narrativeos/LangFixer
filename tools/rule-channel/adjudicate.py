#!/usr/bin/env python3
"""规则通道运行时 CLI（约定②命中即问 + ③判断回写同一本 gold journal）。

两种用法：
1) 命中提问（给运行时/UI 消费，stdout 一行一问）：
   python3 adjudicate.py --check --id nacgec-123 --src "要文句子……"
   对 rule-candidates 索引里该行的位置逐一跑谓词，命中即出：
   {"iid","question":{"ctx","gold","model":"两轮均否"},"families":[...]}
2) 用户判断回写（与 gold-adjudicate 页同 iid 同格式，merge 重跑自动吸收）：
   python3 adjudicate.py --record nacgec-123#7 real --note "运行时命中用户判"

--candidates 默认 ~/tmp/true_gold/rule_candidates.jsonl，
--journal 默认 ~/tmp/gold_adjudicate/gold_verdicts_journal.jsonl。
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

from rules import FAMILIES, punct_convention_ok


def load_index(cand_path: Path):
    idx = {}
    if not cand_path.exists():
        sys.exit(f"未找到 {cand_path}（先跑 true-gold/merge.py）")
    for line in open(cand_path, encoding="utf-8"):
        x = json.loads(line)
        idx.setdefault(x["id"], []).append(x)
    return idx


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="对给定行文本跑规则，输出命中问题")
    ap.add_argument("--record", metavar="IID", help="回写判断：<id>#<pos>")
    ap.add_argument("verdict", nargs="?", choices=("real", "noise", "mark"),
                    help="--record 时的判断")
    ap.add_argument("--id", help="--check 的行 id")
    ap.add_argument("--src", help="--check 的行文本（- 读 stdin）")
    ap.add_argument("--note", default="运行时命中用户判")
    ap.add_argument("--candidates", default="~/tmp/true_gold/rule_candidates.jsonl")
    ap.add_argument("--journal", default="~/tmp/gold_adjudicate/gold_verdicts_journal.jsonl")
    a = ap.parse_args()

    if a.check:
        if not a.id or not a.src:
            sys.exit("--check 需要 --id 与 --src")
        src = sys.stdin.read() if a.src == "-" else a.src
        pth = Path(a.src) if a.src != "-" else None
        if pth and pth.exists():          # 接受文件路径或字面文本
            src = pth.read_text(encoding="utf-8")
        conv = punct_convention_ok(src)
        idx = load_index(Path(a.candidates).expanduser())
        for x in idx.get(a.id, []):
            ref = x["gold"].get("ref") or ""
            fams = [k for k, fn in FAMILIES.items()
                    if (not k.startswith("punct") or conv) and fn(src, x["pos"], ref)]
            if not fams:
                continue
            lo, hi = max(0, x["pos"] - 20), min(len(src), x["pos"] + 21)
            ctx = src[lo:x["pos"]] + "⟦" + (src[x["pos"]] if x["pos"] < len(src) else "") + "⟧" + src[x["pos"] + 1:hi]
            print(json.dumps({
                "iid": f"{x['id']}#{x['pos']}", "set": x["set"],
                "question": {"ctx": ctx,
                             "gold": {"op": x["gold"].get("op"), "ref": ref},
                             "model": "两轮提问均判无错"},
                "families": fams,
            }, ensure_ascii=False))
        return

    if a.record:
        if not a.verdict:
            sys.exit("--record 需要 verdict（real|noise|mark）")
        jrn = Path(a.journal).expanduser()
        jrn.parent.mkdir(parents=True, exist_ok=True)
        rec = {"iid": a.record, "verdict": a.verdict, "reason": a.note, "at": int(time.time())}
        with open(jrn, "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        print(f"已回写 {jrn}: {rec['iid']} → {rec['verdict']}")
        return

    ap.print_help()


if __name__ == "__main__":
    main()
