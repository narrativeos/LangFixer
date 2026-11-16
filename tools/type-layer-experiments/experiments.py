import json, re, collections, os

HOME = os.path.expanduser("~")
V22 = f"{HOME}/tmp/dataset_v22"
TG = f"{HOME}/tmp/true_gold"
LEX = json.load(open("/Users/futuremeng/github/narrativeos/LangFixer/lexicons/zh_gec_lexicon.json"))

def v22_rows():
    for f in sorted(os.listdir(V22)):
        if f.endswith("_v22.jsonl"):
            for line in open(os.path.join(V22, f)):
                r = json.loads(line)
                r["_set"] = f[:-len("_v22.jsonl")]
                yield r

print("="*30, "实验1: 句内同型多错频率")
# coarse: true_gold rows grouped by id, type field as-is
by_id = collections.defaultdict(list)
for line in open(f"{TG}/true_gold.jsonl"):
    r = json.loads(line)
    by_id[r["id"]].append(r.get("type"))
n_all = len(by_id)
n_ge2 = 0; n_share = 0
for t in by_id.values():
    if len(t) >= 2:
        n_ge2 += 1
        if len(set(t)) < len(t): n_share += 1
print(f"true_gold 句数={n_all} ≥2错句={n_ge2} ({n_ge2/n_all:.1%}) 其中同型={n_share} ({n_share/max(n_ge2,1):.1%}) [粗型口径]")
# fine: join via text -> gold_types
id2text = {}
for r in v22_rows(): id2text[r["id"]] = r["src"]
text2types = {}
for line in open(f"{TG}/gold_types.jsonl"):
    g = json.loads(line); text2types.setdefault(g["text"], g["types"])
n_ge2b = 0; n_mult = 0; n_nomatch = 0
for iid, ts in by_id.items():
    if len(ts) < 2: continue
    txt = id2text.get(iid)
    if txt is None or txt not in text2types: n_nomatch += 1; continue
    E = len([x for x in by_id[iid]])  # edits counted
    T = len(text2types[txt])
    n_ge2b += 1
    if E > T: n_mult += 1
print(f"gold_types 匹配={n_ge2b} 同族多错={n_mult} ({n_mult/max(n_ge2b,1):.1%}) 未匹配={n_nomatch} [10型口径]")

print("="*30, "实验2: must桶 consistency+cohesion 规则可达性")
FAM = {
 "代词": r"[他她它其这那]",
 "时间": r"\d+年|当时|以前|后来|曾经",
 "数字": r"\d|[一二两三]|第",
 "关联词": r"因为|所以|虽然|但是|而且|然而|因此|不但|不仅|并且|可是|不过|于是|则",
}
cnt = collections.Counter(); tot = collections.Counter()
for line in open(f"{V22}/review_queue_new_errors.jsonl"):
    r = json.loads(line)
    if r["type"] not in ("consistency","cohesion"): continue
    tot[r["type"]] += 1
    src, a, b, fix = r["src"], r["a"], r["b"], r["fix"]
    span = src[a:b] if b > a else fix            # 插入编辑 span 空 -> 用 fix
    ctx = src[max(0,a-6):b+6] + fix             # 邻近上下文
    hit_span = [k for k,p in FAM.items() if re.search(p, span)]
    hit_ctx  = [k for k,p in FAM.items() if re.search(p, ctx)]
    if hit_span: cnt[r["type"], "span"] += 1
    if hit_ctx:  cnt[r["type"], "ctx"] += 1
for t in ("consistency","cohesion"):
    print(f"{t} n={tot[t]} span内命中={cnt[t,'span']} ({cnt[t,'span']/tot[t]:.1%}) ±6上下文命中={cnt[t,'ctx']} ({cnt[t,'ctx']/tot[t]:.1%})")
    fams = collections.Counter()
    for line in open(f"{V22}/review_queue_new_errors.jsonl"):
        r=json.loads(line)
        if r["type"]!=t: continue
        src,a,b=r["src"],r["a"],r["b"]
        span=src[a:b] if b>a else r["fix"]
        for k,p in FAM.items():
            if re.search(p,span): fams[k]+=1
    print("   span命中分族:", dict(fams))

print("="*30, "实验3: 规则臂下界（confusables+punct+anomaly, 同一本lexicon）")
CONF = []
for e in LEX["confusables"]:
    if isinstance(e, dict):
        CONF.append((e["wrong"], e["right"], e.get("condition", {})))
    else:
        CONF.append((e[0], e[1], None))
PMAP = {p[0]: p[1] for p in LEX["punct_map"]}
ANOM = [p for r in LEX["rules"] if r.get("category")=="anomaly" for p in r.get("patterns",[])]
ANOM = [(re.compile(p["pattern"]), p.get("strong", False)) for p in ANOM]
CJK = lambda ch: "\u4e00" <= ch <= "\u9fff"

def rule_hits(src):
    hits = []  # (a,b,kind)
    for wrong, right, cond in CONF:
        start = 0
        while True:
            i = src.find(wrong, start)
            if i < 0: break
            ok = True
            if cond:
                if "context_regex" in cond:
                    win = src[max(0,i-2): i+len(wrong)+2]
                    ok = re.search(cond["context_regex"], win) is not None
                if ok and "next_text" in cond:
                    j = i + len(wrong)
                    ok = j < len(src) and src[j] in cond["next_text"]
            if ok: hits.append((i, i+len(wrong), "conf"))
            start = i + 1
    for i, ch in enumerate(src):
        if ch in PMAP and ((i>0 and CJK(src[i-1])) or (i+1<len(src) and CJK(src[i+1]))):
            hits.append((i, i+1, "punct"))
    # anomaly: sentence split, terminator included
    sents = []; s = 0
    for i, ch in enumerate(src):
        if ch in "。！？!?\n":
            sents.append((s, i+1)); s = i+1
    if s < len(src): sents.append((s, len(src)))
    for a, b in sents:
        seg = src[a:b].lstrip()
        if seg and seg[0] in "「『“\"'《【(" : continue  # in-quote guard (approx)
        best = None
        for pat, strong in ANOM:
            if pat.search(seg): best = strong; break
        if best is not None:
            hits.append((a + src[a:b].find(seg), b, "anomaly"))
    return hits

# true_gold chars per id for precision
tgold_chars = collections.defaultdict(set)
for line in open(f"{TG}/true_gold.jsonl"):
    r = json.loads(line)
    if r["unit"] == "edit":
        tgold_chars[r["id"]].update(range(r["a"], r["b"]))
    else:
        tgold_chars[r["id"]].add(r["pos"])

rec_chars = collections.Counter()   # by set
hit_chars = collections.Counter()
tp_chars = collections.Counter(); fp_note = collections.Counter()
comp_stats = collections.defaultdict(lambda: collections.Counter())
by_type = collections.defaultdict(lambda: [0,0])  # coarse type -> [hit, total]
# join true_gold type per original gold char
tg_type = collections.defaultdict(dict)
for line in open(f"{TG}/true_gold.jsonl"):
    r = json.loads(line)
    if r["unit"]=="edit":
        for x in range(r["a"], r["b"]): tg_type[r["id"]][x] = r["type"]
    else: tg_type[r["id"]][r["pos"]] = r.get("type","?")

for r in v22_rows():
    src = r["src"]; st = r["_set"]
    goldc = set()
    for e in r["gold_edits"]: goldc.update(range(e["a"], e["b"]))
    for x in goldc:
        rec_chars[st] += 1; rec_chars["ALL"] += 1
        t = tg_type.get(r["id"], {}).get(x, "?"); by_type[t][1] += 1
    hits = rule_hits(src)
    hitc = set()
    for a, b, kind in hits:
        comp_stats[kind]["total"] += b - a
        hitc.update(range(a, b))
        for x in range(a, b):
            if x in goldc: comp_stats[kind]["tp"] += 1
    hit_chars[st] += len(hitc); hit_chars["ALL"] += len(hitc)
    tgc = tgold_chars.get(r["id"], set())
    for x in hitc:
        if x in goldc or x in tgc: tp_chars[st] += 1; tp_chars["ALL"] += 1
        else: fp_note[st] += 1; fp_note["ALL"] += 1
    for x in goldc & hitc:
        t = tg_type.get(r["id"], {}).get(x, "?"); by_type[t][0] += 1

print("recall vs 原始金集:")
gold_hit = collections.Counter()
# recompute per-set gold∩hit properly
for r in v22_rows():
    src=r["src"]
    goldc=set()
    for e in r["gold_edits"]: goldc.update(range(e["a"],e["b"]))
    hc=set()
    for a,b,k in rule_hits(src): hc.update(range(a,b))
    gold_hit[r["_set"]] += len(goldc & hc)
for st in sorted(set(k for k in rec_chars if k!="ALL")):
    if rec_chars[st]:
        print(f"  {st:9s} recall={gold_hit[st]}/{rec_chars[st]} = {gold_hit[st]/rec_chars[st]:.1%}  precision(∩true_gold)={tp_chars[st]}/{hit_chars[st]} = {tp_chars[st]/max(hit_chars[st],1):.1%}")
print(f"  ALL       recall={gold_hit and sum(gold_hit.values())}/{rec_chars['ALL']} = {sum(gold_hit.values())/rec_chars['ALL']:.1%}  precision={tp_chars['ALL']}/{hit_chars['ALL']} = {tp_chars['ALL']/hit_chars['ALL']:.1%}")
print("组件: ", {k: f"{v['tp']}/{v['total']} char-TP率={v['tp']/max(v['total'],1):.1%}" for k,v in comp_stats.items()})
print("按粗型 recall:", {t: f"{h}/{n}={h/n:.1%}" for t,(h,n) in sorted(by_type.items(), key=lambda kv:-kv[1][1]) if n>=50})
