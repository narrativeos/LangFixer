#!/usr/bin/env python3
"""零样本生成式订正差分探针：网关 LLM「改写整句」后与原文做字级 diff。
D 侧：synthD 漏字边界召回；干净侧：FP/句率与 insert 型 FP。key 走启动 env，不落盘。
用法: LANGFIXER_LLM_KEY=… python3 /tmp/probe_gendiff.py [--limit N]"""
import argparse, difflib, json, os, time, urllib.request
from concurrent.futures import ThreadPoolExecutor

BASE = os.environ.get("LANGFIXER_LLM_BASE", "http://192.168.1.130:4000/v1")
KEY = os.environ.get("LANGFIXER_LLM_KEY", "")
MODEL = os.environ.get("LANGFIXER_LLM_MODEL", "Qwen3.8")
SYS = "你是中文文本校对器。只修正句子中的错字与缺失字，除此之外一字不改：不增删标点，不改写措辞，不调整语序。直接输出修正后的句子，禁止任何解释。"

def call(t, timeout=120, tries=3):
    body = {"model": MODEL, "messages": [{"role": "system", "content": SYS},
            {"role": "user", "content": "文本纠错：\n" + t}],
            "temperature": 0, "max_tokens": 512,
            "chat_template_kwargs": {"enable_thinking": False}}
    last = None
    for a in range(tries):
        try:
            req = urllib.request.Request(BASE.rstrip("/") + "/chat/completions",
                data=json.dumps(body).encode(),
                headers={"Authorization": "***" + KEY, "content-type": "application/json"})
            d = json.load(urllib.request.urlopen(req, timeout=timeout))
            return (d["choices"][0]["message"].get("content") or "").strip()
        except Exception as e:
            last = e; time.sleep(min(30, 2 ** a))
    raise last

def diff_ops(a, b):
    return [{"op": tag, "i1": i1, "i2": i2, "a": a[i1:i2], "b": b[j1:j2]}
            for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(a=a, b=b, autojunk=False).get_opcodes()
            if tag != "equal"]

def ins_pos(a, b):
    """difflib(src→clean) 首个 insert/replace 的插入位与参考字（edits[].span 坐标系不可用）。"""
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(a=a, b=b, autojunk=False).get_opcodes():
        if tag in ("insert", "replace"):
            return i1, b[j1:j2]
    return 0, ""

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--synth", default="/tmp/synthD_nonde.jsonl")
    ap.add_argument("--clean", action="append", default=["/tmp/clean_law.txt", "/tmp/clean_gold.txt"])
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--out", default="/tmp/probe_gendiff_out.jsonl")
    args = ap.parse_args()
    if not KEY:
        raise SystemExit("需 LANGFIXER_LLM_KEY")

    fw = open(args.out, "w", encoding="utf-8")
    rows = [json.loads(l) for l in open(args.synth, encoding="utf-8") if l.strip()][:args.limit or None]
    with ThreadPoolExecutor(args.workers) as ex:
        outs = list(ex.map(lambda r: call(r["src"]), rows))
    n_exact = n_bound = 0
    for r, o in zip(rows, outs):
        clean = r["synth"]["clean"]; ref = r["edits"][0]["ref"]; p, _c = ins_pos(r["src"], clean)
        ops = diff_ops(r["src"], o)
        bound = any(op["op"] in ("insert", "replace") and abs(op["i1"] - p) <= 2 and ref[0] in op["b"] for op in ops)
        exact = o == clean; n_exact += exact; n_bound += bound
        fw.write(json.dumps({"set": "synthD", "src": r["src"], "clean": clean, "out": o,
                             "span": p, "ref": ref, "exact": exact, "bound_ins": bound, "ops": ops}, ensure_ascii=False) + "\n")
    print(f"[synthD] n={len(rows)} exact={n_exact} boundary_ins={n_bound}", flush=True)

    for cp in args.clean:
        lines = [l.strip() for l in open(cp, encoding="utf-8") if l.strip()][:args.limit or None]
        with ThreadPoolExecutor(args.workers) as ex:
            outs = list(ex.map(call, lines))
        n_ch = n_ins = 0
        for s, o in zip(lines, outs):
            ops = diff_ops(s, o)
            ins_fp = any(op["op"] == "insert" or (op["op"] == "replace" and len(op["b"]) > len(op["a"])) for op in ops)
            n_ch += bool(ops); n_ins += ins_fp
            if ops:
                fw.write(json.dumps({"set": os.path.basename(cp), "src": s, "out": o, "ops": ops, "ins_fp": ins_fp}, ensure_ascii=False) + "\n")
        print(f"[clean {os.path.basename(cp)}] n={len(lines)} changed={n_ch} ins_fp={n_ins}", flush=True)
    fw.close()

if __name__ == "__main__":
    main()
