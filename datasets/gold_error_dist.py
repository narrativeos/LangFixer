#!/usr/bin/env python3
"""错误分布对照器：各 gold 的编辑类型（S/D/I）与「单字替换同音占比」，
以及任意候选语料 `{source,target}` 的同口径分布。

用途（EVOLUTION §13 #2/#4/#6 的判定依据）：判断一个新数据源的**错误生成机制**
是否与真实流量同源。真实流量基准（母语者人工标注）：
  cscdns S 99.4% / D 0.3% / I 0.3%，单字替换同音 80.9%；
  cscd / sig13 / sig14 / med / nlpcc23：S 99–100%，同音 57–71%。
反例（已否定）：midu2022 初赛伪数据 S/D/I=45/32/23、同音 24.5%（OCR/形近 + 增删）；
  CTC2021/train_large_v2 抽样亦为形近合成噪声。

用法：
    ./.venv/bin/python gold_error_dist.py
    ./.venv/bin/python gold_error_dist.py --pairs ~/Downloads/preliminary_train.jsonl.gz
    ./.venv/bin/python gold_error_dist.py --pairs x.jsonl --limit 30000
"""
from __future__ import annotations

import argparse
import difflib
import gzip
import json
import os
from collections import Counter

from pypinyin import lazy_pinyin

HERE = os.path.dirname(os.path.abspath(__file__))
GOLDS = ('cscdns_gold.json', 'nlpcc23_gold.json', 'cctc_gold.json', 'midu_gold.json',
         'ctc21_gold.json', 'sig13_gold.json', 'sig14_gold.json',
         'cscd_gold.json', 'nacgec_gold.json', 'mucgec_gold.json')


def summarize(label: str, pairs, edits=None, n_err=None) -> None:
    """pairs: [(src, tgt)]；edits: gold 的 edits 列表（优先，口径更准）；
    n_err: 有错句数——gold 必须按 edits 判定（clean 句 ref 可能有规范化差异）。"""
    ops = Counter()
    pys = pyd = 0
    if edits is not None:
        for e in edits:
            ops[e['op']] += 1
            if e['op'] == 'S' and len(e['src']) == 1 and len(e['ref']) == 1:
                if lazy_pinyin(e['src'])[0] == lazy_pinyin(e['ref'])[0]:
                    pys += 1
                else:
                    pyd += 1
        if n_err is None:
            n_err = sum(1 for s, t in pairs if s != t)
    else:
        n_err = 0
        for s, t in pairs:
            if not s or not t or s == t:
                continue
            n_err += 1
            for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(
                    None, s, t, autojunk=False).get_opcodes():
                if tag == 'equal':
                    continue
                op = 'S' if tag == 'replace' else ('D' if tag == 'delete' else 'I')
                ops[op] += 1
                if op == 'S' and i2 - i1 == 1 and j2 - j1 == 1:
                    if lazy_pinyin(s[i1])[0] == lazy_pinyin(t[j1])[0]:
                        pys += 1
                    else:
                        pyd += 1
    tot = sum(ops.values()) or 1
    print(f'{label:26} 句{len(pairs):>7} 有错{n_err:>7} 编辑{tot:>7}  '
          f'S {ops["S"]/tot:5.1%}  D {ops["D"]/tot:5.1%}  I {ops["I"]/tot:5.1%}  '
          f'单字替换同音 {pys/max(pys+pyd,1):5.1%}')


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument('--pairs', default='', help='候选语料 .jsonl / .jsonl.gz（{source,target}）')
    ap.add_argument('--limit', type=int, default=30000, help='候选语料最多取多少对')
    args = ap.parse_args()

    print('真实 gold（判定基准）：')
    for nm in GOLDS:
        p = os.path.join(HERE, nm)
        if not os.path.isfile(p):
            continue
        rows = json.load(open(p, encoding='utf-8'))
        base = nm.replace('_gold.json', '')
        summarize(base, [(r['src'], r.get('ref', '')) for r in rows],
                  edits=[e for r in rows for e in r['edits']],
                  n_err=sum(1 for r in rows if r['edits']))
        splits = sorted({r.get('split') for r in rows if r.get('split')})
        for sp in splits:
            if len(splits) < 2:
                break
            sel = [r for r in rows if r.get('split') == sp]
            summarize(f'{base}/{sp}', [(r['src'], r.get('ref', '')) for r in sel],
                      edits=[e for r in sel for e in r['edits']],
                      n_err=sum(1 for r in sel if r['edits']))

    if args.pairs:
        path = os.path.expanduser(args.pairs)
        opener = (lambda p: gzip.open(p, 'rt', encoding='utf-8')) \
            if path.endswith('.gz') else (lambda p: open(p, encoding='utf-8'))
        pairs = []
        with opener(path) as f:
            for ln in f:
                if len(pairs) >= args.limit:
                    break
                ln = ln.strip()
                if not ln.startswith('{'):
                    continue
                r = json.loads(ln)
                pairs.append((str(r.get('source', '')), str(r.get('target', ''))))
        print('\n候选语料（同口径 diff）：')
        summarize(os.path.basename(path), pairs)
        print('\n判定：与真实 gold 的 S 占比差 >20pp 或同音占比差 >20pp → 错误机制不同源，'
              '\n不得进句级训练池 / 不得扩混淆字词表（EVOLUTION §13）。')


if __name__ == '__main__':
    main()
