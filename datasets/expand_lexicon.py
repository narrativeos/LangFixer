#!/usr/bin/env python3
"""从 JFLEG 语料提取高频错词对，扩充 lexicons/en_gec_lexicon.json：
- misspellings（新增字段）：高相似度词对（拼写错误）
- preposition_confusables（新增字段）：介词↔介词 误用
- uncountable_nouns（已有字段）：补入高频 复数→不可数 对
- past_tenses / common_verbs：补入 PAST_FORMS 中的新条目
输出完整 JSON 回写原文件（保留原有条目）。"""
import pathlib
import json
from difflib import SequenceMatcher
from collections import Counter

import analyze_jfleg as A

LEX = str(pathlib.Path(__file__).resolve().parents[1] / "lexicons" / "en_gec_lexicon.json")

STOP = {
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "of",
    "in", "on", "at", "by", "for", "with", "and", "or", "but", "that", "this",
    "these", "those", "it", "he", "she", "they", "we", "you", "i", "me", "him",
    "her", "them", "us", "my", "your", "his", "its", "our", "their", "not",
    "no", "do", "does", "did", "have", "has", "had", "will", "would", "can",
    "could", "should", "may", "might", "must", "so", "as", "if", "from",
    "into", "over", "under", "about", "between", "through", "during", "after",
    "before", "against", "among", "within", "upon", "beyond", "toward",
    "towards", "outside", "inside", "across", "along", "around", "behind",
    "beside", "besides", "below", "above", "off", "up", "down", "out", "via",
    "per", "since", "until", "till", "except", "such", "past", "round", "onto",
    "very", "too", "just", "also", "only", "all", "any", "some", "one", "two",
    "many", "much", "more", "most", "make", "makes", "made",
}


def collect_pairs():
    counts = Counter()
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
                    key = (aw, bw)
                    if key in seen:
                        continue
                    seen.add(key)
                    counts[(A.strip_punct(aw.lower()), A.strip_punct(bw.lower()))] += 1
    return counts


def main():
    counts = collect_pairs()
    d = json.load(open(LEX))

    # 1) 拼写：高相似度、非停用词、长度>2
    misspell = d.setdefault("misspellings", [])
    have = {(w, r) for w, r in misspell}
    cand = []
    for (aw, bw), n in counts.items():
        if aw in STOP or bw in STOP or aw == bw:
            continue
        if len(aw) <= 2 or len(bw) <= 2:
            continue
        sim = SequenceMatcher(None, aw, bw).ratio()
        if sim >= 0.6 and n >= 4:
            cand.append((n, sim, aw, bw))
    cand.sort(reverse=True)
    for n, sim, aw, bw in cand[:80]:
        if (aw, bw) not in have:
            misspell.append([aw, bw])
            have.add((aw, bw))

    # 2) 介词↔介词
    preps = d.setdefault("preposition_confusables", [])
    have_p = {(w, r) for w, r in preps}
    cand = []
    for (aw, bw), n in counts.items():
        if aw in A.PREPS and bw in A.PREPS and n >= 5:
            cand.append((n, aw, bw))
    cand.sort(reverse=True)
    for n, aw, bw in cand[:40]:
        if (aw, bw) not in have_p:
            preps.append([aw, bw])
            have_p.add((aw, bw))

    # 3) 不可数名词：复数形式 → 不可数（informations -> information）
    uncount = d.setdefault("uncountable_nouns", [])
    have_u = {(w, r) for w, r in uncount}
    cand = []
    for (aw, bw), n in counts.items():
        if aw in STOP or bw in STOP:
            continue
        if n < 3:
            continue
        if aw.endswith("s") and bw == aw[:-1] and len(bw) > 3:
            cand.append((n, aw, bw))
    cand.sort(reverse=True)
    for n, aw, bw in cand[:40]:
        if (aw, bw) not in have_u:
            uncount.append([aw, bw])
            have_u.add((aw, bw))

    # 4) past_tenses / common_verbs 补全（PAST_FORMS 中 base -> past）
    pt = d.setdefault("past_tenses", [])
    have_pt = {(w, r) for w, r in pt}
    cv = d.setdefault("common_verbs", [])
    have_cv = set(cv)
    past_values = set(A.PAST_FORMS.values())

    def is_conjugate(k, b):
        # k 是否为 b 的屈折形式（plays / passes / studies）
        if k == b + "s" or k == b + "es":
            return True
        if b.endswith("y") and k == b[:-1] + "ies":
            return True
        return False

    # 按过去式分组：{went: [go, goes, went], played: [play, plays, played], ...}
    groups = {}
    for k, v in A.PAST_FORMS.items():
        groups.setdefault(v, []).append(k)
    for v, keys in groups.items():
        for k in keys:
            # k 是组内其他词的屈折形式 → 非原形
            if any(is_conjugate(k, b) for b in keys if b != k):
                continue
            # k 是过去式且组内有其他原形候选 → 非原形（零变化动词如 cut 组内仅自身，保留）
            if k in past_values and len(keys) > 1:
                continue
            if (k, v) not in have_pt:
                pt.append([k, v])
                have_pt.add((k, v))
            if k not in have_cv:
                cv.append(k)
                have_cv.add(k)

    json.dump(d, open(LEX, "w"), ensure_ascii=False, indent=2)
    print(f"misspellings: {len(misspell)}, preposition_confusables: {len(preps)}, "
          f"uncountable_nouns: {len(uncount)}, past_tenses: {len(pt)}, common_verbs: {len(cv)}")


if __name__ == "__main__":
    main()

