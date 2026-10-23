"""F：新 gold 集（CTCDataset）pattern 覆盖 + 门控可见性评测（编辑粒度）。

对 cscdns/nlpcc23/cctc 三个原生评测集的每条 gold 编辑：
- pattern 重叠：任一 PatternHit span 与编辑 span 重叠
- 严格覆盖：重叠且 hit.correction 含编辑 ref（可本地定案）
- 门控可见：编辑所在句 pattern 命中或启发式 p ≥ GRAY_ZONE_LOW（会送 LLM）

用法：./.venv/bin/python3 eval_pattern_coverage.py
"""
import json
import re
import sys
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import correct  # noqa: E402

GRAY = float(sys.argv[1]) if len(sys.argv) > 1 else 0.30
REPO = HERE.parent
SENT_SPLIT = re.compile(r'(?<=[。！？；!?;])|[\n]')


def char_span(ed, bts):
    s1, s2 = ed['span']
    return len(bts[:s1].decode('utf-8', 'ignore')), len(bts[:s2].decode('utf-8', 'ignore'))


def main():
    table, bg = correct.load_zh_ngram_tables(
        str(HERE / 'ngram_table.json'), str(HERE / 'bigram_table_zh.json'))
    for nm in ('cscdns_gold.json', 'nlpcc23_gold.json', 'cctc_gold.json'):
        data = json.load(open(REPO / 'datasets' / nm, encoding='utf-8'))
        cov, vis = {}, {}
        uncovered = Counter()
        for e in data:
            if not e['edits']:
                continue
            src = e['src']
            bts = src.encode('utf-8')
            hits = correct.find_zh_hits(src)
            sents = [s for s in SENT_SPLIT.split(src) if s.strip()]
            sent_gate = [bool(correct.find_zh_hits(s)) or
                         correct.zh_heuristic_prob(s, table, bg) >= GRAY
                         for s in sents]
            off, bounds = 0, []
            for s in sents:
                i = src.find(s, off)
                bounds.append((i, i + len(s)))
                off = i + len(s)
            for ed in e['edits']:
                c1, c2 = char_span(ed, bts)
                if c2 == c1:
                    c2 = c1 + 1
                cat = ed['cat']
                cov.setdefault(cat, [0, 0, 0])
                vis.setdefault(cat, [0, 0])
                cov[cat][0] += 1
                ov = [h for h in hits if h.span[0] < c2 and h.span[1] > c1]
                strict = [h for h in ov if h.correction and ed['ref'] and ed['ref'] in h.correction]
                cov[cat][1] += bool(ov)
                cov[cat][2] += bool(strict)
                if cat == 'char-sub' and not strict:
                    uncovered[(ed['src'], ed['ref'])] += 1
                k = next((k for k, (a, b) in enumerate(bounds) if a <= c1 < b), None)
                vis[cat][1] += bool(k is not None and sent_gate[k])
        print(f'\n== {nm} ==')
        print('| cat | n | pattern 重叠 | 严格覆盖 | 门控可见 |')
        for cat, (n, o, s) in cov.items():
            v = vis[cat]
            print(f'| {cat} | {n} | {o} ({o/n:.0%}) | {s} ({s/n:.0%}) | {v[1]} ({v[1]/n:.0%}) |')
        if uncovered:
            print('未覆盖 char-sub top15:',
                  ', '.join(f'{a}→{b}×{c}' for (a, b), c in uncovered.most_common(15)))


if __name__ == '__main__':
    main()
