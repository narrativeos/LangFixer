#!/usr/bin/env python3
"""灰区 ③ 协议 A/B：判定协议（生产缓存）vs 订正差分协议（网关现跑）。

语料：/tmp/gray380.jsonl——MuCGEC funnel 缓存中 llm_ran=true 的 380 句
（= ② 在 MuCGEC 上真实交给 ③ 的灰区集），每行带 gold 编辑与判定臂预测。
差分臂：同一批句、同模型（默认 qwen3.8-27b，与缓存 ③ 臂一致）、无 ② 候选，
只换协议：输出订正句 → 字级 difflib 差分 → 预测编辑。

打分口径（与生产 cat_rule 兼容并补漏字盲区）：
- span 统一为 UTF-8 字节坐标（make_gold_zh.py gold 与 Rust 预测同口径）；
  差分臂字级 difflib 经字→字节前缀和映射到字节坐标；
- I 编辑（零宽 gold）命中：预测编辑在 gold 位 ±2 内且 ref 非空；分 content/punct；
- S 编辑命中：宽度>0 的预测编辑 span 重叠（生产口径）；
- D 编辑命中：重叠且净减字（ref=="" 或 len(ref)<len(src)）；
- FP：不与任何 gold 编辑（含零宽，±1 容差）重叠的预测编辑；
  wide 口径含零宽插入，prod 口径只计宽度>0。

用法: python3 eval_diff_protocol.py [--in /tmp/gray380.jsonl] [--model qwen3.8-27b]
缓存断点续跑于 --cache（默认 /tmp/diff_arm_cache.jsonl）。
"""
import argparse, difflib, json, os, time, urllib.request
from concurrent.futures import ThreadPoolExecutor

BASE = os.environ.get("LANGFIXER_LLM_BASE", "http://192.168.1.130:4000/v1")
SYS = ("你是中文文本校对器。只修正句子中的错字与缺失字，除此之外一字不改："
       "不增删标点，不改写措辞，不调整语序。直接输出修正后的句子，禁止任何解释。")

def get_key():
    k = os.environ.get("LANGFIXER_LLM_KEY", "")
    if k:
        return k
    for p in ("langfixer-settings.json",
              os.path.expanduser("~/github/narrativeos/LangFixer/langfixer-settings.json")):
        try:
            return json.load(open(p))["llm"]["api_key"]
        except Exception:
            pass
    raise SystemExit("无网关 key")

KEY = get_key()

def call(model, t, timeout=240, tries=5):
    body = {"model": model, "messages": [{"role": "system", "content": SYS},
            {"role": "user", "content": "文本纠错：\n" + t}],
            "temperature": 0, "max_tokens": 512,
            "chat_template_kwargs": {"enable_thinking": False}}
    last = None
    for a in range(tries):
        try:
            req = urllib.request.Request(BASE.rstrip("/") + "/chat/completions",
                data=json.dumps(body).encode(),
                headers={"Authorization": ("Bearer ") + KEY, "content-type": "application/json"})
            d = json.load(urllib.request.urlopen(req, timeout=timeout))
            return (d["choices"][0]["message"].get("content") or "").strip()
        except Exception as e:
            last = e
            code = getattr(e, "code", None)
            time.sleep(20 if code == 429 else min(30, 2 ** a))
    raise last

def char_ops(a, b):
    """src→out 字级 diff → 预测编辑（span 为 src 字节坐标，与 gold/judge 同口径）。"""
    cum = [0]
    for ch in a:
        cum.append(cum[-1] + len(ch.encode("utf-8")))
    out = []
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(a=a, b=b, autojunk=False).get_opcodes():
        if tag == "equal":
            continue
        out.append({"span": [cum[i1], cum[i2]], "src": a[i1:i2], "ref": b[j1:j2],
                    "op": "I" if tag == "insert" and i1 == i2 else ("D" if tag == "delete" else "S")})
    return out

PUNCT = set("，。、；：\u201c\u201d''（）《》？！,. ")

def klass(e):
    a, b = e["span"]
    if a == b and not e["src"]:
        return "I-punct" if e["ref"] and all(ch in PUNCT for ch in e["ref"]) else "I-content"
    if e.get("ref"):
        return "S"
    return "D"

def hit(p, g, k):
    pa, pb = p["span"]; ga, gb = g["span"]
    if k == "I-punct" or k == "I-content":
        return pa <= ga + 2 and pb >= ga - 2 and p["ref"]
    return pa < gb + 1 and ga < pb + 1

def fp_hit(p, golds):
    pa, pb = p["span"]
    for g in golds:
        ga, gb = g["span"]
        if pa < gb + 1 and ga < pb + 1:
            return True
        if ga == gb and pa <= ga + 1 and pb >= ga - 1:
            return True
        if pa == pb and ga <= pa + 1 and gb >= pa - 1:
            return True
    return False

def score(preds, golds):
    """preds/golds: [{'span','src','ref'}]。返回各类 (hit,total) 与 FP。"""
    r = {}
    for g in golds:
        k = klass(g)
        h = r.setdefault(k, [0, 0])
        h[1] += 1
        if any(hit(p, g, k) for p in preds):
            h[0] += 1
    # 句级 I 命中
    igolds = [g for g in golds if klass(g) == "I-content"]
    if igolds:
        h = r.setdefault("I-content-sent", [0, 0])
        h[1] += 1
        if any(any(hit(p, g, "I-content") for p in preds) for g in igolds):
            h[0] += 1
    nfp = nfpw = 0
    for p in preds:
        if not fp_hit(p, golds):
            nfpw += 1
            if p["span"][1] > p["span"][0]:
                nfp += 1
    return r, nfp, nfpw

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp", default="/tmp/gray380.jsonl")
    ap.add_argument("--model", default=os.environ.get("LANGFIXER_LLM_MODEL", "qwen3.8-27b"))
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--cache", default="/tmp/diff_arm_cache.jsonl")
    ap.add_argument("--report", default="")
    args = ap.parse_args()

    rows = [json.loads(l) for l in open(args.inp, encoding="utf-8") if l.strip()]
    done = {}
    if os.path.exists(args.cache):
        for l in open(args.cache, encoding="utf-8"):
            r = json.loads(l)
            done[r["text"]] = r["out"]
    todo = [r for r in rows if r["text"] not in done]
    print(f"[1/2] 差分臂生成 {len(rows)} 句（缓存 {len(rows)-len(todo)}，待跑 {len(todo)}，模型 {args.model}）", flush=True)
    fw = open(args.cache, "a", encoding="utf-8")
    def job(r):
        o = call(args.model, r["text"])
        fw.write(json.dumps({"text": r["text"], "out": o}, ensure_ascii=False) + "\n")
        fw.flush()
        done[r["text"]] = o
        return o
    if todo:
        with ThreadPoolExecutor(args.workers) as ex:
            list(ex.map(job, todo))
    fw.close()

    agg = {}
    fps = {}
    arms = [("判定臂funnel27b", "judge_a"), ("判定臂Qwen3.8", "judge_b"), ("差分臂Qwen3.8", None)]
    for name, key in arms:
        agg[name] = {}; fps[name] = [0, 0]
    for r in rows:
        golds = r["gold"]
        preds_by_arm = {
            "判定臂funnel27b": [{"span": p["span"], "src": p["src"], "ref": p["ref"]} for p in r.get("judge_a", [])],
            "判定臂Qwen3.8": [{"span": p["span"], "src": p["src"], "ref": p["ref"]} for p in r.get("judge_b", [])],
            "差分臂Qwen3.8": char_ops(r["text"], done[r["text"]]),
        }
        for name, preds in preds_by_arm.items():
            k, nfp, nfpw = score(preds, golds)
            for kk, (h, t) in k.items():
                a = agg[name].setdefault(kk, [0, 0]); a[0] += h; a[1] += t
            fps[name][0] += nfp; fps[name][1] += nfpw
    lines = [f"# 灰区 ③ 协议 A/B（MuCGEC llm_ran 380 句，差分臂 {args.model}）", "",
             "| 指标 | " + " | ".join(n for n, _ in arms) + " |",
             "|---" * (len(arms) + 1) + "|"]
    for kk in ["I-content-sent", "I-content", "I-punct", "S", "D"]:
        cells = []
        for name, _ in arms:
            v = agg[name].get(kk, [0, 0])
            cells.append(f"{v[0]}/{v[1]}={v[0]/v[1]:.1%}" if v[1] else "-")
        lines.append(f"| {kk} 召回 | " + " | ".join(cells) + " |")
    lines.append("| FP wide（含零宽插入） | " + " | ".join(str(fps[n][1]) for n, _ in arms) + " |")
    lines.append("| FP prod（宽度>0） | " + " | ".join(str(fps[n][0]) for n, _ in arms) + " |")
    lines.append(f"\n句数 {len(rows)}；缓存 {args.cache}")
    rep = "\n".join(lines)
    print(rep)
    if args.report:
        with open(args.report, "w", encoding="utf-8") as f:
            f.write(rep + "\n")

if __name__ == "__main__":
    main()
