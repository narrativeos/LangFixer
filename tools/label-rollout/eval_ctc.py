#!/usr/bin/env python3
"""CTC 生成式订正实测：D 侧（synthD 漏字边界召回）+ 干净侧 FP。
判据：D 坐实达标（句级 exact 或边界插入命中）+ 干净 FP/句率极低。"""
import argparse, difflib, json, os
import torch
from transformers import AutoModelForCausalLM, AutoTokenizer

def load_clean(path):
    with open(path, encoding="utf-8") as f:
        return [ln.strip() for ln in f if ln.strip()]

def diff_ops(a, b):
    out = []
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(a=a, b=b, autojunk=False).get_opcodes():
        if tag != "equal":
            out.append({"op": tag, "i1": i1, "i2": i2, "a": a[i1:i2], "b": b[j1:j2]})
    return out

@torch.no_grad()
def gen(model, tok, texts, batch):
    outs = []
    for i in range(0, len(texts), batch):
        b = texts[i:i+batch]
        msgs = [[{"role": "user", "content": "文本纠错：\n" + s}] for s in b]
        inp = tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=True)
        enc = tok(inp, return_tensors="pt", padding=True, add_special_tokens=False).to(model.device)
        out = model.generate(**enc, max_new_tokens=256, do_sample=False, repetition_penalty=1.08,
                             pad_token_id=tok.pad_token_id)
        for k in range(len(b)):
            outs.append(tok.decode(out[k][enc["input_ids"].shape[1]:], skip_special_tokens=True).strip())
        print(f"  gen {min(i+batch, len(texts))}/{len(texts)}", flush=True)
    return outs

def ins_pos(a, b):
    """difflib(src→clean) 首个插入位（edits[].span 坐标系与 src 不符，不可用）。"""
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(a=a, b=b, autojunk=False).get_opcodes():
        if tag in ("insert", "replace"):
            return i1
    return 0

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default=os.path.expanduser("~/tmp/models/ctc-1.5b"))
    ap.add_argument("--synth", default="/tmp/synthD_nonde.jsonl")
    ap.add_argument("--clean", action="append", default=[])
    ap.add_argument("--batch", type=int, default=8)
    ap.add_argument("--out", default="/tmp/eval_ctc_out.jsonl")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--skip-synth", action="store_true", help="跳过 synthD 阶段，只跑干净侧")
    args = ap.parse_args()

    tok = AutoTokenizer.from_pretrained(args.model)
    if tok.pad_token is None:
        tok.pad_token = tok.eos_token
    tok.padding_side = "left"
    model = AutoModelForCausalLM.from_pretrained(args.model, dtype=torch.bfloat16).to("mps")
    model.eval()

    fw = open(args.out, "w", encoding="utf-8")

    if not args.skip_synth:
        rows = [json.loads(ln) for ln in open(args.synth, encoding="utf-8") if ln.strip()]
        if args.limit:
            rows = rows[:args.limit]
        outs = gen(model, tok, [r["src"] for r in rows], args.batch)
        n_exact = n_bound = 0
        for r, o in zip(rows, outs):
            clean = r["synth"]["clean"]; p = ins_pos(r["src"], clean); ref = r["edits"][0]["ref"]
            ops = diff_ops(r["src"], o)
            bound = any(op["op"] in ("insert", "replace") and abs(op["i1"] - p) <= 2
                        and ref and ref[0] in op["b"] for op in ops)
            exact = (o == clean)
            n_exact += exact; n_bound += bound
            fw.write(json.dumps({"set": "synthD", "src": r["src"], "clean": clean, "out": o,
                                 "span": p, "ref": ref, "exact": exact, "bound_ins": bound,
                                 "ops_src_out": diff_ops(r["src"], o)}, ensure_ascii=False) + "\n")
        print(f"[synthD] n={len(rows)} exact_recall={n_exact}/{len(rows)} boundary_ins={n_bound}/{len(rows)}", flush=True)

    for cp in args.clean:
        lines = load_clean(cp)
        if args.limit:
            lines = lines[:args.limit]
        outs = gen(model, tok, lines, args.batch)
        n_changed = n_ins_fp = 0
        for s, o in zip(lines, outs):
            ops = diff_ops(s, o)
            changed = bool(ops)
            ins_fp = any(op["op"] == "insert" or (op["op"] == "replace" and len(op["b"]) > len(op["a"])) for op in ops)
            n_changed += changed; n_ins_fp += ins_fp
            if changed:
                fw.write(json.dumps({"set": os.path.basename(cp), "src": s, "out": o,
                                     "ops": ops, "ins_fp": ins_fp}, ensure_ascii=False) + "\n")
        print(f"[clean {os.path.basename(cp)}] n={len(lines)} changed={n_changed} ins_fp={n_ins_fp}", flush=True)

    fw.close()

if __name__ == "__main__":
    main()
