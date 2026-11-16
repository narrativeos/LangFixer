#!/usr/bin/env python3
"""把「门控定位 → LLM 判定」配方推广到全部金集，产出 dataset_v2（带 provenance 与抽检队列）。

产出目录默认 ~/tmp/dataset_v2（**不写进 LangFixer 仓库**，由你决定何时导入）。
四步：门控字级打分 → 候选定位 → LLM 判定（可答「无错」）→ 汇总 + 分档 + 队列。
"""
import concurrent.futures as cf, hashlib, json, os, re, statistics, sys, time
import urllib.request

GP = os.path.dirname(os.path.abspath(__file__))
GP = os.path.expanduser(os.environ.get("GP_DATA", GP))   # 仓库副本可用 GP_DATA 指回数据目录
OUT = os.path.expanduser(os.environ.get("OUT", "~/tmp/dataset_v2"))
BASE = os.environ.get("LANGFIXER_LLM_BASE", "http://192.168.1.130:4000/v1")
KEY = os.environ.get("LANGFIXER_LLM_KEY", "")
MODEL = os.environ.get("LANGFIXER_LLM_MODEL", "Qwen3.8")
CONC = int(os.environ.get("CONC", "16"))
TAU = float(os.environ.get("TAU", "0.5"))
CAP = int(os.environ.get("CAP", "12"))
VER = os.environ.get("VER", "v2.1")
NORMAL = {"logic": "cohesion", "word": "grammar", "lexical": "grammar",
          "搭配": "grammar", "标点": "punct", "错别字": "typo", "语法": "grammar"}
DEVICE = os.environ.get("DEVICE", "mps")
PUNCTCH = set("，。、；：？！“”‘’（）《》〈〉【】〔〕—…·,.;:?!()<>[]{}~·")
TYPES = "typo|grammar|punct|consistency|fact|cohesion"
ENUM = set(TYPES.split("|"))
MODE_SNIP = VER.replace(".", "") in ("v22", "v23")   # 片段+整句提问 + 第二召回通道
REPO = os.environ.get("REPO", os.path.expanduser("~/github/narrativeos/LangFixer"))
RAD, RARE = 6, int(os.environ.get("RARE", "15"))
PUNCT_HALF = set(',.;:?!()[]{}<>"\'')
PUNCT_TERM = set("，。、；：？！")
DELWHY = __import__("re").compile(r"删|多余|冗余|多出|重复")
WHYHIT = re.compile(r"删|多余|冗余|多出|重复|漏|语序|搭配|少了|多了")   # v2.3①预筛
MODE_FINE = VER.replace(".", "") == "v23"                # v2.3：why预筛+单片段重试
SYS_SNIP = ("你是中文纠错判定器。用户给出整句和若干**片段**（用「」括出）。判断每个片段内是否含错误，"
            '只输出严格 JSON：{"decisions":[{"no":片段序号,"err":true,"type":'
            '"' + TYPES + '","fix":"该片段的修正","why":"≤20字"}]}。'
            "必须结合整句判断；没有错就 err:false。若该处应删除，fix 给出去掉该字后的片段；"
            "若是漏字/语序调整，fix 给出补全或调整后的片段。宁可 err:false 也不要乱改。")
RECIPE = ("gate+lex2_localize → llm_snippet_adjudicate(片段±" + str(RAD) + "+整句,分窗,可答无错)"
          if MODE_SNIP else "gate_localize → llm_adjudicate(分窗,含标点)")
if MODE_FINE:
    RECIPE += " +why预筛单片段重试 +单片段mtok细化 +parse_fail口径修正"
SYS = ("你是中文纠错判定器。用户给一个文本和若干**可疑位置**（按字序号标出）。"
       "对每个位置判断是否真有错，只输出严格 JSON："
       '{"decisions":[{"at":序号,"err":true,"type":"' + TYPES + '","fix":"修正后的该处文本","why":"≤20字"}]}。'
       "没有错就 err:false（省略 type/fix）。宁可 err:false，也不要把对的改成别的。")
LOG = open(os.path.join(GP, f"rollout_{VER.replace('.','')}.log"), "a", encoding="utf-8", buffering=1)
JRN = os.path.join(GP, f"rollout_journal_{VER.replace('.','')}.jsonl")
_jlock = __import__("threading").Lock()


def log(*a):
    s = " ".join(str(x) for x in a)
    print(s, file=LOG, flush=True)
    print(s, flush=True)


def b2c(src, lo, hi):
    return (len(src.encode("utf-8")[:lo].decode("utf-8", "ignore")),
            len(src.encode("utf-8")[:hi].decode("utf-8", "ignore")))


def stage1_gate(rows):
    import torch
    from transformers import AutoTokenizer, AutoModelForTokenClassification
    dev = torch.device(DEVICE if DEVICE == "cpu" or torch.backends.mps.is_available() else "cpu")
    tok = AutoTokenizer.from_pretrained(os.path.join(GP, "model"))
    model = AutoModelForTokenClassification.from_pretrained(os.path.join(GP, "model")).to(dev).eval()
    WIN, STRIDE, BATCH = 190, 150, 32
    log("[1/4] 门控字级打分…")
    scores, t0 = {}, time.time()
    for i in range(0, len(rows), 512):
        chunk = rows[i:i + 512]
        jobs = []
        for ti, t in enumerate([r["src"] for r in chunk]):
            if len(t) <= WIN:
                jobs.append((ti, 0, t))
            else:
                for st in range(0, len(t), STRIDE):
                    jobs.append((ti, st, t[st:st + WIN]))
        out = [[0.0] * len(r["src"]) for r in chunk]
        with torch.no_grad():
            for j in range(0, len(jobs), BATCH):
                ch = jobs[j:j + BATCH]
                enc = tok([c[2] for c in ch], return_tensors="pt", padding=True, truncation=True,
                          max_length=256, return_offsets_mapping=True)
                om = enc.pop("offset_mapping").tolist()
                enc = {k: v.to(dev) for k, v in enc.items()}
                p = torch.softmax(model(**enc).logits.float(), dim=-1)[..., 1].cpu().tolist()
                for bi, (ti, st, seg) in enumerate(ch):
                    for k, (a, b) in enumerate(om[bi]):
                        if a is None or b <= a:
                            continue
                        for q in range(int(a), min(int(b), len(seg))):
                            out[ti][st + q] = max(out[ti][st + q], p[bi][k])
        for r, o in zip(chunk, out):
            scores[r["id"]] = [round(v, 3) for v in o]
        log(f"      {min(i+512,len(rows)):,}/{len(rows):,} · {time.time()-t0:.0f}s")
    return scores


def punct_cands(s):
    out = set()
    for i, ch in enumerate(s):
        zh_l = i > 0 and "\u4e00" <= s[i - 1] <= "\u9fff"
        zh_r = i + 1 < len(s) and "\u4e00" <= s[i + 1] <= "\u9fff"
        if ch in PUNCT_HALF and (zh_l or zh_r):
            out.add(i)                       # 中文语流里的半角标点
        if ch in PUNCT_TERM and i + 1 < len(s) and s[i + 1] in PUNCT_TERM:
            out.add(i); out.add(i + 1)       # 叠用的句内/句末标点
    return out


def oov_cands(s, tri, bg, freq):
    pos = [i for i, c in enumerate(s) if "\u4e00" <= c <= "\u9fff"]
    z = [s[i] for i in pos]
    votes = {}
    for k in range(len(z) - 2):
        if z[k] + z[k + 1] + z[k + 2] in tri:
            continue
        m1 = z[k] + z[k + 1] not in bg
        m2 = z[k + 1] + z[k + 2] not in bg
        if m1 and m2:
            votes[pos[k + 1]] = votes.get(pos[k + 1], 0) + 2   # 两个 bigram 都断→中心字
        elif m1:
            votes[pos[k]] = votes.get(pos[k], 0) + 1           # 断头→边缘字
        elif m2:
            votes[pos[k + 2]] = votes.get(pos[k + 2], 0) + 1
    return {p for p, v in votes.items() if v >= 2 or freq.get(s[p], 1e9) <= RARE}


def channel2(s, tri, bg, freq):
    return punct_cands(s) | oov_cands(s, tri, bg, freq)


def frags_of(s, cs):
    if not cs:
        return []
    spans, st, last = [], cs[0], cs[0]
    for c in cs[1:]:
        if c - last <= 2 * RAD:
            last = c
        else:
            spans.append((st, last)); st = last = c
    spans.append((st, last))
    return [(max(0, a - RAD), min(len(s), b + RAD + 1)) for a, b in spans]


def call(t, cands, timeout=240, tries=4, mtok=None):
    last = None
    for a in range(tries):
        try:
            return _call1(t, cands, timeout, mtok)
        except Exception as e:                                # 502/超时不能掀掉整轮
            last = e
            time.sleep(min(30, 2 ** a) + 0.5 * a)
    raise last


def _call1(t, cands, timeout=240, mtok=None):
    if MODE_SNIP:                            # cands = 片段窗口 [(a,b),…]
        u = "整句：" + t + "\n" + "\n".join(f"片段{i+1}：「{t[a:b]}」" for i, (a, b) in enumerate(cands))
        sysm, mtok = SYS_SNIP, mtok or 800
    else:
        u = t + "\n可疑位置：" + "、".join(f"第{c+1}字「{t[c]}」" for c in cands)
        sysm, mtok = SYS, mtok or 700
    body = {"model": MODEL, "messages": [{"role": "system", "content": sysm}, {"role": "user", "content": u}],
            "temperature": 0, "max_tokens": mtok, "chat_template_kwargs": {"enable_thinking": False}}
    req = urllib.request.Request(BASE.rstrip("/") + "/chat/completions", data=json.dumps(body).encode(),
                                 headers={"Authorization": "Bearer " + KEY, "content-type": "application/json"})
    t0 = time.time()
    d = json.load(urllib.request.urlopen(req, timeout=timeout))
    ch = d["choices"][0]
    return (ch["message"].get("content") or "").strip(), ch.get("finish_reason"), time.time() - t0


def main():
    os.makedirs(OUT, exist_ok=True)
    rows = json.load(open(os.path.join(GP, "type_proxy.json"), encoding="utf-8"))
    for i, r in enumerate(rows):                    # id 必须按全量顺序分配（否则分数会错位）
        r["id"] = f"{r['set']}-{i}"
    if os.environ.get("SUBSET"):
        import random
        random.seed(3)
        rows = random.sample(rows, int(os.environ["SUBSET"]))
    if os.path.exists(os.path.join(GP, "gate_charscores.json")):
        scores = json.load(open(os.path.join(GP, "gate_charscores.json"), encoding="utf-8"))
        log(f"[1/4] 复用门控字级分数 {len(scores):,} 条")
    else:
        scores = stage1_gate(rows)
        json.dump(scores, open(os.path.join(GP, "gate_charscores.json"), "w"))
    assert all(r["id"] in scores and len(scores[r["id"]]) == len(r["src"]) for r in rows[:50])

    cands = {}
    for r in rows:
        sc = scores[r["id"]]
        c = [i for i in range(len(r["src"])) if sc[i] >= TAU
             and ("\u4e00" <= r["src"][i] <= "\u9fff" or r["src"][i] in PUNCTCH)]
        cands[r["id"]] = sorted(c)          # 不截断：多的候选分窗问（见 job）
    if MODE_SNIP:
        tri = json.load(open(os.path.join(REPO, "feature-model", "ngram_table.json"), encoding="utf-8"))
        bg = frozenset(json.load(open(os.path.join(REPO, "feature-model", "bigram_table_zh.json"), encoding="utf-8")))
        from collections import Counter
        freq = Counter(c for r in rows for c in r["src"] if "\u4e00" <= c <= "\u9fff")
        unseen = cov = n2 = new_rows = 0
        for r in rows:
            g = set()
            for e in r.get("edits") or []:
                a, b = b2c(r["src"], *e["span"])
                g.update(range(a, max(b, a + 1)))
            u = {c for c in g if c < len(r["src"])} - set(cands[r["id"]])
            add = channel2(r["src"], tri, bg, freq) - set(cands[r["id"]])
            n2 += len(add)
            new_rows += not cands[r["id"]] and bool(add)
            unseen += len(u); cov += len(u & add)
            cands[r["id"]] = sorted(set(cands[r["id"]]) | add)
        log(f"[2/4b] 第二召回通道（标点规则+OOV/罕见字, RARE={RARE}）：补 {n2:,} 候选字 · "
            f"门控未见 {unseen:,} 找回 {cov:,}（{cov*100/max(1,unseen):.1f}%）· 新入判定行 {new_rows:,}")
    ncand = sum(len(v) for v in cands.values())
    todo = [r for r in rows if cands[r["id"]]]
    log(f"[2/4] 候选定位：{ncand:,} 处 / {sum(len(r['src']) for r in rows)/1e6:.2f}M 字"
        f"（{ncand*1000/max(1,sum(len(r['src']) for r in rows)):.1f}/千字）· 需判定 {len(todo):,} 行 · "
        f"无候选 {len(rows)-len(todo):,} 行按门控判「无需处理」")

    phash = hashlib.sha256(SYS.encode()).hexdigest()[:12]
    log(f"[3/4] LLM 判定（{MODEL}, prompt {phash}, 并发 {CONC}）…")
    done, t0, lock = {}, time.time(), __import__("threading").Lock()
    if os.path.exists(JRN):                                  # 断点续跑
        for l in open(JRN, encoding="utf-8"):
            try:
                d0 = json.loads(l)
                done[d0["id"]] = d0["state"]
            except Exception:
                pass
        log(f"      续跑：从日志恢复 {len(done):,} 行")
    todo = [r for r in todo if r["id"] not in done]
    jf = open(JRN, "a", encoding="utf-8")

    def parse(raw, src):
        try:
            got = json.loads(re.sub(r"^```|```$", "", raw.strip(), flags=re.M).strip())["decisions"]
        except Exception:
            return []
        out = []
        for g in got:
            try:
                k = int(g.get("at", 0)) - 1
            except Exception:
                continue
            if 0 <= k < len(src) and g.get("err"):
                ty = str(g.get("type") or "")
                if ty not in ENUM:
                    ty = NORMAL.get(ty, "grammar")
                out.append({"a": k, "b": k + 1, "type": ty, "fix": g.get("fix"), "why": g.get("why")})
        return out

    def parse_snip(raw, src, win, rid):
        # 返回 (dec, anom, ok, flagged)：ok=JSON 解析成否（v2.3③ parse_fail 口径），
        # flagged=判无错但 why 露出修正意图的片段序（v2.3①预筛）
        try:
            got = json.loads(re.sub(r"^```|```$", "", raw.strip(), flags=re.M).strip())["decisions"]
        except Exception:
            return [], 0, False, []
        dec, anom, flagged = [], 0, []
        for g in got:
            try:
                k = int(g.get("no", 0)) - 1
            except Exception:
                continue
            if not (0 <= k < len(win)):
                continue
            if not g.get("err"):
                if WHYHIT.search(str(g.get("why") or "")):
                    flagged.append(k)
                continue
            a, b = win[k]
            frag, fix = src[a:b], g.get("fix")
            fix = "" if fix is None else str(fix)
            ty = str(g.get("type") or "")
            if ty not in ENUM:
                ty = NORMAL.get(ty, "grammar")
            fa = 0
            while fa < len(frag) and fa < len(fix) and frag[fa] == fix[fa]:
                fa += 1
            fb, fc = len(frag), len(fix)
            while fb > fa and fc > fa and frag[fb - 1] == fix[fc - 1]:
                fb -= 1; fc -= 1
            if fb == fa and fc == fa:        # 片段未被真正修改：v2.1 的 fix==原文缺陷
                anom += 1
                log(f"      [anomaly] {rid} 片段未改而 err:true · why={g.get('why','')[:40]}")
                continue
            dec.append({"a": a + fa, "b": a + fb, "type": ty, "fix": fix[fa:fc], "why": g.get("why")})
        return dec, anom, True, flagged

    def job(r):
        try:
            return _job(r)
        except Exception as e:                                # 单行失败只记错误，不炸整轮
            with lock:
                done[r["id"]] = {"dec": [], "sec": 0.0, "fin": f"http_error:{type(e).__name__}"}
                jf.write(json.dumps({"id": r["id"], "state": done[r["id"]]}, ensure_ascii=False) + "\n")
                jf.flush()
            return None

    def _job(r):
        cs = cands[r["id"]]
        dec, secs, fin2, nfail, anom = [], [], "stop", 0, 0
        if MODE_SNIP:
            frs = frags_of(r["src"], cs)
            stack = [[frs[k:k + CAP], 0] for k in range(0, max(1, len(frs)), CAP)]
            while stack:
                win, tries = stack.pop(0)
                if not win:
                    continue
                raw, fin, sec = call(r["src"], win)
                secs.append(sec); fin2 = fin
                got, a2, ok2, flagged = parse_snip(raw, r["src"], win, r["id"])
                anom += a2
                if fin == "length" and len(win) > 1:      # 截断→对半重试，不丢窗口
                    h = len(win) // 2
                    stack[0:0] = [[win[:h], tries], [win[h:], tries]]
                    continue
                if MODE_FINE and fin == "length" and len(win) == 1 and tries == 0:
                    # v2.3② 单片段仍截断：拉高 max_tokens 重试一次
                    raw, fin, sec = call(r["src"], win, mtok=1600)
                    secs.append(sec); fin2 = fin
                    got, a2, ok2, flagged = parse_snip(raw, r["src"], win, r["id"])
                    anom += a2; tries = 1
                if MODE_FINE and flagged and tries == 0:  # v2.3① 预筛命中：单片段重试一次
                    stack[0:0] = [[[win[k]], 1] for k in flagged if k < len(win)]
                nfail += not ok2 and bool(win) and fin != "length"
                dec += got
        else:
            for k in range(0, max(1, len(cs)), CAP):
                raw, fin, sec = call(r["src"], cs[k:k + CAP])
                secs.append(sec); fin2 = fin
                got = parse(raw, r["src"])
                nfail += not got and bool(cs[k:k + CAP])
                dec += got
        dec.sort(key=lambda e: e["a"])
        with lock:
            done[r["id"]] = {"dec": dec, "sec": sum(secs), "fin": fin2, "nwin": len(secs),
                             "parse_fail": nfail, "anomaly": anom}
            jf.write(json.dumps({"id": r["id"], "state": done[r["id"]]}, ensure_ascii=False) + "\n")
            jf.flush()
            if len(done) % 1000 == 0:
                el = time.time() - t0
                log(f"      {len(done):,}/{len(todo):,} · {el/60:.1f} 分钟 · p50 "
                    f"{statistics.median([v['sec'] for v in done.values()])*1000:.0f} ms")

    with cf.ThreadPoolExecutor(CONC) as ex:
        list(ex.map(job, todo))
    log(f"      完成 {len(done):,} 行 · {(time.time()-t0)/60:.1f} 分钟")

    jf.close()
    nerr = sum(1 for v in done.values() if str(v.get("fin", "")).startswith("http_error"))
    if nerr:
        log(f"      {nerr:,} 行因端点错误未完成 —— 重新运行本脚本即可续跑这些行")
    log("[4/4] 汇总 + 分档 + 队列…")
    by_set, queue, stat = {}, [], {"auto_clean": 0, "consensus": 0, "model_new": 0, "model_miss": 0,
                                   "type": {}, "rows": 0, "chars": 0, "parse_fail": sum(
                                       1 for v in done.values() for _ in [0] * bool(v.get("parse_fail"))),
                                   "snip_anomaly": sum(v.get("anomaly", 0) for v in done.values())}
    for r in rows:
        src, d = r["src"], done.get(r["id"], {"dec": [], "fin": "no_cand"})
        gold = []
        gset = set()
        for e in r.get("edits") or []:
            a, b = b2c(src, *e["span"])
            gold.append({"a": a, "b": b, "op": e.get("op"), "ref": e.get("ref"), "bytes": e["span"]})
            gset.update(range(a, max(b, a + 1)))
        mset, cons, new, miss = set(), 0, [], 0
        for e in d["dec"]:                    # 逐条口径（累计并集曾把共识高估 3,685）
            es = set(range(e["a"], e["b"]))
            mset |= es
            stat["type"][e["type"]] = stat["type"].get(e["type"], 0) + len(es)
            if es & gset:
                cons += 1
            else:
                new.append(e)
        miss = len(gset - mset)
        tier = "auto_clean" if not d["dec"] and not gset else ("mixed" if new and miss else
               ("new_only" if new else ("model_miss" if miss else "agrees")))
        rec = {"id": r["id"], "set": r["set"], "src": src, "gold_edits": gold,
               "model_errors": d["dec"], "consensus": cons, "model_new": len(new), "model_miss": miss,
               "gate_cands": cands[r["id"]], "tier": tier,
               "provenance": {"labeler": MODEL, "prompt_sha": phash, "gate_tau": TAU,
                              "recipe": RECIPE, "version": VER, "at": int(time.time())}}
        by_set.setdefault(r["set"], []).append(rec)
        stat["rows"] += 1
        stat["chars"] += len(src)
        stat["consensus"] += cons
        stat["model_new"] += len(new)
        stat["model_miss"] += miss
        stat["auto_clean"] += tier == "auto_clean"
        if new:
            queue += [{"id": r["id"], "set": r["set"], "src": src, **e} for e in new]
    for s, recs in by_set.items():
        with open(os.path.join(OUT, f"{s}_{VER.replace(".", "")}.jsonl"), "w", encoding="utf-8") as f:
            for rec in recs:
                f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    with open(os.path.join(OUT, "review_queue_new_errors.jsonl"), "w", encoding="utf-8") as f:
        for q in queue:
            f.write(json.dumps(q, ensure_ascii=False) + "\n")
    json.dump(stat, open(os.path.join(OUT, "stats.json"), "w"), ensure_ascii=False, indent=1)
    open(os.path.join(OUT, "README.md"), "w", encoding="utf-8").write(
        f"""# dataset_v2（派生数据集，位于仓库外）

配方：门控（102M 字级 p_err, τ={TAU}）定位 → LLM 判定（可答「无错」）。
标注器 {MODEL}，prompt sha {phash}，产出时间 {time.strftime('%F %T')}。

## 三条必须遵守的规则
1. **污染隔离**：该标注器的训练数据不可审计，几乎必然见过 CSCD-NS/NaCGEC 等公开集。
   因此 **v2 不得用于评测 {MODEL} 或同族模型**；用于评测其它系统时也要注明。
2. **provenance 不可丢**：每行 `provenance.labeler/prompt_sha` 与 `tier` 必须随数据传递；
   `model_errors` 不是人标。
3. **新增项必须抽检**：`review_queue_new_errors.jsonl` 是模型提出、金集没有的标注，
   按每桶 ≥58 条抽检后才能升级为可依赖标签（与 zh_route_1k 同一纪律）。
4. **gold 是参照不是裁决**：目标是构建真正的 gold——共识部分直接入；
   模型新增按桶抽检通过后入；gold-only 且模型反复否的进灰区人裁，
   **不得默认整段照搬 gold**（gold 自身噪声由该分诊流程剥离）。

## 计数
版本 {VER} · 行 {stat['rows']:,} · 字 {stat['chars']:,} · 共识 {stat['consensus']:,} · 解析失败行 {stat['parse_fail']:,} ·
模型新增 {stat['model_new']:,} · 模型漏标 {stat['model_miss']:,} · 门控判无需处理 {stat['auto_clean']:,}
类型分布 {stat['type']}
""")
    log(f"      写出 {len(by_set)} 个 jsonl → {OUT}")
    log(f"      共识 {stat['consensus']:,} · 新增 {stat['model_new']:,}（队列待抽检）· 漏标 {stat['model_miss']:,} · "
        f"自动判无需处理 {stat['auto_clean']:,}")
    log(f"      类型分布 {stat['type']}")


if __name__ == "__main__":
    main()
