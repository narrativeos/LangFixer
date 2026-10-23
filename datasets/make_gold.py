#!/usr/bin/env python3
"""生成 JFLEG gold 编辑 JSON（datasets/jfleg_gold.json）：
每条源句的 4 参考词级编辑（去重），含：
- span：编辑在源句中的字节区间（S/D 为源词区间含尾随空白；I 为插入点零宽 span）
- cat：analyze_jfleg 的完整分类（用于看总体分布）
- cat_rule：规则引擎可及性分类（spelling=词表可命中 / article / agreement /
  tense / preposition / gerund_inf / other），用于统计规则召回
供 gec-en/tests/jfleg_eval.rs 使用。"""
import pathlib
import json
from difflib import SequenceMatcher

import analyze_jfleg as A

ROOT = pathlib.Path(__file__).resolve().parents[1]

ARTICLES = {"a", "an", "the"}


def word_spans(s):
    out = []
    pos = 0
    for w in s.split():
        start = pos
        pos += len(w)
        rest = s[pos:]
        skip = len(rest) - len(rest.lstrip())
        pos += skip
        out.append((w, [start, pos]))
    return out


def diff_spans(s, ref):
    a = s.split()
    b = ref.split()
    spans = word_spans(s)
    sm = SequenceMatcher(None, a, b, autojunk=False)
    edits = []
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            continue
        op = {"replace": "S", "delete": "D", "insert": "I"}[tag]
        if tag == "replace":
            n = max(i2 - i1, j2 - j1)
            for k in range(n):
                ai = i1 + k
                bj = j1 + k
                aw = a[ai] if ai < i2 else ""
                bw = b[bj] if bj < j2 else ""
                if aw:
                    span = spans[ai][1]
                else:
                    p = spans[ai][1][0] if ai < len(spans) else (spans[-1][1][1] if spans else 0)
                    span = [p, p]
                edits.append((op, aw, bw, span))
        elif tag == "delete":
            for i in range(i1, i2):
                edits.append(("D", a[i], "", spans[i][1]))
        elif tag == "insert":
            p = spans[i1][1][0] if i1 < len(spans) else (spans[-1][1][1] if spans else 0)
            for j in range(j1, j2):
                edits.append(("I", "", b[j], [p, p]))
    return edits


def cat_rule(op, aw, bw, misspell_set):
    al = A.strip_punct(aw.lower())
    bl = A.strip_punct(bw.lower())
    if (al, bl) in misspell_set:
        return "spelling"
    if al in ARTICLES or bl in ARTICLES:
        return "article"
    if op == "S":
        if (al.endswith("s") and bl == al[:-1]) or (bl.endswith("s") and al == bl[:-1]):
            return "agreement"
        if al in A.PAST_FORMS and bl in A.PAST_FORMS and A.PAST_FORMS[al] != A.PAST_FORMS[bl]:
            return "tense"
        if (al.endswith("ed") or bl.endswith("ed")) and (al in A.PAST_FORMS or bl in A.PAST_FORMS):
            return "tense"
        if al in A.PREPS and bl in A.PREPS:
            return "preposition"
        if al.endswith("ing") or bl.endswith("ing") or al == "to" or bl == "to":
            return "gerund_inf"
    return "other"


def main():
    misspell_set = set()
    lex = json.load(open(str(ROOT / "lexicons" / "en_gec_lexicon.json")))
    for w, r in lex.get("misspellings", []):
        misspell_set.add((w.lower(), r.lower()))

    out = []
    for split in ["dev", "test"]:
        src, refs = A.load(split)
        for i, s in enumerate(src):
            seen = set()
            edits = []
            for k in range(4):
                for op, aw, bw, span in diff_spans(s, refs[k][i]):
                    if (op, aw, bw) in seen:
                        continue
                    seen.add((op, aw, bw))
                    edits.append({
                        "op": op,
                        "src": aw,
                        "ref": bw,
                        "span": span,
                        "cat": A.classify_sub(aw, bw) if op == "S" else "other",
                        "cat_rule": cat_rule(op, aw, bw, misspell_set),
                    })
            out.append({"split": split, "src": s, "edits": edits})

    path = str(ROOT / "datasets" / "jfleg_gold.json")
    json.dump(out, open(path, "w"), ensure_ascii=False)
    from collections import Counter

    c = Counter(e["cat_rule"] for item in out for e in item["edits"])
    print(f"sentences: {len(out)}")
    for cat, n in c.most_common():
        print(f"  {cat}: {n}")


if __name__ == "__main__":
    main()

