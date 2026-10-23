#!/usr/bin/env python3
"""L3 特征模型工作流：extract（特征抽取）/ train（训练）/ eval（评测）。

数据流（与 decision-model 的 vega_finetune.py 同构，扩展为 L3 双模型）：
    prepare_data.py 产出 data/<lang>/{train,dev}.jsonl
        → extract: 每句 → 特征向量
        → train:   ① 句级二分类（has_error：有错误 / 无错误）
                    ② 错误类型多分类（collocation / word_order / structure / logic / grammar）
                    vegaml 可用则用其训练；否则回退 sklearn
                    （L3_FALLBACK=1 强制回退），模型存 models/<lang>/
        → eval:    在 dev 集上报告 acc / precision / recall / F1 + 错误类型分布

特征（通用句级特征 + correct.py 模式命中数，与 server.py 推理共用）：
    len, words, cjk_ratio, unique_ratio, repeat_ratio, punct_ratio,
    upper_ratio, digit_ratio, 8 个启发式错误模式命中数,
    以及 correct.find_hits 的 collocation / word_order / missing 命中数

用法：
    python l3_finetune.py extract --lang en --data data/en --out l3_data/en
    python l3_finetune.py train   --lang en --data data/en
    python l3_finetune.py eval    --lang en --data data/en
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import sys
from collections import Counter

sys.path.insert(0, os.path.dirname(__file__))
import correct  # noqa: E402

# 通用错误信号模式（命中 → 特征置 1；与 decision-model 启发式同源但更宽）
_ERROR_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"\b(teh|recieve|seperat\w*|occured|writen|begining|comming|wich|becuase|tommorow|untill)\b", re.I),
    re.compile(r"\b(he|she|it|they|we|you)\s+(go|come|see|make|take|give|know|think|want|need|like|love|help|try|use|get|put|set|run|walk|talk|work|play|read|write|speak|say|tell|ask|answer|call|meet|start|stop|open|close|turn|move|keep|leave|arrive|return|finish|begin|continue|happen|seem|feel|look|sound|taste|smell)\b", re.I),
    re.compile(r"\b(yesterday|last|ago)\b.*\b(is|am|are|was)\b|\b(is|am|are)\b.*\b(yesterday|last|ago)\b", re.I),
    re.compile(r"\b(was|were|did|didn't)\s+(go|come|see|make|take|give|know|think|want|need|like|love|help|try|use|get|put|set|run|walk|talk|work|play|read|write|speak|say|tell|ask|answer|call|meet|start|stop|open|close|turn|move|keep|leave|arrive|return|finish|begin|continue|happen|seem|feel|look|sound|taste|smell)\b", re.I),
    re.compile(r"的(了|着)|了(的|地)|的地(得|的)"),
    re.compile(r"把(他|她|它|你|我)(们)?(是|有|在|很|太|最)"),
    re.compile(r"(非常|很|太|最)的(是|有|在)"),
    re.compile(r"([一-龥])\1\1"),
]

FEATURE_NAMES = [
    "len", "words", "cjk_ratio", "unique_ratio", "repeat_ratio",
    "punct_ratio", "upper_ratio", "digit_ratio",
    *[f"pat{i}" for i in range(len(_ERROR_PATTERNS))],
    "hit_collocation", "hit_word_order", "hit_missing", "hit_total",
    "rare_bigram", "ngram_oov",
]

# 稀有 bigram 背景表（CEC 混合池的干净侧 ref 构建，prepare_l3_pool.py 产出）：
# 错别字句会制造干净语料中不存在的字对 → rare_bigram 高；干净句 ≈ 0
_BG_DIR = os.path.join(os.path.dirname(__file__), "l3_bg")
_BG_CACHE: dict[str, set[str]] = {}


def load_bg(lang: str) -> set[str]:
    """加载 {lang}_bigrams.json（常见 bigram 集合）；缺失返回空集（特征恒 0）。"""
    if lang not in _BG_CACHE:
        path = os.path.join(_BG_DIR, f"{lang}_bigrams.json")
        bg: set[str] = set()
        if os.path.exists(path):
            with open(path, encoding="utf-8") as f:
                bg = set(json.load(f))
        _BG_CACHE[lang] = bg
    return _BG_CACHE[lang]


def rare_bigram_ratio(text: str, bg: set[str]) -> float:
    """不在干净背景表中的字对占比（拼写错误的聚合级代理信号）"""
    if not bg or len(text) < 2:
        return 0.0
    tot = len(text) - 1
    rare = sum(1 for i in range(tot) if text[i:i + 2] not in bg)
    return rare / tot


# ─── char 3-gram OOV（Phase 2.2） ───
_NGRAM_TABLE: dict[str, float] | None = None

def _load_ngram_table() -> dict[str, float]:
    global _NGRAM_TABLE
    if _NGRAM_TABLE is not None:
        return _NGRAM_TABLE
    path = os.path.join(os.path.dirname(__file__), "ngram_table.json")
    if os.path.isfile(path):
        with open(path, encoding="utf-8") as f:
            _NGRAM_TABLE = json.load(f)
    else:
        _NGRAM_TABLE = {}
    return _NGRAM_TABLE

def _ngram_oov(text: str) -> float:
    """句级特征 ngram_oov：未加权 OOV 比例（0=全正常, 1=全异常）。

    注意（方案 B 2026-10-18）：此处**故意**保持未加权/未平滑口径——特征必须
    与已训练模型一致（train.features.jsonl 按此口径提取）。启发式门控的
    hard/soft 加权口径见 correct.zh_ngram_oov_ratio，两者用途不同。
    换表后本特征分布已变（旧表 OOV≈0.8 是小表假象）；zh 模型重新训练（C）
    时须用新表重新提取特征。
    """
    table = _load_ngram_table()
    if not table:
        return 0.0
    zh = [c for c in text if "\u4e00" <= c <= "\u9fff"]
    if len(zh) < 3:
        return 0.0
    total = oov = 0
    for i in range(len(zh) - 2):
        tri = zh[i] + zh[i+1] + zh[i+2]
        total += 1
        if tri not in table:
            oov += 1
    return oov / total if total else 0.0


def extract_features(text: str, lang: str = "en") -> list[float]:
    """句级特征向量（extract / train / server 推理共用）"""
    n = max(1, len(text))
    words = re.findall(r"[\w']+", text)
    cjk = sum(1 for c in text if "\u4e00" <= c <= "\u9fff")
    punct = sum(1 for c in text if not c.isalnum())
    upper = sum(1 for c in text if c.isupper())
    digit = sum(1 for c in text if c.isdigit())
    feat = [
        math.log1p(n),
        math.log1p(len(words)),
        cjk / n,
        (len(set(words)) / len(words)) if words else 0.0,
        1.0 - (len(set(words)) / len(words)) if words else 0.0,
        punct / n,
        upper / n,
        digit / n,
    ]
    feat.extend(1.0 if p.search(text) else 0.0 for p in _ERROR_PATTERNS)
    # correct.py 模式命中数（与服务端 span 定位同源）
    try:
        hits = correct.find_hits(lang, text)
        by_type = Counter(h.error_type for h in hits)
        feat.extend([
            float(by_type.get("collocation", 0)),
            float(by_type.get("word_order", 0)),
            float(by_type.get("missing", 0)),
            float(len(hits)),
        ])
    except Exception:  # noqa: BLE001 — 模式库异常不应阻断训练
        feat.extend([0.0] * 4)
    # 稀有 bigram 占比（拼写错误的聚合级代理；背景表缺失时恒 0）
    feat.append(rare_bigram_ratio(text, load_bg(lang)))
    # char 3-gram OOV 比例（Phase 2.2：最强单特征，clean≈0 / error≈0.8）
    feat.append(_ngram_oov(text))
    return feat


def load_rows(path: str) -> list[dict]:
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def cmd_extract(args: argparse.Namespace) -> None:
    os.makedirs(args.out, exist_ok=True)
    total = 0
    for split in ("train", "dev"):
        src = os.path.join(args.data, f"{split}.jsonl")
        if not os.path.exists(src):
            print(f"[extract] 跳过（不存在）: {src}")
            continue
        rows = load_rows(src)
        out_path = os.path.join(args.out, f"{split}.features.jsonl")
        with open(out_path, "w", encoding="utf-8") as f:
            for r in rows:
                f.write(
                    json.dumps(
                        {
                            "x": extract_features(r["text"], args.lang),
                            "y": r["label"],
                            "error_type": r.get("error_type", ""),
                            "text": r["text"],
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
        total += len(rows)
        print(f"[extract] {out_path}: {len(rows)} 句")
    print(f"[extract] 完成：{total} 句 → {args.out}（特征维 {len(FEATURE_NAMES)}）")


# ─── 训练 ───


def _train_vegaml(X: list[list[float]], y: list[int], out_dir: str) -> bool:
    """vegaml 可用时走其训练入口；不可用返回 False（调用方回退 sklearn）"""
    try:
        import vegaml  # type: ignore[import]
    except ImportError:
        return False
    try:
        model = vegaml.train(X, y)
        os.makedirs(out_dir, exist_ok=True)
        vegaml.save(model, out_dir)
        print(f"[train] vegaml 模型已保存: {out_dir}")
        return True
    except Exception as e:  # noqa: BLE001
        print(f"[train] vegaml 训练失败（{e}），回退 sklearn", file=sys.stderr)
        return False


def _train_sklearn(X: list[list[float]], y: list[int], out_dir: str, rows: list[dict]) -> None:
    """回退模型：句级二分类 + 错误类型多分类（joblib 打包）"""
    import joblib
    from sklearn.linear_model import LogisticRegression

    os.makedirs(out_dir, exist_ok=True)
    bin_model = LogisticRegression(max_iter=2000)
    bin_model.fit(X, y)

    # 类型多分类：仅用正样本训练
    pos_idx = [i for i, label in enumerate(y) if label == 1]
    types = [rows[i].get("error_type") or "grammar" for i in pos_idx]
    type_model = None
    if len(set(types)) > 1:
        type_model = LogisticRegression(max_iter=2000)
        type_model.fit([X[i] for i in pos_idx], types)
    bundle = {
        "bin": bin_model,
        "types": type_model,
        "feature_names": FEATURE_NAMES,
        "classes": sorted(set(types)) if type_model else [],
    }
    joblib.dump(bundle, os.path.join(out_dir, "model.joblib"))
    print(f"[train] sklearn 回退模型已保存: {out_dir}（类型类 {bundle['classes']}）")


def cmd_train(args: argparse.Namespace) -> None:
    # 优先读 extract 产出的特征文件（{data}/train.features.jsonl），回退原始 gold 行
    src = os.path.join(args.data, "train.features.jsonl")
    if not os.path.exists(src):
        src = os.path.join(args.data, "train.jsonl")
    if not os.path.exists(src):
        sys.exit(f"[train] 找不到训练数据: {src}（先运行 extract）")
    rows = load_rows(src)
    X = [r["x"] for r in rows]
    y = [r["y"] for r in rows]
    out_dir = os.path.join(args.models_dir, args.lang)
    if _train_vegaml(X, y, out_dir):
        return
    _train_sklearn(X, y, out_dir, rows)


# ─── 评测 ───


def compute_f1(bundle: dict, dev_path: str) -> dict | None:
    """在 dev 特征集上计算句级二分类 acc/P/R/F1（飞轮 retrain 前后对比共用）。

    返回 `{acc, precision, recall, f1, n, tp, fp, fn, tn}`；
    dev 集缺失 / 模型无 predict_proba 时返回 None（调用方按「无法评测」处理）。
    """
    if not os.path.exists(dev_path):
        return None
    model = bundle.get("bin")
    if model is None or not hasattr(model, "predict_proba"):
        return None
    rows = load_rows(dev_path)
    if not rows:
        return None
    X = [r["x"] for r in rows]
    y_true = [r["y"] for r in rows]
    n_exp = getattr(model, "n_features_in_", None)
    if X and n_exp is not None and len(X[0]) != n_exp:
        # 特征文件过期（FEATURE_NAMES 增维后未重抽）：从行内 text 重抽，
        # lang 由 dev 路径的父目录名推断（l3_data/<lang>/…）。
        # 不重抽直接补零会让 F1 失真（review P0-2 连带修复）。
        lang = os.path.basename(os.path.dirname(dev_path))
        if not all("text" in r for r in rows):
            return None
        X = [extract_features(r["text"], lang) for r in rows]
    proba = model.predict_proba(X)
    y_prob = [row[1] if len(row) > 1 else 0.0 for row in proba]
    y_pred = [1 if p >= 0.5 else 0 for p in y_prob]

    tp = sum(1 for t, p in zip(y_true, y_pred) if t == 1 and p == 1)
    fp = sum(1 for t, p in zip(y_true, y_pred) if t == 0 and p == 1)
    fn = sum(1 for t, p in zip(y_true, y_pred) if t == 1 and p == 0)
    tn = sum(1 for t, p in zip(y_true, y_pred) if t == 0 and p == 0)
    acc = (tp + tn) / max(1, len(y_true))
    prec = tp / max(1, tp + fp)
    rec = tp / max(1, tp + fn)
    f1 = 2 * prec * rec / max(1e-9, prec + rec)
    # 逐错误类型召回（仅 label=1，按 error_type 分组）：
    # retrain 门用它防止「整体 F1 达标但某类召回悄悄退化」（飞轮固化 LLM 盲区）
    by_class: dict[str, list[int]] = {}
    for r, p in zip(rows, y_pred):
        if r["y"] == 1:
            by_class.setdefault(r.get("error_type") or "grammar", []).append(p)
    per_class = {
        et: {
            "n": len(preds),
            "recall": round(sum(preds) / len(preds), 4),
        }
        for et, preds in by_class.items()
    }
    return {
        "acc": round(acc, 4),
        "precision": round(prec, 4),
        "recall": round(rec, 4),
        "f1": round(f1, 4),
        "n": len(y_true),
        "tp": tp,
        "fp": fp,
        "fn": fn,
        "tn": tn,
        "per_class": per_class,
    }


def cmd_eval(args: argparse.Namespace) -> None:
    import joblib

    model_path = os.path.join(args.models_dir, args.lang, "model.joblib")
    if not os.path.exists(model_path):
        sys.exit(f"[eval] 找不到模型: {model_path}（先运行 train；vegaml 原生模型暂不支持 eval）")
    bundle = joblib.load(model_path)

    dev = os.path.join(args.data, "dev.features.jsonl")
    if not os.path.exists(dev):
        dev = os.path.join(args.data, "dev.jsonl")
    if not os.path.exists(dev):
        sys.exit(f"[eval] 找不到评测数据: {dev}（先运行 extract）")

    report = compute_f1(bundle, dev)
    if report is None:
        sys.exit(f"[eval] 无法评测（模型无 predict_proba 或 dev 集为空）")
    print(
        f"[eval] dev {report['n']} 句: acc={report['acc']:.3f} P={report['precision']:.3f}"
        f" R={report['recall']:.3f} F1={report['f1']:.3f}"
    )
    print(f"[eval] 混淆: TP={report['tp']} FP={report['fp']} FN={report['fn']} TN={report['tn']}")

    # 错误类型召回分布（仅 label=1）
    rows = load_rows(dev)
    X = [r["x"] for r in rows]
    proba = bundle["bin"].predict_proba(X)
    y_pred = [1 if (row[1] if len(row) > 1 else 0.0) >= 0.5 else 0 for row in proba]
    by_type: dict[str, list[int]] = {}
    for r, p in zip(rows, y_pred):
        if r["y"] == 1:
            by_type.setdefault(r.get("error_type", "grammar"), []).append(p)
    for etype, preds in sorted(by_type.items(), key=lambda kv: -len(kv[1])):
        hit = sum(preds)
        print(f"[eval]   {etype}: {hit}/{len(preds)} 召回")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("extract", help="gold 数据 → 特征向量")
    p.add_argument("--lang", required=True, choices=["en", "zh"])
    p.add_argument("--data", required=True, help="prepare_data 输出目录（含 {train,dev}.jsonl）")
    p.add_argument("--out", required=True, help="特征输出目录")
    p.set_defaults(func=cmd_extract)

    p = sub.add_parser("train", help="训练句级二分类 + 类型多分类模型")
    p.add_argument("--lang", required=True, choices=["en", "zh"])
    p.add_argument("--data", required=True)
    p.add_argument("--models-dir", default=os.path.join(os.path.dirname(__file__), "models"))
    p.set_defaults(func=cmd_train)

    p = sub.add_parser("eval", help="dev 集评测")
    p.add_argument("--lang", required=True, choices=["en", "zh"])
    p.add_argument("--data", required=True)
    p.add_argument("--models-dir", default=os.path.join(os.path.dirname(__file__), "models"))
    p.set_defaults(func=cmd_eval)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()