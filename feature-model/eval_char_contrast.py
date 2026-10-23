"""句内对比归一实验（G 遗留：PLA 分数域漂移）——负结果记录

动机：绝对 top-3 分数随域漂移（confirmed 口语 p50 0.125 vs cscdns 净
0.079），τ_hi（高分直接灰区）被否决。假设：句内反常对比
（top-3 相对本句中位数）可能域无关。

结论（models/char_ctc_zh v1，2026-10）：
- 对比特征（diff/maxdiff）AUC 无增益（cscdns 0.736/0.752 vs t3 0.737）；
- confirmed 漂移依旧（diff p50 0.116 vs cscdns 净 0.078）——漂移源于
  训练分布（口语碎片在 LM 下确实更"反常"），非分数尺度问题；
- 后处理归一失败 → 正解是域数据进训练（finetune_char_ctc.py
  --confirmed-repeat，v2），τ_hi 待 v2 后重探。

用法：
  # 先跑 eval_char_pla.py --model models/char_ctc_zh 生成逐字分数
  # 再用 char_gate.py detail 模式给 confirmed 补逐字分数（临时脚本）
  ./.venv/bin/python3 eval_char_contrast.py
"""
import bisect
import json
import statistics
from pathlib import Path

import correct

HERE = Path(__file__).resolve().parent
CONF_CHARS = Path('/tmp/confirmed_chars.json')  # conf_detail 临时脚本产物
EPS = 1e-6


def feats(chars):
    if not chars:
        return {}
    s = sorted(chars, reverse=True)
    t3 = sum(s[:3]) / min(3, len(chars))
    med = statistics.median(chars)
    return {'t3': t3, 'diff': t3 - med, 'ratio': t3 / (med + EPS),
            'max': s[0], 'maxdiff': s[0] - med}


def auc(c, e):
    if not c or not e:
        return float('nan')
    sc = sorted(c)
    tot = 0.0
    for x in sorted(e):
        tot += bisect.bisect_left(sc, x) + 0.5 * (
            bisect.bisect_right(sc, x) - bisect.bisect_left(sc, x))
    return tot / (len(c) * len(e))


def main():
    data = json.load(open(HERE / 'l3_data' / 'char_pla_scores_char_ctc_zh.json',
                          encoding='utf-8'))
    conf = json.load(open(CONF_CHARS, encoding='utf-8'))
    table, bg = correct.load_zh_ngram_tables(
        str(HERE / 'ngram_table.json'), str(HERE / 'bigram_table_zh.json'))
    for r in data + conf:
        r.update(feats(r['chars']))
        r['heur'] = correct.zh_heuristic_prob(r['src'], table, bg)

    dss = ['cscdns_gold.json', 'cscd_gold.json', 'nlpcc23_gold.json',
           'cctc_gold.json', 'nacgec_gold.json']
    print('特征 AUC（clean vs err）:')
    print(f"{'feat':8s} " + ' '.join(f'{d.split("_")[0]:>8s}' for d in dss)
          + '   confirmed p50 (cscdns净 p50)')
    for f in ('t3', 'diff', 'ratio', 'maxdiff'):
        aucs = [auc([r[f] for r in data if r['ds'] == ds and not r['err']],
                    [r[f] for r in data if r['ds'] == ds and r['err']])
                for ds in dss]
        cp = statistics.median([r[f] for r in conf])
        np50 = statistics.median([r[f] for r in data
                                  if r['ds'] == 'cscdns_gold.json' and not r['err']])
        print(f'{f:8s} ' + ' '.join(f'{a:8.3f}' for a in aucs)
              + f'   {cp:.4f} ({np50:.4f})')

    # 候选策略：送LLM = (heur灰 且 diff>T_lo) 或 diff>T_hi
    print('\n策略搜索（contrast=diff）：clean放行/err可见')
    for t_lo, t_hi in ((0.0, 9.9), (0.0, 0.55), (0.0, 0.35), (0.05, 0.55)):
        line = []
        for ds in dss:
            c = [r for r in data if r['ds'] == ds and not r['err']]
            e = [r for r in data if r['ds'] == ds and r['err']]
            sent = lambda r, lo=t_lo, hi=t_hi: (r['heur'] >= 0.30 and r['diff'] > lo) or r['diff'] > hi  # noqa: E731
            cp = sum(1 for r in c if not sent(r)) / len(c)
            ev = sum(1 for r in e if sent(r)) / len(e)
            line.append(f'{ds.split("_")[0]} {cp:.0%}/{ev:.1%}')
        cc = sum(1 for r in conf if not (r['heur'] >= 0.30 and r['diff'] > t_lo or r['diff'] > t_hi)) / len(conf)
        print(f'  T_lo={t_lo:<6} T_hi={t_hi:<5} ' + ' '.join(line) + f' | conf {cc:.0%}')


if __name__ == '__main__':
    main()
