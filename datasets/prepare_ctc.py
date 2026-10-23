#!/usr/bin/env python3
"""将 CTCDataset（~/github/CTCDataset）中未污染的评测集转为标准 gold JSON：

| gold 文件 | CTCDataset 来源 | split | 定位 | diff 口径 |
|---|---|---|---|---|
| `cscdns_gold.json`  | cscd-ns/test.jsonl | cscdns | 原生微博母语者文本（真实流量最佳代理） | difflib（历史口径） |
| `nlpcc23_gold.json` | NLPCC2023/spell/data.jsonl | nlpcc23 | 人工标注拼写检查（500 错/500 净） | difflib（历史口径） |
| `cctc_gold.json`    | CCTC/cctc_test_wide.jsonl | cctc | 跨句母语者纠错（语境评测用） | difflib（历史口径） |
| `sig13_gold.json`   | yacsc/sighan_revised/test_sighan13.jsonl | sig13 | SIGHAN-2013 修订版（泛化性） | cec_common |
| `sig14_gold.json`   | yacsc/sighan_revised/test_sighan14.jsonl | sig14 | SIGHAN-2014 修订版（泛化性） | cec_common |
| `midu_gold.json`    | midu2022/preliminary_val.jsonl | midu | 蜜度 2022 初赛验证集（真实场景长文 50/50） | cec_common |
| `ctc21_gold.json`   | CTC2021/val_revised.jsonl | ctc21 | CTC2021 校对验证集（真实场景长文） | cec_common |

schema 与 cscd_gold.json 一致：`{split, src, edits:[{op,src,ref,span,cat,cat_rule}]}`；
edits 由 source→target 字级 diff 得出（label=0 → 空编辑）。

两种 diff 口径：
- **difflib**（`diff_edits_plain`）：cscdns/nlpcc23/cctc 三个已发布 gold 的原始口径，
  保持不动以维持既有数字可比（`SequenceMatcher` 默认 autojunk 在长句上会误配，
  新集不再使用该口径）；
- **cec_common**（`cec_common.diff_edits`，需 pypinyin + opencc → `datasets/.venv`）：
  与 cscd/nacgec/mucgec gold 同构，`cat_rule` 带 punct/particle/variant/confusable/other，
  新集一律走这条，数字才与 cscd/sig15 可比；缺依赖时回退 difflib 并告警。

sig15 gold 修订（默认开，`--keep-sig15` 跳过）：CTCDataset 的 `yacsc/sighan_revised/`
是 SIGHAN 测试集的校对版（修繁简转换噪音 + 补漏标）。`cscd_gold.json` 的 sig15 split
原样来自 twnlp `test_sig_15.txt`，与修订版比对约 7% 漏标（962 句交集里 67 句）；
本步骤把该 split 整体替换为修订版并打印修正统计。

去污染：`prepare_l3_pool.load_blocklist()` 已纳入本脚本产出的全部 gold 文件，
n-gram 表/训练池构建不会包含其任何源句。CTCDataset 的 train 侧与 l3_pool_raw 重叠
90%+（twnlp csc_data 即其混合），故不并入训练资产；test 侧保持评测纯净
（逐子集重叠审计见 `ctc_audit.py`）。

用法：
    ./.venv/bin/python prepare_ctc.py            # 用 datasets/.venv（pypinyin + opencc）
    python3 prepare_ctc.py [CTCDataset根目录] [--keep-sig15]
"""
import difflib
import json
import os
import sys
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
CTC = os.path.expanduser('~/github/CTCDataset')

# (gold 文件名, split, CTCDataset 相对路径, diff 口径)
SOURCES = [
    ('cscdns_gold.json', 'cscdns', 'cscd-ns/test.jsonl', 'plain'),
    ('nlpcc23_gold.json', 'nlpcc23', 'NLPCC2023/spell/data.jsonl', 'plain'),
    ('cctc_gold.json', 'cctc', 'CCTC/cctc_test_wide.jsonl', 'plain'),
    ('sig13_gold.json', 'sig13', 'yacsc/sighan_revised/test_sighan13.jsonl', 'cec'),
    ('sig14_gold.json', 'sig14', 'yacsc/sighan_revised/test_sighan14.jsonl', 'cec'),
    ('midu_gold.json', 'midu', 'midu2022/preliminary_val.jsonl', 'cec'),
    ('ctc21_gold.json', 'ctc21', 'CTC2021/val_revised.jsonl', 'cec'),
]

# sig15 gold 的替换源（yacsc 校对版）与被替换的 gold 文件 / split
SIG15_REVISED = 'yacsc/sighan_revised/test_sighan15.jsonl'
CSCD_GOLD = 'cscd_gold.json'
SIG15_SPLIT = 'sig15'

PUNCT = set('，。！？；：、“”‘’（）《》【】,.!?;:()<>「」『』…—～ ')

sys.path.insert(0, HERE)
try:
    import cec_common as C  # noqa: E402  （依赖 pypinyin + opencc）
except Exception:  # noqa: BLE001  缺依赖 → 回退 difflib 口径
    C = None


def byte_span(s: str, i1: int, i2: int):
    return [len(s[:i1].encode('utf-8')), len(s[:i2].encode('utf-8'))]


def classify(aw: str, bw: str) -> str:
    if aw and all(c in PUNCT for c in aw) or bw and all(c in PUNCT for c in bw):
        return 'punct'
    if not bw:
        return 'deletion'
    if not aw:
        return 'insertion'
    return 'char-sub' if len(aw) == 1 and len(bw) == 1 else 'word-sub'


def diff_edits_plain(src: str, tgt: str):
    """历史口径（cscdns/nlpcc23/cctc）：cat_rule 仅标 punct，其余 'other'。"""
    edits = []
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(None, src, tgt).get_opcodes():
        if tag == 'equal':
            continue
        aw, bw = src[i1:i2], tgt[j1:j2]
        op = 'S' if tag == 'replace' else ('D' if tag == 'delete' else 'I')
        cat = classify(aw, bw)
        span = byte_span(src, i1, i2) if op != 'I' else byte_span(src, i1, i1)
        edits.append({'op': op, 'src': aw, 'ref': bw, 'span': span,
                      'cat': cat, 'cat_rule': 'punct' if cat == 'punct' else 'other'})
    edits.sort(key=lambda e: e['span'][0])
    return edits


def pick_diff(mode: str):
    """按口径取 diff 函数；'cec' 不可用时回退 difflib 并告警。"""
    if mode == 'cec':
        if C is None:
            print('  ⚠️  cec_common 不可用（缺 pypinyin/opencc）→ 回退 difflib 口径'
                  '（cat_rule 只剩 punct/other）；建议用 datasets/.venv 重跑')
            return diff_edits_plain
        return C.diff_edits
    return diff_edits_plain


def read_jsonl(path: str):
    with open(path, encoding='utf-8') as f:
        for ln in f:
            ln = ln.strip()
            if ln:
                yield json.loads(ln)


def build_split(path: str, split: str, diff):
    """jsonl（{source,target,label}）→ gold 条目列表（按 src 去重）。"""
    out, seen, skipped = [], set(), Counter()
    for r in read_jsonl(path):
        src, tgt = str(r.get('source', '')).strip(), str(r.get('target', '')).strip()
        label = int(r.get('label', 0))
        if not src or src in seen:
            skipped['dup/empty'] += 1
            continue
        seen.add(src)
        edits = diff(src, tgt) if tgt and tgt != src else []
        if label == 1 and not edits:
            skipped['label=1 无 diff'] += 1
            continue
        if label == 0 and edits:
            skipped['label=0 有 diff'] += 1
        out.append({'split': split, 'src': src, 'edits': edits})
    return out, skipped


def write_json(path: str, data) -> None:
    tmp = path + '.tmp'
    with open(tmp, 'w', encoding='utf-8') as f:
        json.dump(data, f, ensure_ascii=False)
    os.replace(tmp, path)


def patch_sig15(cscd_path: str, rev_path: str, diff) -> None:
    """cscd_gold.json 的 sig15 split → yacsc 校对版（句集 + 编辑一并替换）。"""
    if not os.path.isfile(cscd_path):
        print(f'MISSING: {cscd_path}（先跑 prepare_cscd.py）')
        return
    if not os.path.isfile(rev_path):
        print(f'MISSING: {rev_path}（yacsc 校对版缺失 → sig15 保持原样）')
        return

    gold = json.load(open(cscd_path, encoding='utf-8'))
    old_sig = [it for it in gold if it.get('split') == SIG15_SPLIT]
    if not old_sig:
        print(f'{CSCD_GOLD}: 无 {SIG15_SPLIT} split，跳过')
        return
    old_by_src = {it['src']: it for it in old_sig}

    new_sig, skipped = build_split(rev_path, SIG15_SPLIT, diff)
    new_by_src = {it['src']: it for it in new_sig}

    inter = set(old_by_src) & set(new_by_src)
    key = lambda it: [(e['op'], e['ref']) for e in it['edits']]  # noqa: E731
    gain = sum(1 for s in inter if not old_by_src[s]['edits'] and new_by_src[s]['edits'])
    drop = sum(1 for s in inter if old_by_src[s]['edits'] and not new_by_src[s]['edits'])
    rewritten = sum(1 for s in inter
                    if old_by_src[s]['edits'] and new_by_src[s]['edits']
                    and key(old_by_src[s]) != key(new_by_src[s]))

    # 原位替换：其它 split 顺序不变，sig15 段整体换成修订版
    out, inserted = [], False
    for it in gold:
        if it.get('split') == SIG15_SPLIT:
            if not inserted:
                out.extend(new_sig)
                inserted = True
            continue
        out.append(it)

    write_json(cscd_path, out)
    n_err = sum(1 for it in new_sig if it['edits'])
    print(f'{CSCD_GOLD} sig15 → yacsc 校对版：{len(old_sig)} → {len(new_sig)} 句'
          f'（含错 {n_err}）；编辑 '
          f'{sum(len(it["edits"]) for it in old_sig)} → {sum(len(it["edits"]) for it in new_sig)}')
    print(f'  句集：交集 {len(inter)}，换入 {len(set(new_by_src) - set(old_by_src))}，'
          f'换出 {len(set(old_by_src) - set(new_by_src))}（多为繁简/空白规范化差异）')
    print(f'  标注：补漏标 {gain} 句、去多标 {drop} 句、编辑改写 {rewritten} 句'
          f'，跳过 {dict(skipped)}')


def main():
    import argparse

    ap = argparse.ArgumentParser(description='CTCDataset → gold JSON')
    ap.add_argument('ctc_root', nargs='?', default=CTC, help='CTCDataset 根目录')
    ap.add_argument('--keep-sig15', action='store_true',
                    help='不替换 cscd_gold.json 的 sig15 split（默认替换为 yacsc 校对版）')
    args = ap.parse_args()

    for out_name, split, rel, mode in SOURCES:
        path = os.path.join(args.ctc_root, rel)
        if not os.path.isfile(path):
            print(f'MISSING: {path}')
            continue
        out, skipped = build_split(path, split, pick_diff(mode))
        out_path = os.path.join(HERE, out_name)
        write_json(out_path, out)
        n_err = sum(1 for e in out if e['edits'])
        ratio = n_err / len(out) if out else 0.0
        cats = Counter(ed['cat_rule'] for e in out for ed in e['edits'])
        print(f'{out_name}: {len(out)} 句（含错 {n_err} {ratio:.1%}）'
              f' 跳过 {dict(skipped)} → {out_path}')
        print(f'  cat_rule: {dict(cats.most_common())}')

    if not args.keep_sig15:
        patch_sig15(os.path.join(HERE, CSCD_GOLD),
                    os.path.join(args.ctc_root, SIG15_REVISED), pick_diff('cec'))


if __name__ == '__main__':
    main()
