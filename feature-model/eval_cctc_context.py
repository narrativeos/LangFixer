"""CCTC 跨句语境损失评测（A/D 定向复查的代价量化）

问题：现行管线按句打分（启发式与 PLA 侧车都是句级）——相对全文打分，
跨句语境下的错误检出损失多大？用 CCTC（跨句母语纠错集）测：
对每条 gold 编辑 span，比较其 span 内字级 PLA 概率最大值：
  full = 整条文本一次打分的 span-max；
  iso  = span 所在句子单独打分的 span-max（模拟现行句级管线）。
context loss = full 可检出（span-max ≥ τ）而 iso 不可检出的比例。

用法（需 char_gate 侧车 detail 模式）：
  ./.venv_char/bin/python char_gate.py --port 4811 &
  ./.venv/bin/python3 eval_cctc_context.py [--url http://127.0.0.1:4811/gate]
"""
import argparse
import json
import re
import statistics
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
SENT_RE = re.compile(r"[^。！？；.!?]+[。！？；.!?]*|\n+")


def post(url, texts):
    req = urllib.request.Request(url, data=json.dumps(
        {'texts': texts, 'detail': True}).encode(),
        headers={'Content-Type': 'application/json'})
    return json.loads(urllib.request.urlopen(req).read())['scores']


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--url', default='http://127.0.0.1:4811/gate')
    args = ap.parse_args()

    rows = json.load(open(HERE.parent / 'datasets' / 'cctc_gold.json', encoding='utf-8'))
    srcs = [str(r['src']) for r in rows]

    # 1) 全文打分
    full = []
    for i in range(0, len(srcs), 32):
        full += post(args.url, srcs[i:i + 32])

    # 2) 句级打分（现行管线口径）：每句单独打分，回填到原文偏移
    sents = {}  # ri -> [(abs_start, stripped_text)]
    jobs = []   # (row_idx, abs_start, sent_text)
    for ri, s in enumerate(srcs):
        lst = []
        for m in SENT_RE.finditer(s):
            t = m.group(0).strip()
            if len(t) < 2:
                continue
            off = m.start() + (len(m.group(0)) - len(m.group(0).lstrip()))
            lst.append((off, t))
            jobs.append((ri, off, t))
        sents[ri] = lst
    scored = []
    for i in range(0, len(jobs), 32):
        scored += post(args.url, [j[2] for j in jobs[i:i + 32]])
    iso_map = {(ri, st): chars for (ri, st, _), d in zip(jobs, scored)
               for chars in [d['chars']]}

    # 3) 每个 gold span（UTF-8 字节偏移 → 字符偏移）：full vs iso span-max
    recs = []
    for ri, r in enumerate(rows):
        fchars = full[ri]['chars']
        for ed in r['edits']:
            s1, s2 = ed['span']
            bts = srcs[ri].encode('utf-8')
            a = len(bts[:s1].decode('utf-8', 'ignore'))
            b = len(bts[:s2].decode('utf-8', 'ignore'))
            if not (0 <= a < b <= len(srcs[ri])) or b - a > 10:
                continue
            fmx = max(fchars[a:min(b, len(fchars))], default=0.0)
            imx, found = 0.0, False
            for st, t in sents[ri]:
                if st <= a and b <= st + len(t):
                    off = a - st
                    imx = max(iso_map[(ri, st)][off:off + (b - a)], default=0.0)
                    found = True
                    break
            if not found:
                continue
            recs.append({'full': fmx, 'iso': imx, 'delta': fmx - imx})

    print(f'gold spans 可比对: {len(recs)}')
    print(f'span-max 均值: full={statistics.mean(r["full"] for r in recs):.4f} '
          f'iso={statistics.mean(r["iso"] for r in recs):.4f} '
          f'delta均值={statistics.mean(r["delta"] for r in recs):.4f}')
    print(f'\nτ      full检出  iso检出  语境损失(full有iso无)')
    for tau in (0.05, 0.10, 0.20, 0.30):
        fd = sum(1 for r in recs if r['full'] >= tau)
        id_ = sum(1 for r in recs if r['iso'] >= tau)
        loss = sum(1 for r in recs if r['full'] >= tau and r['iso'] < tau)
        print(f'{tau:<6} {fd/len(recs):>7.1%} {id_/len(recs):>7.1%} {loss/len(recs):>10.1%}')


if __name__ == '__main__':
    main()
