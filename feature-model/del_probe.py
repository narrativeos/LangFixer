#!/usr/bin/env python3
"""漏字盲区两步探针（取证脚本，判决已收口见 tools/label-rollout/README.md
「D 类取证判决」§5：通道不成立，FP≫收益；保留供后续方案对照）：
step1 门控 p_err 在删字边界上的召回 vs 干净句 FP；
step2 虚词插入重打分：p_err 显著回落 → 漏字「确认」（实测对别字也成立，非特异）。
用法：--clean 干净句文件 --d-rows 合成 D 行 jsonl --n 取样上限；
干净参照必须用真净句（如 datasets/cscd/test_*.txt wrong==correct 行，
或 cscdns_gold.json edits==[] 行），注意文体域会主导 FP 水位。
"""
import argparse, json, os, sys, statistics
import torch
from transformers import AutoTokenizer, AutoModelForPreTraining

M = os.path.join(os.path.dirname(os.path.abspath(__file__)), "models", "char_ctc_zh")
FUNC = list("的地得着过了在为并而或把被让给就也")
dev = torch.device("mps" if torch.backends.mps.is_available() else "cpu")
tok = AutoTokenizer.from_pretrained(M)
model = AutoModelForPreTraining.from_pretrained(M).to(dev).eval()
HAN = lambda c: "\u4e00" <= c <= "\u9fff"


def perr(text):
    enc = tok(text, return_tensors="pt", truncation=True, max_length=126,
              return_offsets_mapping=True)
    om = enc.pop("offset_mapping")[0].tolist()
    enc = {k: v.to(dev) for k, v in enc.items()}
    with torch.no_grad():
        p = torch.sigmoid(model(**enc).logits.float())[0]
    out = [0.0] * len(text)
    for (lo, hi), pv in zip(om, p.tolist()):
        for i in range(lo, hi):
            if i < len(out):
                out[i] = max(out[i], pv)
    return out


def perr_batch(texts, boundary):
    """返回每条在 boundary 左右字符上的 max p_err。"""
    res = []
    for i in range(0, len(texts), 48):
        enc = tok(texts[i:i + 48], padding=True, return_tensors="pt", truncation=True,
                  max_length=126, return_offsets_mapping=True)
        om = enc.pop("offset_mapping").tolist()
        enc = {k: v.to(dev) for k, v in enc.items()}
        with torch.no_grad():
            p = torch.sigmoid(model(**enc).logits.float())
        for b in range(p.shape[0]):
            row = [0.0] * len(texts[i + b])
            for (lo, hi), pv in zip(om[b], p[b].tolist()):
                for j in range(lo, min(hi, len(row))):
                    row[j] = max(row[j], pv)
            res.append(max(row[boundary - 1:boundary + 1]))
    return res


def b2c(s, b):
    return len(s.encode()[:b].decode("utf-8", "ignore"))


def main():
    ap = argparse.ArgumentParser(description="漏字盲区两步探针（取证用）")
    ap.add_argument("--clean", default="/tmp/clean_gold.txt", help="干净句一行一句")
    ap.add_argument("--d-rows", default="/tmp/synthD_nonde.jsonl", help="synth D 行 jsonl")
    ap.add_argument("--n", type=int, default=400, help="clean 取样上限")
    a = ap.parse_args()
    clean = [l.strip() for l in open(a.clean, encoding="utf-8")][:a.n]
    clean = [s for s in clean if len(s) <= 110]
    drows = [json.loads(l) for l in open(a.d_rows, encoding="utf-8")]
    print(f"clean {len(clean)} 句 · D {len(drows)} 行 · dev={dev}")

    # D 侧 gold 边界
    dgold = []
    for r in drows:
        e = r["edits"][0]
        lo = b2c(r["src"], e["span"][0])
        hi = b2c(r["src"], e["span"][1])
        cand = [p for p in (lo, hi, lo + 1, hi + 1) if 1 <= p < len(r["src"])]
        dgold.append((r["src"], cand[0] if cand else 1))

    # step1：每句一次前向，边界 max(p_err[i-1], p_err[i])
    def scan(text):
        p = perr(text)
        b = [i for i in range(1, len(text)) if HAN(text[i - 1]) and HAN(text[i])]
        return p, {i: max(p[i - 1], p[i]) for i in b}

    clean_sc = [scan(s) for s in clean]
    d_sc = [scan(s) for s, _ in dgold]

    for th in (0.2, 0.3, 0.5, 0.7):
        cf = sum(1 for _, f in clean_sc if any(v >= th for v in f.values()))
        cb = sum(1 for _, f in clean_sc for v in f.values() if v >= th)
        rec = sum(1 for (src, gb), (_, f) in zip(dgold, d_sc) if any(f.get(i, 0) >= th for i in (gb - 1, gb, gb + 1)))
        print(f"θ={th}: clean 触发句 {cf}/{len(clean)} 边界 {cb} · D gold边界召回 {rec}/{len(dgold)}")

    dmed = statistics.median(max(f.values() or [0.0]) for _, f in d_sc) if d_sc else 0
    print(f"D 句内边界 p_err max 中位: {dmed:.3f} | clean: "
          f"{statistics.median(max(f.values() or [0.0]) for _, f in clean_sc):.3f}")

    # step2：对 step1(θ=0.3) 触发的 (句,边界) 做虚词插入确认；D 侧另测 θ=0.2 + gold±1
    cands = []
    for (src, gb), (_, f) in zip(dgold, d_sc):
        if any(f.get(i, 0) >= 0.3 for i in (gb - 1, gb, gb + 1)):
            for i in (gb - 1, gb, gb + 1):
                if 1 <= i < len(src) and f.get(i, 0) >= 0.3:
                    cands.append((src, i, "D"))
    for s, (_, f) in zip(clean, clean_sc):
        for i, v in f.items():
            if v >= 0.3:
                cands.append((s, i, "C"))
    dcands02 = []
    for (src, gb), (_, f) in zip(dgold, d_sc):
        for i in (gb - 1, gb, gb + 1):
            if 1 <= i < len(src) and 0.2 <= f.get(i, 0) < 0.3:
                dcands02.append((src, i, "D02"))
    cands += dcands02
    print(f"step2 待确认边界: {sum(1 for x in cands if x[2]=='D')} D(θ.3) · {sum(1 for x in cands if x[2]=='D02')} D(θ.2±1) · {sum(1 for x in cands if x[2]=='C')} clean")

    conf = {"D": 0, "C": 0, "D02": 0}
    detail = {"D": [], "C": [], "D02": []}
    for src, i, tag in cands:
        base = perr_batch([src], i)[0]
        vals = perr_batch([src[:i] + f + src[i:] for f in FUNC], i)
        best = min(vals); bf = FUNC[vals.index(best)]
        if best <= base - 0.25 and best <= 0.2:
            conf[tag] += 1
            detail[tag].append((base, best, bf, src[max(0, i - 6):i] + "▁" + src[i:i + 6]))
    print(f"插入确认: D {conf['D']}/{sum(1 for x in cands if x[2]=='D')} · D(θ.2±1) {conf['D02']}/{len(dcands02)} · cleanFP {conf['C']}/{sum(1 for x in cands if x[2]=='C')}")
    for tag in ("C", "D", "D02"):
        print(f"--- {tag} 确认明细 ---")
        for base, best, bf, ctx in detail[tag]:
            print(f"  base={base:.3f} best={best:.3f} +「{bf}」 {ctx}")


if __name__ == "__main__":
    main()
