#!/usr/bin/env python3
"""n-gram 背景表 A/B 验收器（EVOLUTION §13 #1 规定的口径）。

背景：「灰区率下降」不等于收益。实测（§12.1）任何外部干净语料扩表都只是把
clean 与 err 的 OOV **同步下移**——成本降、漏检同步升，判别比反而变差
（cscdns 母语判别比 3:1 → 1.2:1）。因此换表/扩表提案必须看两个量：

  ① 本脚本：原生集 OOV p50 / 灰区率 / **AUC(clean vs err)** /
     **等安全线放行率**（τ = err 分数第 5 百分位，即保留 95% 错误不漏时 clean 放行比例）
  ② `eval_l3_funnel.py`（配 `L3_NGRAM_TABLE` / `L3_BIGRAM_TABLE`）：MuCGEC FN 与
     真实用户文本门控段的 clean/病句放行比

判定线：AUC 不升 **或** 等安全线放行率增幅 < 5pp → 拒绝（只是移动工作点）。

用法（成对给「trigram 表 bigram 表 标签」，可多组）：
    ./.venv/bin/python eval_ngram_table_ab.py \
        ngram_table.json bigram_table_zh.json 现表 \
        /tmp/tbl_3M.json /tmp/tblbg_3M.json +midu@3M
"""
from __future__ import annotations

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
DS = os.path.join(REPO, 'datasets')
sys.path.insert(0, HERE)
from build_ngram_table import oov_ratio, zh_chars  # noqa: E402

NATIVE = ('cscdns_gold.json', 'midu_gold.json', 'ctc21_gold.json', 'cctc_gold.json')


def auc(clean: list[float], err: list[float]) -> float:
    """Mann-Whitney AUC：OOV 能否把 clean（低）与 err（高）分开。0.5 = 无信号。"""
    if not clean or not err:
        return float('nan')
    allv = sorted(clean + err)
    rank: dict[float, float] = {}
    i = 0
    while i < len(allv):
        j = i
        while j < len(allv) and allv[j] == allv[i]:
            j += 1
        for k in range(i, j):
            rank[allv[k]] = (i + j - 1) / 2.0 + 1
        i = j
    n1, n2 = len(clean), len(err)
    return (sum(rank[v] for v in clean) - n1 * (n1 + 1) / 2.0) / (n1 * n2)


def load_groups():
    groups = {}
    for nm in NATIVE:
        p = os.path.join(DS, nm)
        if not os.path.isfile(p):
            continue
        rows = json.load(open(p, encoding='utf-8'))
        key = nm.replace('_gold.json', '')
        groups[key] = {
            'clean': [r['src'] for r in rows if not r['edits'] and len(zh_chars(r['src'])) >= 5],
            'err': [r['src'] for r in rows if r['edits'] and len(zh_chars(r['src'])) >= 5],
            'all': [r['src'] for r in rows if len(zh_chars(r['src'])) >= 5],
        }
    conf = os.path.join(HERE, 'l3_data', 'zh', 'confirmed.jsonl')
    if os.path.isfile(conf):
        groups['confirmed'] = {
            'clean': [json.loads(ln).get('text', '') for ln in open(conf, encoding='utf-8')
                      if ln.strip()],
            'err': [], 'all': []}
        groups['confirmed']['all'] = groups['confirmed']['clean']
    muc = os.path.join(DS, 'mucgec_gold.json')
    if os.path.isfile(muc):
        sents = [e['src'] for e in json.load(open(muc, encoding='utf-8'))]
        groups['mucgec(学习者)'] = {'clean': [], 'err': [], 'all': sents}
    return groups


def main() -> None:
    args = sys.argv[1:]
    if len(args) < 3 or len(args) % 3:
        sys.exit(__doc__)
    groups = load_groups()
    for i in range(0, len(args), 3):
        tp, bp, label = args[i:i + 3]
        table = json.load(open(tp, encoding='utf-8'))
        bg = set(json.load(open(bp, encoding='utf-8')))
        print(f'\n=== {label}：trigram {len(table)} / bigram {len(bg)} ===')
        print(f"{'组':12}{'句':>6}{'OOV p50':>9}{'灰区≥.30':>10}{'AUC':>7}{'放行率@95%召回':>16}")
        for k, g in groups.items():
            f = lambda s: oov_ratio(s, table, bg, weighted=True)  # noqa: E731
            allr = [f(s) for s in g['all']]
            if not allr:
                continue
            p50 = sorted(allr)[len(allr) // 2]
            gray = sum(1 for r in allr if r >= 0.30) / len(allr)
            cl = [f(s) for s in g['clean']]
            er = [f(s) for s in g['err']]
            if er and cl:
                tau = sorted(er)[int(0.05 * len(er))]
                rel = sum(1 for v in cl if v < tau) / len(cl)
                print(f'{k:12}{len(allr):>6}{p50:>9.4f}{gray:>10.1%}{auc(cl, er):>7.3f}{rel:>16.1%}')
            else:
                print(f'{k:12}{len(allr):>6}{p50:>9.4f}{gray:>10.1%}{"—":>7}{"—":>16}')
    print('\n判定：AUC 不升 或 等安全线放行率增幅 <5pp → 拒绝（只是移动成本↔漏检工作点）。'
          '\n还须跑：L3_NGRAM_TABLE=… L3_BIGRAM_TABLE=… python3 eval_l3_funnel.py（看 MuCGEC FN）')


if __name__ == '__main__':
    main()
