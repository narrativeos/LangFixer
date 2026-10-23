#!/usr/bin/env python3
"""提取介词误用的短语级模式：(prev, wrong->right) 与 (wrong, next) 上下文计数"""
from collections import Counter

import analyze_jfleg as A

bigram = Counter()
trigram = Counter()
for split in ["dev", "test"]:
    src, refs = A.load(split)
    for i, s in enumerate(src):
        a = s.split()
        seen = set()
        for k in range(4):
            b = refs[k][i].split()
            for op, aw, bw in A.word_diff(a, b):
                if op != "S":
                    continue
                al = A.strip_punct(aw.lower())
                bl = A.strip_punct(bw.lower())
                if not (al in A.PREPS and bl in A.PREPS):
                    continue
                if (al, bl) in seen:
                    continue
                seen.add((al, bl))
                for idx, w in enumerate(a):
                    if A.strip_punct(w.lower()) == al:
                        prev = a[idx - 1].lower() if idx > 0 else "<S>"
                        nxt = a[idx + 1].lower() if idx + 1 < len(a) else "<E>"
                        bigram[(prev, al, bl)] += 1
                        trigram[(al, nxt, bl)] += 1
                        break

print("=== (prev, wrong -> right) top 40 ===")
for (prev, al, bl), n in bigram.most_common(40):
    print(f"{n:4d}  {prev} | {al} -> {bl}")
print()
print("=== (wrong, next -> right) top 40 ===")
for (al, nxt, bl), n in trigram.most_common(40):
    print(f"{n:4d}  {al} {nxt} | -> {bl}")
