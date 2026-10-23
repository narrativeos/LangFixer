"""D.2：微调 Chinese ELECTRA discriminator 的 CTC 头做字级错误检测。

训练对来自 l3_pool_raw（src=错句, ref=净句）：
- 正例：src 句，标签由 SequenceMatcher(src, ref) 字级对齐生成（replace/delete→1），
  English/多字 wordpiece 用 offset mapping 对齐到首字
- 负例：ref 干净句（全 0），1:1 混合校准
- 去污染：load_blocklist()（MuCGEC/CSCD/NaCGEC/cscdns/nlpcc23/cctc 评测源句剔除）

评测（训练后）：对 6 个 gold 集算句级 AUC + 门控操作点，对照启发式。
用法（需 .venv_char；模型缓存在 HF，国内用 HF_ENDPOINT=https://hf-mirror.com）：
  HF_ENDPOINT=https://hf-mirror.com ./.venv_char/bin/python finetune_char_ctc.py [--steps N]
"""
import argparse
import difflib
import gzip
import hashlib
import json
import os
import random
import re
import sys
import time
from pathlib import Path

import torch
from torch.utils.data import DataLoader, Dataset
from transformers import AutoModelForPreTraining, AutoTokenizer

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from prepare_l3_pool import load_blocklist  # noqa: E402

BASE_MODEL = 'hfl/chinese-electra-180g-base-discriminator'
OUT_DIR = HERE / 'models' / 'char_ctc_zh'
MAX_LEN = 128
POOL_FILES = ['train_CGED.txt', 'train_FCGEC.txt', 'train.txt']
CSC_SAMPLE = 40000  # train.txt 采样上限（CGEC/FCGEC 全量）


def load_pairs():
    block = load_blocklist()
    rng = random.Random(42)
    by_file = {}
    for fn in POOL_FILES:
        rows = []
        for ln in open(HERE / 'l3_pool_raw' / fn, encoding='utf-8'):
            parts = ln.rstrip('\n').split('\t')
            if len(parts) < 2:
                continue
            src, ref = parts[0].strip(), parts[1].strip()
            if not src or not ref or src == ref or src in block or ref in block:
                continue
            if len(src) > 110 or len(ref) > 110:
                continue
            rows.append((src, ref))
        by_file[fn] = rows
    pairs = by_file['train_CGED.txt'] + by_file['train_FCGEC.txt']
    rng.shuffle(by_file['train.txt'])
    pairs += by_file['train.txt'][:CSC_SAMPLE]
    rng.shuffle(pairs)
    return pairs


def load_confirmed(repeat: int, holdout: float):
    """飞轮正样本（用户采纳的生产文本）→ 纯干净负例，过采样 repeat 次。
    域校准用途（G 遗留：短口语分数系统性偏高）；confirmed 是飞轮资产，
    不属于评测集，允许进训练。

    holdout>0：按句子 md5 稳定切分（与 repeat 无关，跨训练版本一致），
    切出部分**不进训练**并固化到 l3_data/zh/confirmed_holdout.json——
    τ_hi 等生产校准评测只在 holdout 上做，杜绝记忆效应假象（G2 教训）。"""
    p = HERE / 'l3_data' / 'zh' / 'confirmed.jsonl'
    if not p.is_file() or repeat <= 0:
        return []
    rows = []
    for ln in open(p, encoding='utf-8'):
        ln = ln.strip()
        if not ln:
            continue
        t = str(json.loads(ln).get('text', '')).strip()
        if t and len(t) <= 110:
            rows.append(t)
    if holdout > 0:
        train = [t for t in rows if int(hashlib.md5(t.encode()).hexdigest()[:8], 16) % 100 >= round(holdout * 100)]
        hold = [t for t in rows if int(hashlib.md5(t.encode()).hexdigest()[:8], 16) % 100 < round(holdout * 100)]
        hp = HERE / 'l3_data' / 'zh' / 'confirmed_holdout.json'
        old = json.load(open(hp, encoding='utf-8')) if hp.is_file() else []
        json.dump(sorted(set(old + hold)), open(hp, 'w', encoding='utf-8'), ensure_ascii=False, indent=0)
        print(f'confirmed holdout: {len(hold)} → {hp}', flush=True)
        rows = train
    return [(t, t) for t in rows for _ in range(repeat)]


_PLACE = re.compile(r'ued\d*|\[UNK\]|<unk>')


def load_extra_clean(path, cap: int, repeat: int, seed: int):
    """外部**纯干净**长文负例（EVOLUTION §13 #5：midu2022 初赛伪数据的干净侧）。

    只取干净侧（jsonl 的 target/ref/text，或纯文本每行一句），**绝不取错误侧**。
    过滤：去重、占位符、长度 6..110（与池对同口径）、去污染块表；
    自污染防护：同一 jsonl 里出现在 source 侧的句子一律剔除（那句子本身有错）。
    种子化抽样到 cap，再按 repeat 过采样。返回 (t, t) 纯负例对。
    """
    p = Path(path)
    if not p.is_file():
        sys.exit(f'--extra-clean: 文件不存在 {p}')
    opener = gzip.open if p.suffix == '.gz' else open
    seen, texts, src_side = set(), [], set()
    with opener(p, 'rt', encoding='utf-8', errors='replace') as f:
        for ln in f:
            ln = ln.strip()
            if not ln:
                continue
            if ln.startswith('{'):
                try:
                    o = json.loads(ln)
                except Exception:
                    continue
                for k in ('source', 'src'):
                    s = str(o.get(k, '')).strip()
                    if s:
                        src_side.add(s)
                t = ''
                for k in ('target', 'ref', 'text', 'clean'):
                    t = str(o.get(k, '')).strip()
                    if t:
                        break
            else:
                t = ln
            if not t or t in seen:
                continue
            seen.add(t)
            if _PLACE.search(t) or not (6 <= len(t) <= 110):
                continue
            texts.append(t)
    block = load_blocklist()
    texts = [t for t in texts if t not in block and t not in src_side]
    random.Random(seed).shuffle(texts)
    if cap:
        texts = texts[:cap]
    return [(t, t) for t in texts for _ in range(repeat)]


def char_labels(src, ref):
    """字级标签：replace/delete 的 src 字符 → 1。"""
    lab = [0] * len(src)
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(None, src, ref).get_opcodes():
        if tag in ('replace', 'delete'):
            for i in range(i1, i2):
                lab[i] = 1
    return lab


class CTCDataset(Dataset):
    def __init__(self, pairs, tok):
        self.examples = []
        for src, ref in pairs:
            self._add(src, char_labels(src, ref), tok)
            self._add(ref, [0] * len(ref), tok)

    def _add(self, text, lab, tok):
        enc = tok(text, truncation=True, max_length=MAX_LEN,
                  return_offsets_mapping=True)
        ids = enc['input_ids']
        offs = enc['offset_mapping']
        labels, mask = [], []
        for (a, b), tid in zip(offs, ids):
            if a == b == 0:  # [CLS]/[SEP]/pad
                labels.append(0)
                mask.append(0)
            else:
                labels.append(lab[a] if a < len(lab) else 0)
                mask.append(1)
        self.examples.append((ids, labels, mask))

    def __len__(self):
        return len(self.examples)

    def __getitem__(self, i):
        return self.examples[i]


def collate(batch, pad_id=0):
    B = len(batch)
    L = max(len(x[0]) for x in batch)

    def arr(width, val):
        return torch.full((B, width), val, dtype=torch.long)
    ids, lab, msk = arr(L, pad_id), arr(L, 0), arr(L, 0)
    for i, (x, y, z) in enumerate(batch):
        n = len(x)
        ids[i, :n] = torch.tensor(x)
        lab[i, :n] = torch.tensor(y)
        msk[i, :n] = torch.tensor(z)
    return ids, lab, msk


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--epochs', type=int, default=1)
    ap.add_argument('--batch', type=int, default=32)
    ap.add_argument('--lr', type=float, default=3e-5)
    ap.add_argument('--max-steps', type=int, default=0)
    ap.add_argument('--confirmed-repeat', type=int, default=20,
                    help='飞轮 confirmed 作干净负例的过采样倍数（0=关闭）')
    ap.add_argument('--confirmed-holdout', type=float, default=0.2,
                    help='confirmed 稳定 holdout 比例（不进训练，固化供 τ_hi 校准）')
    ap.add_argument('--extra-clean', default='',
                    help='外部纯干净长文负例（jsonl 取 target/ref/text，或纯文本每行一句）；'
                         '只取干净侧，过滤+去污染+种子抽样（§13 #5 A/B 用）')
    ap.add_argument('--extra-clean-max', type=int, default=50000,
                    help='干净侧语料种子化抽样上限（0=全量）')
    ap.add_argument('--extra-clean-repeat', type=int, default=1,
                    help='干净侧过采样倍数（1:1 与 cap 相乘）')
    ap.add_argument('--seed', type=int, default=42,
                    help='torch/random/DataLoader 种子（A/B 双臂必须一致）')
    ap.add_argument('--out', default=str(OUT_DIR))
    args = ap.parse_args()

    random.seed(args.seed)
    torch.manual_seed(args.seed)
    t0 = time.time()
    pairs = load_pairs()
    print(f'pairs: {len(pairs)} ({time.time()-t0:.0f}s)', flush=True)
    dev, train = pairs[:2000], pairs[2000:]
    conf = load_confirmed(args.confirmed_repeat, args.confirmed_holdout)
    if conf:
        print(f'confirmed negatives (oversampled): {len(conf)}', flush=True)
    extra = (load_extra_clean(args.extra_clean, args.extra_clean_max,
                              args.extra_clean_repeat, args.seed)
             if args.extra_clean else [])
    if extra:
        print(f'extra clean negatives: {len(extra)} '
              f'(cap={args.extra_clean_max} repeat={args.extra_clean_repeat}) '
              f'({time.time()-t0:.0f}s)', flush=True)

    tok = AutoTokenizer.from_pretrained(BASE_MODEL)
    ds = CTCDataset(train + conf + extra, tok)
    dv = CTCDataset(dev, tok)
    print(f'examples: train={len(ds)} dev={len(dv)} ({time.time()-t0:.0f}s)', flush=True)

    model = AutoModelForPreTraining.from_pretrained(BASE_MODEL)
    dev_t = 'mps' if torch.backends.mps.is_available() else 'cpu'
    model.to(dev_t)
    opt = torch.optim.AdamW(model.parameters(), lr=args.lr)
    dl = DataLoader(ds, batch_size=args.batch, shuffle=True, collate_fn=collate,
                    generator=torch.Generator().manual_seed(args.seed))

    model.train()
    n_pos = 0
    step = 0
    for ids, lab, msk in dl:
        ids, lab, msk = ids.to(dev_t), lab.to(dev_t), msk.to(dev_t)
        logits = model(input_ids=ids, attention_mask=(ids != 0).long()).logits
        bce = torch.nn.functional.binary_cross_entropy_with_logits(logits, lab.float(), reduction='none')
        denom = msk.sum().clamp(min=1)
        loss = (bce * msk).sum() / denom
        if not torch.isfinite(loss):
            # MPS command buffer 故障（并发抢占 GPU 时会发生）会把张量打坏成 NaN/垃圾，
            # 继续训只是浪费时间——直接作废本轮，重跑
            print(f'  ABORT: step {step} loss={float(loss)} 非有限值'
                  f'（疑似 GPU command buffer 故障），本轮作废重跑', flush=True)
            sys.exit(3)
        opt.zero_grad()
        loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        opt.step()
        n_pos += int((lab * msk).sum())
        step += 1
        if step % 100 == 0:
            print(f'  step {step} loss {float(loss):.4f} pos {n_pos} ({time.time()-t0:.0f}s)', flush=True)
        if args.max_steps and step >= args.max_steps:
            break

    # dev sanity（不跑全量评测也能立刻判断本轮是否被 GPU 故障污染）：
    # 字级 loss + 字级 AUC（正例=被改字，负例=未改字）
    model.eval()
    probs, labs = [], []
    vsum, vcnt = 0.0, 0
    with torch.no_grad():
        for ids, lab, msk in DataLoader(dv, batch_size=args.batch, collate_fn=collate):
            ids, lab, msk = ids.to(dev_t), lab.to(dev_t), msk.to(dev_t)
            logits = model(input_ids=ids, attention_mask=(ids != 0).long()).logits
            bce = torch.nn.functional.binary_cross_entropy_with_logits(
                logits.float(), lab.float(), reduction='none')
            m = msk.bool()
            vsum += float((bce * msk).sum())
            vcnt += int(msk.sum())
            probs.append(torch.sigmoid(logits.float())[m].cpu())
            labs.append(lab[m].cpu())
    import bisect
    p = torch.cat(probs).tolist()
    y = torch.cat(labs).tolist()
    sp = sorted(p)
    tot = 0.0
    n_pos_dev = n_neg_dev = 0
    for pi, yi in zip(p, y):
        if yi:
            n_pos_dev += 1
            lo = bisect.bisect_left(sp, pi)
            tot += lo + 0.5 * (bisect.bisect_right(sp, pi) - lo)
        else:
            n_neg_dev += 1
    auc_dev = tot / (n_pos_dev * n_neg_dev) if n_pos_dev and n_neg_dev else float('nan')
    print(f'dev: loss {vsum/max(1,vcnt):.4f}  char AUC {auc_dev:.3f} '
          f'(pos {n_pos_dev} / neg {n_neg_dev})', flush=True)
    if not (vsum == vsum and auc_dev == auc_dev):
        print('  ABORT: dev 指标非有限值，本轮作废', flush=True)
        sys.exit(3)
    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    model.save_pretrained(out_dir)
    tok.save_pretrained(out_dir)
    # 落训练 cmd + 计数 + dev 指标：checkpoint 本身不记录它用了哪一档
    # --extra-clean，A/B 双臂因此必须从这里读回（abA/abB 缺此记录，不可复产）
    meta = {'argv': sys.argv, 'base_model': BASE_MODEL, 'seed': args.seed,
            'epochs': args.epochs, 'batch': args.batch, 'lr': args.lr,
            'max_steps': args.max_steps, 'steps_done': step,
            'pairs': len(pairs), 'confirmed_neg': len(conf),
            'extra_clean_src': args.extra_clean,
            'extra_clean_max': args.extra_clean_max,
            'extra_clean_repeat': args.extra_clean_repeat,
            'extra_clean_n': len(extra), 'examples_train': len(ds),
            'examples_dev': len(dv),
            'dev_loss': vsum / max(1, vcnt), 'dev_char_auc': auc_dev,
            'secs': round(time.time() - t0)}
    (out_dir / 'train_meta.json').write_text(
        json.dumps(meta, ensure_ascii=False, indent=1), encoding='utf-8')
    print(f'saved → {out_dir} ({time.time()-t0:.0f}s)')


if __name__ == '__main__':
    main()
