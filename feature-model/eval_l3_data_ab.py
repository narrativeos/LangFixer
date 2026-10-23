#!/usr/bin/env python3
"""L3 句级门控数据增益三臂对照（EVOLUTION §12.2 的复现器；§13 #2 的判定依据）。

用途：评估「往 L3 训练池加一批新语料」是否值得。三臂（每臂都含全量现有池
`data/zh/train.jsonl`；B/C 各加 CAP 错 + CAP 净，数量对齐）：
  A  baseline          ：仅现有池
  B  +候选 筛同音      ：正例仅「全部编辑均为 1-1 且拼音相同」的句子（对齐真实流量分布）
  C  +候选 不筛        ：正例为任意注入错误（S/D/I 混合，含 OCR 形近与增删）

模型：HistGradientBoostingClassifier（vegaml GBM 的本地代理；三臂同参）。
评测：原生集 clean/err 分开（cscdns / midu / cctc / ctc21）+ 池内 dev.jsonl。
指标：AUC(P(err))；等安全线放行率 = τ 取 err 分数第 5 百分位（保留 95% 错误）时
      clean 中 P<τ 的比例。
注意：特征里的 `ngram_oov` / `rare_bigram` 取当前仓库表——换表后须重跑（缓存自动失效
      需手动删 `L3_AB_CACHE`）。

已定案结果（2026-10-19，候选 = midu2022 初赛 100 万伪数据）：
  A 0.530/5.0% · B 0.545/7.3% · C 0.550/6.9%（cscdns AUC/放行率）→ 增益小、
  筛同音与不筛无差别 → 不进池（§13 #2）。

用法：
    MIDU_PAIRS=~/Downloads/preliminary_train.jsonl.gz L3_AB_CAP=120000 \
      ./.venv/bin/python eval_l3_data_ab.py
"""
from __future__ import annotations

import difflib
import gzip
import hashlib
import json
import os
import pickle
import random
import sys
from collections import Counter

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
DS = os.path.join(REPO, 'datasets')
F = os.environ.get('MIDU_PAIRS', os.path.expanduser('~/Downloads/preliminary_train.jsonl.gz'))
CACHE = os.environ.get('L3_AB_CACHE', '/tmp/l3_ab_cache.pkl')
CAP = int(os.environ.get('L3_AB_CAP', '120000'))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, 'datasets'))

h = lambda s: hashlib.md5(s.encode('utf-8')).digest()  # noqa: E731
zhlen = lambda s: sum(1 for c in s if '\u4e00' <= c <= '\u9fff')  # noqa: E731


def blocklist() -> set[str]:
    sys.path.insert(0, HERE)
    from prepare_l3_pool import load_blocklist
    return load_blocklist()


def build_rows():
    """采样 midu 对（HOM=全同音替换 / OTHER=其余），返回待抽特征的文本清单。"""
    from pypinyin import lazy_pinyin
    block = blocklist()
    seen = set()
    hom, other = [], []
    n_scan = 0
    with gzip.open(F, 'rt', encoding='utf-8') as f:
        for ln in f:
            if len(hom) >= CAP and len(other) >= CAP:
                break
            r = json.loads(ln)
            s, t = str(r.get('source', '')).strip(), str(r.get('target', '')).strip()
            n_scan += 1
            if not s or not t or s == t or 'ued' in s:
                continue
            if zhlen(t) < 10 or zhlen(t) > 100:
                continue
            if t in block or s in block:
                continue
            ht = h(t)
            if ht in seen:
                continue
            seen.add(ht)
            ops = [(tag, i1, i2, j1, j2) for tag, i1, i2, j1, j2
                   in difflib.SequenceMatcher(None, s, t, autojunk=False).get_opcodes()
                   if tag != 'equal']
            ok = bool(ops)
            for tag, i1, i2, j1, j2 in ops:
                if tag != 'replace' or i2 - i1 != 1 or j2 - j1 != 1 \
                        or lazy_pinyin(s[i1])[0] != lazy_pinyin(t[j1])[0]:
                    ok = False
                    break
            if ok:
                if len(hom) < CAP:
                    hom.append((s, t))
            elif len(other) < CAP:
                other.append((s, t))
    print(f'[midu] 扫描 {n_scan} 行 → 同音对 {len(hom)}  其余对 {len(other)}')
    return hom, other


def pool_rows():
    rows = []
    with open(os.path.join(HERE, 'data', 'zh', 'train.jsonl'), encoding='utf-8') as f:
        for ln in f:
            ln = ln.strip()
            if not ln:
                continue
            r = json.loads(ln)
            rows.append((str(r.get('text', '')), int(r.get('label', 0))))
    print(f'[pool] 现有池 {len(rows)} 行，label1 {sum(1 for _, y in rows if y)}')
    return rows


def eval_sets():
    out = {}
    for nm in ('cscdns_gold.json', 'midu_gold.json', 'cctc_gold.json', 'ctc21_gold.json'):
        rows = json.load(open(os.path.join(DS, nm), encoding='utf-8'))
        key = nm.replace('_gold.json', '')
        out[key] = [(r['src'], 0 if not r['edits'] else 1) for r in rows if zhlen(r['src']) >= 8]
    dev = []
    with open(os.path.join(HERE, 'data', 'zh', 'dev.jsonl'), encoding='utf-8') as f:
        for ln in f:
            ln = ln.strip()
            if ln:
                r = json.loads(ln)
                dev.append((str(r.get('text', '')), int(r.get('label', 0))))
    out['dev(池内)'] = dev
    return out


_WORKER = {}


def _init_worker():
    sys.path.insert(0, HERE)
    import l3_finetune as L
    _WORKER['L'] = L


def _feat_chunk(texts):
    L = _WORKER['L']
    return [L.extract_features(t, 'zh') for t in texts]


def extract(texts: list[str], nproc: int = 8) -> np.ndarray:
    from multiprocessing import Pool
    chunks = [texts[i:i + 4000] for i in range(0, len(texts), 4000)]
    with Pool(nproc, initializer=_init_worker) as p:
        parts = p.map(_feat_chunk, chunks, chunksize=4)
    return np.asarray([row for part in parts for row in part], dtype=np.float32)


def main():
    rng = random.Random(20261019)
    if os.path.isfile(CACHE):
        with open(CACHE, 'rb') as f:
            d = pickle.load(f)
    else:
        pool = pool_rows()
        hom, other = build_rows()
        evs = eval_sets()
        texts = [t for t, _ in pool] \
            + [s for s, _ in hom] + [t for _, t in hom] \
            + [s for s, _ in other] + [t for _, t in other] \
            + [t for k in evs for t, _ in evs[k]]
        print(f'[feat] 抽取 {len(texts)} 句特征 …')
        X = extract(texts)
        d = {'X': X, 'pool_n': len(pool), 'hom_n': len(hom), 'other_n': len(other),
             'pool_y': [y for _, y in pool],
             'evals': {k: v for k, v in evs.items()}}
        with open(CACHE, 'wb') as f:
            pickle.dump(d, f)
    X, n_pool, n_hom, n_oth = d['X'], d['pool_n'], d['hom_n'], d['other_n']
    yp = np.asarray(d['pool_y'], dtype=np.int8)
    evs = d['evals']

    def sl(a, b):
        return X[a:b]

    i = 0
    X_pool, i = sl(i, i + n_pool), i + n_pool
    X_hs, i = sl(i, i + n_hom), i + n_hom
    X_ht, i = sl(i, i + n_hom), i + n_hom
    X_os, i = sl(i, i + n_oth), i + n_oth
    X_ot, i = sl(i, i + n_oth), i + n_oth
    Xe = {}
    for k, v in evs.items():
        Xe[k], i = sl(i, i + len(v)), i + len(v)

    y_h = np.ones(n_hom, dtype=np.int8)
    y_c = np.zeros(n_hom, dtype=np.int8)
    arms = {
        'A baseline(仅现有池)': (X_pool, yp),
        'B +midu 筛同音': (np.vstack([X_pool, X_hs, X_ht]),
                          np.concatenate([yp, y_h, y_c])),
        'C +midu 不筛': (np.vstack([X_pool, X_os, X_ot]),
                        np.concatenate([yp, np.ones(n_oth, dtype=np.int8),
                                        np.zeros(n_oth, dtype=np.int8)])),
    }

    from sklearn.ensemble import HistGradientBoostingClassifier
    from sklearn.metrics import roc_auc_score

    print(f'\n{"臂":22}{"训练行数":>9}  ' + '  '.join(f'{k:>16}' for k in evs))
    print(' ' * 32 + '  '.join(f'{"AUC / 放行率@95%召回":>18}' for k in evs))
    for name, (Xa, ya) in arms.items():
        m = HistGradientBoostingClassifier(random_state=0, max_iter=250,
                                           learning_rate=0.1)
        m.fit(Xa, ya)
        cells = []
        for k, v in evs.items():
            ys = np.asarray([y for _, y in v], dtype=np.int8)
            p = m.predict_proba(Xe[k])[:, 1]
            auc = roc_auc_score(ys, p) if len(set(ys)) > 1 else float('nan')
            err, cl = p[ys == 1], p[ys == 0]
            if len(err) and len(cl):
                tau = float(np.quantile(err, 0.05))
                rel = float((cl < tau).mean())
            else:
                rel = float('nan')
            cells.append(f'{auc:>8.3f}/{rel:>7.1%}')
        print(f'{name:22}{len(ya):>9}  ' + '  '.join(f'{c:>16}' for c in cells))
    print('\n（AUC=OOV 之外的全特征句级判别；放行率=保留 95% 错误句不漏时 clean 的放行比例）')


if __name__ == '__main__':
    main()

