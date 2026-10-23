"""字级 PLA 模型 A/B 判定器（EVOLUTION §13 验收口径）

读 `eval_char_pla.py` 落盘的 `l3_data/char_pla_scores_<model>.json`（逐句 top-3
字概率均值 + 逐字概率），对两个 checkpoint 做**配对**比较：

- AUC(clean vs err)：判别力
- clean p50：分数分布漂移（域校准是否回落 / 是否只是整体平移）
- **等错误可见率下的 clean 放行率**（thr 由错误侧分位数定，门控语义：
  PLA < thr → 放行）——这是唯一有效验收轴。p50 下降而放行率不升 = 纯平移，判负

判定线：原生集（cscdns/cctc/nlpcc23/midu/ctc21/sig13/sig14）多数档放行率增幅
<2pp 或 AUC 下降 → 该数据/改动拒绝。MuCGEC（学习者）与 holdout（未见生产口语）
为安全/泛化对照，不计入验收轴。

用法：
  ./.venv/bin/python eval_char_pla_ab.py char_ctc_zh_abA char_ctc_zh_abB
  # 参数是 --model 的 basename（或 scores 文件名）；A=基线，B=候选
"""
import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
L3 = HERE / 'l3_data'
NATIVE = ['cscdns_gold.json', 'cctc_gold.json', 'nlpcc23_gold.json',
          'midu_gold.json', 'ctc21_gold.json', 'sig13_gold.json',
          'sig14_gold.json', 'nacgec_gold.json', 'cscd_gold.json']
SAFETY = ['mucgec_gold.json', 'holdout']
VIS = [0.99, 0.97, 0.95, 0.90]


def load(which):
    p = L3 / f'char_pla_scores_{which}.json'
    if not p.is_file():
        cand = L3 / which
        if not cand.is_file():
            sys.exit(f'找不到分数文件：{p}（先跑 eval_char_pla.py --model ...）')
        p = cand
    rows = {}
    for e in json.load(open(p, encoding='utf-8')):
        rows[(e['ds'], e['src'])] = (bool(e['err']), float(e['score']))
    return rows


def auc(clean, err):
    if not clean or not err:
        return float('nan')
    sc, se = sorted(clean), sorted(err)
    import bisect
    tot = 0.0
    for x in se:
        lo = bisect.bisect_left(sc, x)
        tot += lo + 0.5 * (bisect.bisect_right(sc, x) - lo)
    return tot / (len(clean) * len(err))


def p50(xs):
    if not xs:
        return float('nan')
    s = sorted(xs)
    return s[len(s) // 2]


def release(clean, err, vis):
    """thr = 错误侧 (1-vis) 分位 → 错误可见率恰为 vis 时的 clean 放行率。"""
    if not err or not clean:
        return float('nan')
    se = sorted(err)
    idx = min(len(se) - 1, max(0, int(round((1.0 - vis) * len(se)))))
    thr = se[idx]
    return sum(1 for x in clean if x < thr) / len(clean)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('a', help='基线（scores basename）')
    ap.add_argument('b', help='候选（scores basename）')
    args = ap.parse_args()

    ra, rb = load(args.a), load(args.b)
    common = sorted(set(ra) & set(rb))
    print(f'A={args.a}  B={args.b}  配对句对={len(common)}'
          f'（A {len(ra)} / B {len(rb)}）')
    if len(common) < min(len(ra), len(rb)) * 0.9:
        print('⚠ 两模型评测的句子集合差异过大，配对不可信（须同一 eval 批次）')

    by = {}
    for k in common:
        ea, sa = ra[k]
        eb, sb = rb[k]
        if ea != eb:
            continue
        by.setdefault(k[0], {'c': [], 'e': []})
        by[k[0]]['e' if ea else 'c'].append((sa, sb))

    hdr = ' | '.join(f'vis={v:.0%} A→B(Δpp)' for v in VIS)
    print(f'\n| dataset | n | AUC A→B | clean p50 A→B | {hdr} |')
    tot = {v: [] for v in VIS}
    auc_d, p50_d = [], []
    for nm in NATIVE + SAFETY:
        d = by.get(nm)
        if not d:
            continue
        ca = [x for x, _ in d['c']]
        cb = [y for _, y in d['c']]
        ea = [x for x, _ in d['e']]
        eb = [y for _, y in d['e']]
        cells = []
        for v in VIS:
            ra_, rb_ = release(ca, ea, v), release(cb, eb, v)
            if not ea:
                cells.append('—')
                continue
            cells.append(f'{ra_:.1%}→{rb_:.1%}({100*(rb_-ra_):+.1f})')
            if nm in NATIVE:
                tot[v].append(100 * (rb_ - ra_))
        if nm in NATIVE:
            auc_d.append((auc(ca, ea), auc(cb, eb)))
            p50_d.append((p50(ca), p50(cb)))
        print(f'| {nm} | {len(ca)+len(ea)} | {auc(ca,ea):.3f}→{auc(cb,eb):.3f} '
              f'| {p50(ca):.4f}→{p50(cb):.4f} | ' + ' | '.join(cells) + ' |')

    print('\n原生集验收（vis 档 Δpp 均值 / 变差集数）：')
    means = []
    for v in VIS:
        xs = tot[v]
        if not xs:
            continue
        mean = sum(xs) / len(xs)
        worse = sum(1 for x in xs if x < -0.5)
        means.append(mean)
        print(f'  vis={v:.0%}: 平均 {mean:+.2f}pp，明显变差 {worse}/{len(xs)} 集')
    print(f'\n判定线：原生集多数 vis 档平均增幅 <2pp 或 AUC 下降 → 拒绝（§13）；'
          f'p50 变化但放行率不升 = 纯分数平移，同样判负')
    # §13 口径按「多数档」判定，不看单档最好值——单档冲高通常是阈值边界噪声
    good = sum(1 for m in means if m >= 2.0)
    a_up = sum(1 for a, b in auc_d if b > a + 0.002)
    a_dn = sum(1 for a, b in auc_d if b < a - 0.002)
    p_up = sum(1 for a, b in p50_d if b > a * 1.05)
    p_dn = sum(1 for a, b in p50_d if b < a * 0.95)
    best = max(means) if means else float('nan')
    print(f'  档均值 ≥2pp：{good}/{len(means)} 档；原生集 AUC 升 {a_up} / 降 {a_dn}'
          f'（±0.002 噪声带）；clean p50 升 {p_up} / 降 {p_dn}（±5%）')
    if not means:
        print('判定：无法判定（原生集无错误侧样本）')
    elif good * 2 > len(means) and a_dn <= a_up:
        print(f'判定：**通过候选门槛**（多数档 ≥2pp 且 AUC 未净降；最好一档 {best:+.2f}pp）')
    else:
        why = []
        if good * 2 <= len(means):
            why.append(f'多数档 <2pp（{good}/{len(means)}）')
        if a_dn > a_up:
            why.append(f'AUC 净降（{a_dn} 降 / {a_up} 升）')
        tail = f'——单档最好 {best:+.2f}pp 不足以推翻' if best >= 2.0 else ''
        print(f'判定：**拒绝**（{"；".join(why)}）{tail}')


if __name__ == '__main__':
    main()
