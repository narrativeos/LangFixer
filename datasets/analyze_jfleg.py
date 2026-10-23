#!/usr/bin/env python3
"""JFLEG 编辑分布分析：对 src 与 4 个 ref 做词级 diff，
分类编辑 S(替换)/D(删除)/I(插入)，替换再细分，输出计数与样例。"""
import pathlib
from collections import Counter, defaultdict
from difflib import SequenceMatcher

BASE = str(pathlib.Path(__file__).resolve().parents[1] / "datasets" / "jfleg")


def load(split):
    src = open(f"{BASE}/{split}/{split}.src").read().splitlines()
    refs = [open(f"{BASE}/{split}/{split}.ref{k}").read().splitlines() for k in range(4)]
    return src, refs


def word_diff(a, b):
    sm = SequenceMatcher(None, a, b, autojunk=False)
    edits = []
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            continue
        if tag == "replace":
            n = max(i2 - i1, j2 - j1)
            for k in range(n):
                aw = a[i1 + k] if i1 + k < i2 else ""
                bw = b[j1 + k] if j1 + k < j2 else ""
                if aw and bw:
                    edits.append(("S", aw, bw))
                elif aw:
                    edits.append(("D", aw, ""))
                else:
                    edits.append(("I", "", bw))
        elif tag == "delete":
            edits.extend(("D", a[k], "") for k in range(i1, i2))
        elif tag == "insert":
            edits.extend(("I", "", b[k]) for k in range(j1, j2))
    return edits

PAST_FORMS = {
    "go": "went", "goes": "went", "went": "went", "see": "saw", "saw": "saw",
    "seen": "saw", "make": "made", "makes": "made", "made": "made",
    "take": "took", "took": "took", "eat": "ate", "ate": "ate",
    "run": "ran", "ran": "ran", "come": "came", "comes": "came", "came": "came",
    "get": "got", "gets": "got", "got": "got", "give": "gave", "gave": "gave",
    "find": "found", "found": "found", "think": "thought", "thought": "thought",
    "know": "knew", "knew": "knew", "say": "said", "says": "said", "said": "said",
    "tell": "told", "told": "told", "buy": "bought", "bought": "bought",
    "write": "wrote", "wrote": "wrote", "speak": "spoke", "spoke": "spoke",
    "drive": "drove", "drove": "drove", "sit": "sat", "sits": "sat", "sat": "sat",
    "stand": "stood", "stood": "stood", "begin": "began", "began": "began",
    "feel": "felt", "feels": "felt", "felt": "felt", "hold": "held", "held": "held",
    "leave": "left", "leaves": "left", "left": "left", "pay": "paid", "paid": "paid",
    "win": "won", "wins": "won", "won": "won", "lose": "lost", "lost": "lost",
    "bring": "brought", "brought": "brought", "build": "built", "built": "built",
    "catch": "caught", "caught": "caught", "teach": "taught", "taught": "taught",
    "draw": "drew", "drew": "drew", "grow": "grew", "grew": "grew",
    "ride": "rode", "rode": "rode", "break": "broke", "broke": "broke",
    "choose": "chose", "chose": "chose", "fall": "fell", "falls": "fell", "fell": "fell",
    "forget": "forgot", "forgot": "forgot", "swim": "swam", "swam": "swam",
    "sleep": "slept", "slept": "slept", "wear": "wore", "wore": "wore",
    "hear": "heard", "hears": "heard", "heard": "heard", "lead": "led", "led": "led",
    "read": "read", "cut": "cut", "hit": "hit", "let": "let", "set": "set",
    "send": "sent", "sends": "sent", "sent": "sent", "spend": "spent",
    "spends": "spent", "spent": "spent", "understand": "understood", "understood": "understood",
    "meet": "met", "meets": "met", "met": "met", "show": "showed", "shows": "showed",
    "showed": "showed", "shown": "showed", "shut": "shut", "spread": "spread",
}
PAST_FORMS.update({
    "turn": "turned", "turns": "turned", "turned": "turned", "walk": "walked",
    "walks": "walked", "walked": "walked", "talk": "talked", "talks": "talked",
    "talked": "talked", "play": "played", "plays": "played", "played": "played",
    "work": "worked", "works": "worked", "worked": "worked", "help": "helped",
    "helps": "helped", "helped": "helped", "watch": "watched", "watched": "watched",
    "start": "started", "starts": "started", "started": "started", "stop": "stopped",
    "stops": "stopped", "stopped": "stopped", "finish": "finished", "finished": "finished",
    "open": "opened", "opens": "opened", "opened": "opened", "close": "closed",
    "closes": "closed", "closed": "closed", "sell": "sold", "sells": "sold", "sold": "sold",
    "call": "called", "calls": "called", "called": "called", "ask": "asked",
    "asks": "asked", "asked": "asked", "use": "used", "uses": "used", "used": "used",
    "try": "tried", "tries": "tried", "tried": "tried", "want": "wanted",
    "wants": "wanted", "wanted": "wanted", "need": "needed", "needs": "needed",
    "needed": "needed", "like": "liked", "likes": "liked", "liked": "liked",
    "live": "lived", "lives": "lived", "lived": "lived", "love": "loved",
    "loves": "loved", "loved": "loved", "learn": "learned", "learns": "learned",
    "learned": "learned", "move": "moved", "moves": "moved", "moved": "moved",
    "change": "changed", "changes": "changed", "changed": "changed",
    "continue": "continued", "continued": "continued", "develop": "developed",
    "developed": "developed", "depend": "depended", "depended": "depended",
    "discover": "discovered", "discovered": "discovered", "end": "ended",
    "ends": "ended", "ended": "ended", "enter": "entered", "entered": "entered",
    "explain": "explained", "explained": "explained", "fail": "failed",
    "fails": "failed", "failed": "failed", "follow": "followed", "follows": "followed",
    "followed": "followed", "focus": "focused", "focused": "focused",
    "gain": "gained", "gained": "gained", "happen": "happened", "happens": "happened",
    "happened": "happened", "increase": "increased", "increases": "increased",
    "increased": "increased", "include": "included", "includes": "included",
    "included": "included", "improve": "improved", "improves": "improved",
    "improved": "improved", "introduce": "introduced", "introduced": "introduced",
    "keep": "kept", "keeps": "kept", "kept": "kept", "laugh": "laughed",
    "laughed": "laughed", "look": "looked", "looks": "looked", "looked": "looked",
    "manage": "managed", "managed": "managed", "matter": "mattered", "mattered": "mattered",
    "notice": "noticed", "notices": "noticed", "noticed": "noticed", "offer": "offered",
    "offers": "offered", "offered": "offered", "pass": "passed", "passes": "passed",
    "passed": "passed", "pick": "picked", "picked": "picked", "point": "pointed",
    "points": "pointed", "pointed": "pointed", "prepare": "prepared", "prepared": "prepared",
    "present": "presented", "presented": "presented", "prove": "proved",
    "proves": "proved", "proved": "proved", "provide": "provided", "provides": "provided",
    "provided": "provided", "raise": "raised", "raises": "raised", "raised": "raised",
    "remain": "remained", "remains": "remained", "remained": "remained",
    "remember": "remembered", "remembers": "remembered", "remembered": "remembered",
    "report": "reported", "reports": "reported", "reported": "reported",
    "require": "required", "requires": "required", "required": "required",
    "return": "returned", "returns": "returned", "returned": "returned",
    "rise": "rose", "rises": "rose", "rose": "rose", "save": "saved",
    "saves": "saved", "saved": "saved", "seem": "seemed", "seems": "seemed",
    "seemed": "seemed", "stay": "stayed", "stays": "stayed", "stayed": "stayed",
    "study": "studied", "studies": "studied", "studied": "studied", "suffer": "suffered",
    "suffered": "suffered", "support": "supported", "supports": "supported",
    "supported": "supported", "suppose": "supposed", "supposed": "supposed",
    "train": "trained", "trained": "trained", "travel": "travelled", "travelled": "travelled",
    "traveled": "traveled", "visit": "visited", "visits": "visited", "visited": "visited",
    "wait": "waited", "waits": "waited", "waited": "waited", "wake": "woke",
    "wakes": "woke", "woke": "woke", "warn": "warned", "warns": "warned",
    "warned": "warned", "wonder": "wondered", "wonders": "wondered", "wondered": "wondered",
    "worry": "worried", "worries": "worried", "worried": "worried", "wish": "wished",
    "wishes": "wished", "wished": "wished",
})
ARTICLES = {"a", "an", "the"}
PREPS = {
    "in", "on", "at", "of", "to", "for", "with", "by", "from", "into",
    "about", "over", "under", "between", "through", "during", "after",
    "before", "against", "among", "within", "upon", "beyond", "toward",
    "towards", "outside", "inside", "across", "along", "around", "behind",
    "beside", "besides", "below", "above", "near", "off", "up", "down",
    "out", "via", "per", "since", "until", "till", "except", "like",
    "such", "past", "round", "onto",
}
BE_FORMS = {"be", "am", "is", "are", "was", "were", "been", "being"}


def strip_punct(w):
    return w.strip(",.!?;:'\"")


def classify_sub(aw, bw):
    al = strip_punct(aw.lower())
    bl = strip_punct(bw.lower())
    if al == bl:
        return "case/punct"
    if al in PAST_FORMS and bl in PAST_FORMS and PAST_FORMS[al] != PAST_FORMS[bl]:
        return "tense"
    if al in PAST_FORMS or bl in PAST_FORMS:
        if al.endswith("ed") or bl.endswith("ed"):
            return "tense"
        if al in BE_FORMS or bl in BE_FORMS:
            return "be-form"
    if al.endswith("s") and not al.endswith("ss") and bl == al[:-1]:
        return "agreement"
    if bl.endswith("s") and al == bl[:-1]:
        return "agreement"
    if al in ARTICLES or bl in ARTICLES:
        return "article"
    if al in PREPS or bl in PREPS:
        return "preposition"
    if al.endswith("ly") or bl.endswith("ly"):
        return "adverb"
    if al in {"more", "most"} or bl in {"more", "most"}:
        return "comparative"
    if al.endswith("ing") or bl.endswith("ing"):
        return "gerund/inf"
    if al == "to" or bl == "to":
        return "to-inf"
    sm = SequenceMatcher(None, al, bl).ratio()
    if sm > 0.6 and len(al) > 2:
        return "spelling"
    return "other-sub"


def main():
    op_counts = Counter()
    sub_counts = Counter()
    sub_examples = defaultdict(list)
    del_counts = Counter()
    ins_counts = Counter()
    n_sentences = 0
    total_edits = 0
    for split in ["dev", "test"]:
        src, refs = load(split)
        for i, s in enumerate(src):
            if i >= len(refs[0]):
                break
            n_sentences += 1
            a = s.split()
            seen = set()
            for k in range(4):
                b = refs[k][i].split()
                for op, aw, bw in word_diff(a, b):
                    total_edits += 1
                    if (op, aw, bw) in seen:
                        continue
                    seen.add((op, aw, bw))
                    op_counts[op] += 1
                    if op == "S":
                        c = classify_sub(aw, bw)
                        sub_counts[c] += 1
                        if len(sub_examples[c]) < 10:
                            sub_examples[c].append(f"{aw} -> {bw}  [{s[:55]}]")
                    elif op == "D":
                        del_counts[aw] += 1
                    else:
                        ins_counts[bw] += 1
    print(f"=== JFLEG 编辑分布（dev+test，4 参考去重） ===")
    print(f"句子数: {n_sentences}, 去重编辑总数: {total_edits}")
    print("\n-- 操作类型 --")
    for op, c in op_counts.most_common():
        print(f"  {op}: {c}")
    print("\n-- 替换细分 --")
    for c, n in sub_counts.most_common():
        print(f"  {c}: {n}")
        for ex in sub_examples[c][:5]:
            print(f"      e.g. {ex}")
    print("\n-- 删除 top30 --")
    for w, c in del_counts.most_common(30):
        print(f"  {w}: {c}")
    print("\n-- 插入 top30 --")
    for w, c in ins_counts.most_common(30):
        print(f"  {w}: {c}")


if __name__ == "__main__":
    main()



