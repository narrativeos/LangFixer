#!/usr/bin/env python3
"""CTCDataset 利用率审计：逐子集与项目训练资产 / 评测 gold 做句子级重叠比对。

用途：判断 CTCDataset（~/github/CTCDataset）的哪些子集
- 已进训练资产（`feature-model/l3_pool_raw/` 四文件的 src+ref）→ 不可再作评测（污染）；
- 已进评测 gold → 已用；
- 两者都不在 → **未利用**（评测或训练增量）。

输出：每子集 {去重句数, 有错率, 中位句长, 与 l3_pool 重叠%, 与 gold 重叠%, 干净句数}。

结论快照（2026-10-19 实测）：
- 已覆盖：Wang271k 98.9%、lemon 93–100%、cscd-ns train/dev 99.7/99.6%、ECSpell 80–87%、
  MCSCSet test/valid 100%（⚠️ 已污染 → 不可作 CEC 的 MCSC 基准）、sighan14/15 train 96–98%；
- 未利用（重叠 ≈0%）：CTC2021/train_large_v2（217,617 对，OCR/形近合成噪声 → 只适合字级
  判别侧车，不入 L3 句级池；其「喂 n-gram 干净侧」备选已实测否定，见下）、
  midu2022（final/prelim_*）、CTC2021/val*、
  sighan13/14 test、CCTC/train、MCSCSet/train（157k）、yacsc/YACSC（13–39% 已污染，作评测
  须过滤）、NLPCC2023/grammar/HSK（61% 已覆盖，增量约 6.1 万对）；
- 已接入评测：cscd-ns/test、NLPCC2023/spell、CCTC/test、yacsc sighan_revised 13/14/15、
  midu2022/preliminary_val、CTC2021/val_revised（见 `prepare_ctc.py`）。
- **已评估、明确不接入（决策锁定，详见 `EVOLUTION.md` §13）**：
  midu2022 **初赛 100 万伪数据**（外部下载 `preliminary_train.jsonl.gz`；注意与本仓库
  `midu2022/prelim_train` = `preliminary_extend_train.jsonl`（1,000 条人工修正）不是同一份）
  ——零污染（src/tgt ∩ gold = 0.00%）但 100% 有错、零负例，错误分布 S/D/I=45/32/23、
  单字替换同音仅 24.5%（真实流量 99.4% / 80.9%）→ 不评测 / 不扩 n-gram 表 / 不进 L3 池 /
  不扩混淆字词表；唯一未排除路径 = 字级侧车 clean 长文负例（未测）。
  **通用推论（适用任何外部干净语料）：扩 n-gram 表只移动「成本↔漏检」工作点，不改善
  判别比**——验收口径必须是「等漏检下的放行率」（`eval_l3_funnel.py` 真实用户文本门控段）。

用法：`python3 ctc_audit.py [CTCDataset根目录]`（纯标准库，可读 .gz）
"""
import gzip
import hashlib
import json
import os
import statistics
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
POOL = os.path.join(REPO, 'feature-model', 'l3_pool_raw')
DS = os.path.join(REPO, 'datasets')

GOLD_FILES = ('cscd_gold.json', 'nacgec_gold.json', 'mucgec_gold.json',
              'cscdns_gold.json', 'nlpcc23_gold.json', 'cctc_gold.json',
              'sig13_gold.json', 'sig14_gold.json', 'midu_gold.json',
              'ctc21_gold.json', 'jfleg_gold.json')

POOL_FILES = ('train.txt', 'train_CGED.txt', 'train_FCGEC.txt', 'train.para')

# (标签, CTCDataset 相对路径) —— 覆盖上游 README 统计里的全部子集
SUBSETS = [
    ('cscd-ns/train', 'cscd-ns/train.jsonl'),
    ('cscd-ns/dev', 'cscd-ns/dev.jsonl'),
    ('cscd-ns/test', 'cscd-ns/test.jsonl'),
    ('CCTC/train', 'CCTC/cctc_train.jsonl'),
    ('CCTC/test', 'CCTC/cctc_test_wide.jsonl'),
    ('NLPCC23/spell', 'NLPCC2023/spell/data.jsonl'),
    ('NLPCC23/gram/HSK', 'NLPCC2023/grammar/HSK/hsk.jsonl'),
    ('NLPCC23/gram/MuCGEC', 'NLPCC2023/grammar/MuCGEC/MuCGEC_dev_all.jsonl'),
    ('ECSpell/law', 'ECSpell/ec_law.jsonl'),
    ('ECSpell/med', 'ECSpell/ec_med.jsonl'),
    ('ECSpell/odw', 'ECSpell/ec_odw.jsonl'),
    ('MCSCSet/test', 'MCSCSet/test.jsonl'),
    ('MCSCSet/valid', 'MCSCSet/valid.jsonl'),
    ('MCSCSet/train', 'MCSCSet/train.jsonl'),
    ('lemon/car', 'lemon/car.jsonl'),
    ('lemon/new', 'lemon/new.jsonl'),
    ('lemon/nov', 'lemon/nov.jsonl'),
    ('lemon/mec', 'lemon/mec.jsonl'),
    ('lemon/gam', 'lemon/gam.jsonl'),
    ('sighan/13_test', 'sighan/sighan13_test.jsonl'),
    ('sighan/14_test', 'sighan/sighan14_test.jsonl'),
    ('sighan/15_test', 'sighan/sighan15_test.jsonl'),
    ('sighan/14_train', 'sighan/sighan14_train.jsonl'),
    ('sighan/15_train', 'sighan/sighan15_train.jsonl'),
    ('yacsc/no_GE', 'yacsc/YACSC/YACSC-no_GE.jsonl'),
    ('yacsc/with_GE', 'yacsc/YACSC/YACSC-with_GE.jsonl'),
    ('yacsc/rev_sighan13', 'yacsc/sighan_revised/test_sighan13.jsonl'),
    ('yacsc/rev_sighan14', 'yacsc/sighan_revised/test_sighan14.jsonl'),
    ('yacsc/rev_sighan15', 'yacsc/sighan_revised/test_sighan15.jsonl'),
    ('midu2022/final', 'midu2022/final.jsonl'),
    ('midu2022/prelim_train', 'midu2022/preliminary_extend_train.jsonl'),
    ('midu2022/prelim_val', 'midu2022/preliminary_val.jsonl'),
    ('CTC2021/val', 'CTC2021/val.jsonl'),
    ('CTC2021/val_revised', 'CTC2021/val_revised.jsonl'),
    ('CTC2021/train_large_v2', 'CTC2021/train_large_v2.jsonl.gz'),
    ('Wang271k/data', 'Wang271k/data.jsonl.gz'),
]

h = lambda s: hashlib.md5(s.encode('utf-8')).digest()  # noqa: E731


def load_pool() -> set:
    """l3_pool_raw 全部平行对的 src ∪ ref（md5 摘要，省内存）。"""
    out = set()
    for fn in POOL_FILES:
        p = os.path.join(POOL, fn)
        if not os.path.isfile(p):
            print(f'MISSING pool: {p}')
            continue
        with open(p, encoding='utf-8', errors='replace') as f:
            for ln in f:
                parts = ln.rstrip('\n').split('\t')
                for side in parts[:2]:
                    side = side.strip()
                    if side:
                        out.add(h(side))
    return out


def load_gold_src() -> set:
    out = set()
    for nm in GOLD_FILES:
        p = os.path.join(DS, nm)
        if not os.path.isfile(p):
            continue
        for item in json.load(open(p, encoding='utf-8')):
            if item.get('src'):
                out.add(h(item['src']))
    return out


def open_any(path: str):
    return gzip.open(path, 'rt', encoding='utf-8') if path.endswith('.gz') \
        else open(path, encoding='utf-8')


def main() -> None:
    ctc = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser('~/github/CTCDataset')
    pool = load_pool()
    gold = load_gold_src()
    print(f'l3_pool_raw 句侧: {len(pool)}   gold 源句: {len(gold)}\n')
    hdr = f"{'subset':24}{'n':>8}{'uniq':>8}{'err%':>6}{'len':>5}{'pool%':>7}{'gold%':>7}{'clean':>7}"
    print(hdr)
    print('-' * len(hdr))
    for name, rel in SUBSETS:
        path = os.path.join(ctc, rel)
        if not os.path.isfile(path):
            print(f'{name:24}MISSING {rel}')
            continue
        n = uniq = err = clean = in_pool = in_gold = 0
        lens = []
        seen = set()
        with open_any(path) as f:
            for ln in f:
                ln = ln.strip()
                if not ln:
                    continue
                try:
                    r = json.loads(ln)
                except json.JSONDecodeError:
                    continue
                src = str(r.get('source', '')).strip()
                if not src:
                    continue
                n += 1
                lens.append(len(src))
                tgt = str(r.get('target', '')).strip()
                if int(r.get('label', 0)) == 1 or (tgt and tgt != src):
                    err += 1
                if not tgt or tgt == src:
                    clean += 1
                hh = h(src)
                if hh in seen:
                    continue
                seen.add(hh)
                uniq += 1
                in_pool += hh in pool
                in_gold += hh in gold
        med = int(statistics.median(lens)) if lens else 0
        pp = 100.0 * in_pool / max(uniq, 1)
        gp = 100.0 * in_gold / max(uniq, 1)
        print(f'{name:24}{n:>8}{uniq:>8}{100.0*err/max(n,1):>6.0f}{med:>5}'
              f'{pp:>7.1f}{gp:>7.1f}{clean:>7}')


if __name__ == '__main__':
    main()
