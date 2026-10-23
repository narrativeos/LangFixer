"""D.2 字级判别（零样本）：Chinese ELECTRA discriminator 的 CTC（correctness
classification）头直接输出每个字「被替换过」的概率——无需微调即为字级异常
检测器。句级分数 = top-k 字概率均值；扫阈值对照启发式门控。

评测：cscdns/nlpcc23/cctc/cscd/nacgec + 母语者原生新集 midu/ctc21/sig13/sig14（原生）
+ MuCGEC gold（学习者）。
对照基线（启发式 0.30）：cscdns clean 放行 3% / 门控可见 97-99%。

用法（需 .venv_char：python3.12 + torch + transformers）：
  ./.venv_char/bin/python eval_char_pla.py [--limit N] [--model NAME]
  # NAME 可为 HF 模型名或本地微调目录（如 models/char_ctc_zh）
"""
import argparse
import json
import sys
import time
from pathlib import Path

import torch
from transformers import AutoModelForPreTraining, AutoTokenizer

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
DATASETS = ['cscdns_gold.json', 'nlpcc23_gold.json', 'cctc_gold.json',
            'cscd_gold.json', 'nacgec_gold.json', 'midu_gold.json',
            'ctc21_gold.json', 'sig13_gold.json', 'sig14_gold.json',
            'mucgec_gold.json']
TOPK = 3


def load_rows():
    rows = []
    for nm in DATASETS:
        p = REPO / 'datasets' / nm
        if not p.is_file():
            continue
        for e in json.load(open(p, encoding='utf-8')):
            if e.get('src'):
                rows.append((nm, e['src'], bool(e.get('edits'))))
    # §I2 holdout 纪律：confirmed 的 md5 稳定 20%（不进任何训练）——只读，用来看
    # 分数分布是否回落（域校准是否泛化），不参与 AUC（无错误侧）
    hp = HERE / 'l3_data' / 'zh' / 'confirmed_holdout.json'
    if hp.is_file():
        for t in json.load(open(hp, encoding='utf-8')):
            if t:
                rows.append(('holdout', t, False))
    return rows


EXTRA_DS = ['holdout']


def p50(xs):
    if not xs:
        return float('nan')
    s = sorted(xs)
    return s[len(s) // 2]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--model', default='hfl/chinese-electra-180g-base-discriminator')
    ap.add_argument('--limit', type=int, default=0)
    ap.add_argument('--batch', type=int, default=32)
    args = ap.parse_args()

    rows = load_rows()
    if args.limit:
        per, seen = {}, {}
        keep = []
        for nm, src, err in rows:
            per[nm] = per.get(nm, 0)
            if per[nm] < args.limit:
                keep.append((nm, src, err))
                per[nm] += 1
        rows = keep
    print(f'sentences: {len(rows)}  model: {args.model}', flush=True)

    tok = AutoTokenizer.from_pretrained(args.model)
    model = AutoModelForPreTraining.from_pretrained(args.model)
    model.eval()
    dev = 'mps' if torch.backends.mps.is_available() else 'cpu'
    model.to(dev)
    print(f'device: {dev}', flush=True)

    scores = []
    per_char_all = []
    t0 = time.time()
    with torch.no_grad():
        for i in range(0, len(rows), args.batch):
            chunk = rows[i:i + args.batch]
            enc = tok([r[1] for r in chunk], padding=True, truncation=True,
                      max_length=128, return_tensors='pt').to(dev)
            out = model(**enc).logits  # (B, L) discriminator logits
            probs = torch.sigmoid(out.float())
            mask = enc['attention_mask'].bool()
            # 去掉 [CLS]/[SEP]；保存逐字 CTC 概率（离线聚合搜索用）
            for b in range(probs.shape[0]):
                p = probs[b][mask[b]][1:-1]
                per_char = [round(float(x), 4) for x in p]
                per_char_all.append(per_char)
                k = min(TOPK, p.numel())
                scores.append(float(torch.topk(p, k).values.mean()))
            if (i // args.batch) % 20 == 0:
                print(f'  {i}/{len(rows)} {time.time()-t0:.0f}s', flush=True)

    # 每数据集 AUC + 阈值扫描
    from collections import defaultdict
    by = defaultdict(lambda: {'c': [], 'e': []})
    for (nm, src, err), s in zip(rows, scores):
        by[nm]['e' if err else 'c'].append(s)

    def auc(c, e):
        if not c or not e:
            return float('nan')
        pts = sorted(c + e)
        import bisect
        sc = sorted(c)
        se = sorted(e)
        tot = 0.0
        for x in se:
            tot += bisect.bisect_left(sc, x) + 0.5 * (bisect.bisect_right(sc, x) - bisect.bisect_left(sc, x))
        return tot / (len(c) * len(e))

    print('\n| dataset | n_clean | n_err | AUC | clean p50 |')
    for nm in DATASETS + EXTRA_DS:
        d = by.get(nm)
        if not d:
            continue
        print(f'| {nm} | {len(d["c"])} | {len(d["e"])} | {auc(d["c"], d["e"]):.3f} '
              f'| {p50(d["c"]):.4f} |')

    # 操作点：clean 放行率 @ 错误可见率（分数低于阈值 → 放行）
    qs = [0.05, 0.1, 0.2, 0.3, 0.5]
    print('\n| dataset | ' + ' | '.join(f'q={q} clean放行/err可见' for q in qs) + ' |')
    for nm in DATASETS + EXTRA_DS:
        d = by.get(nm)
        if not d or not d['c'] or not d['e']:
            continue
        allsc = sorted(d['c'] + d['e'])
        cells = []
        for q in qs:
            thr = allsc[min(len(allsc) - 1, int(q * len(allsc)))]
            cp = sum(1 for x in d['c'] if x < thr) / len(d['c'])
            ev = sum(1 for x in d['e'] if x >= thr) / len(d['e'])
            cells.append(f'{cp:.0%}/{ev:.0%}')
        print(f'| {nm} | ' + ' | '.join(cells) + ' |')

    out = HERE / 'l3_data' / f"char_pla_scores_{args.model.rsplit('/', 1)[-1]}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    json.dump([{'ds': nm, 'src': src, 'len': len(src), 'err': err,
                'score': s, 'chars': pc}
               for (nm, src, err), s, pc in zip(rows, scores, per_char_all)],
              open(out, 'w', encoding='utf-8'), ensure_ascii=False)
    print(f'\nscores → {out}')


if __name__ == '__main__':
    main()
