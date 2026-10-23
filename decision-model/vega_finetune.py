#!/usr/bin/env python3
"""vegaml 适配工作流：extract（特征抽取）/ train（微调）/ eval（评测）。

数据流：
    prepare_data.py 产出 data/<lang>/{train,dev}.jsonl
        → extract:  每句 → 特征向量（句级二分类：有错误 / 无错误）
        → train:    vegaml 可用则用其训练；否则回退 sklearn LogisticRegression
                    （VEGAML_FALLBACK=1 强制回退），模型存 models/<lang>/
        → eval:     在 dev 集上报告 acc / precision / recall / F1 + 错误类型分布

用法：
    python vega_finetune.py extract --lang en --data data/en --out vegaml_data/en
    python vega_finetune.py train   --lang en --data data/en
    python vega_finetune.py eval    --lang en --data data/en

特征（与 vegaml 特征抽取解耦的通用句级特征，回退模型与 mock 共用）：
    len, words, cjk_ratio, unique_ratio, repeat_ratio, punct_ratio,
    upper_ratio, digit_ratio, 以及 8 个启发式错误模式命中数
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import sys
import time
from collections import Counter

# ─── 特征抽取 ───

# 通用错误信号模式（命中 → 特征置 1；与 server.py 启发式同源但更宽）
_ERROR_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"\b(teir|teh|recieve|seperat\w*|occured|writen|begining|comming|wich|whitch|becuase|tommorow|untill)\b", re.I),
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
]


def extract_features(text: str) -> list[float]:
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
            print(f"[extract] 跳过（不存在）: {src}", file=sys.stderr)
            continue
        rows = load_rows(src)
        out = os.path.join(args.out, f"{split}.jsonl")
        with open(out, "w", encoding="utf-8") as f:
            for r in rows:
                f.write(
                    json.dumps(
                        {
                            "x": extract_features(r["text"]),
                            "y": r["label"],
                            "error_type": r.get("error_type", ""),
                            "text": r["text"],
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
        total += len(rows)
        print(f"[extract] {out}: {len(rows)} 句 × {len(FEATURE_NAMES)} 维特征")
    with open(os.path.join(args.out, "feature_names.json"), "w", encoding="utf-8") as f:
        json.dump(FEATURE_NAMES, f, ensure_ascii=False)
    print(f"[extract] 完成：{total} 句 → {args.out}")


# ─── 训练 ───


def _next_version(out_dir: str) -> str:
    """模型版本号：每次训练自增（`v1`, `v2`, ...）。

    Rust 侧（api-server / 桌面端）经决策服务 `GET /health` 读取该版本并存入
    检测历史参数快照——重训练后版本变化，基于旧模型的历史结果不再被复用
    （「重复检测复用历史」的模型侧门槛）。
    """
    meta_path = os.path.join(out_dir, "meta.json")
    prev = 0
    if os.path.isfile(meta_path):
        try:
            with open(meta_path, encoding="utf-8") as f:
                v = str(json.load(f).get("version", "")).strip()
            if v.startswith("v") and v[1:].isdigit():
                prev = int(v[1:])
        except Exception:  # noqa: BLE001
            pass
    return f"v{prev + 1}"


def _write_meta(out_dir: str, backend: str, version: str) -> None:
    """写 `meta.json`（backend / version / trained_at / feature_names；保留既有字段）。"""
    meta_path = os.path.join(out_dir, "meta.json")
    meta: dict = {}
    if os.path.isfile(meta_path):
        try:
            with open(meta_path, encoding="utf-8") as f:
                meta = json.load(f)
        except Exception:  # noqa: BLE001
            meta = {}
    tz = time.strftime("%z") or "+0000"
    meta.update(
        {
            "backend": backend,
            "version": version,
            # ISO 8601 带冒号时区（`%z` 原生输出无冒号，手动补上）
            "trained_at": time.strftime("%Y-%m-%dT%H:%M:%S") + f"{tz[:3]}:{tz[3:]}",
            "feature_names": FEATURE_NAMES,
        }
    )
    with open(meta_path, "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False)


def _train_vegaml(X: list[list[float]], y: list[int], out_dir: str, version: str) -> bool:
    """vegaml 可用时调用其训练入口；不可用返回 False（调用方回退 sklearn）。"""
    if os.environ.get("VEGAML_FALLBACK", "0") == "1":
        return False
    try:
        import vegaml  # type: ignore[import]
    except ImportError:
        return False
    try:
        model = vegaml.train(X, y, feature_names=FEATURE_NAMES)
        os.makedirs(out_dir, exist_ok=True)
        vegaml.save(model, out_dir)
        _write_meta(out_dir, "vegaml", version)
        print(f"[train] vegaml 模型已保存: {out_dir}（版本 {version}）")
        return True
    except Exception as e:  # noqa: BLE001
        print(f"[train] vegaml 训练失败（{e}），回退 sklearn", file=sys.stderr)
        return False


def _train_sklearn(X: list[list[float]], y: list[int], out_dir: str, version: str) -> None:
    from sklearn.linear_model import LogisticRegression
    from sklearn.metrics import f1_score, precision_score, recall_score

    # gold 数据 95%+ 是错误句（GEC 数据集天然如此），不校正先验的话
    # 模型会几乎把所有句子判为错误（干净句 FP 极高）→ 用 balanced 权重校正
    model = LogisticRegression(max_iter=2000, class_weight="balanced")
    model.fit(X, y)
    p, r = precision_score(y, model.predict(X), zero_division=0), recall_score(
        y, model.predict(X), zero_division=0
    )
    f1 = f1_score(y, model.predict(X), zero_division=0)
    print(f"[train] sklearn LogisticRegression 训练集指标: P={p:.3f} R={r:.3f} F1={f1:.3f}")
    os.makedirs(out_dir, exist_ok=True)
    import joblib

    joblib.dump({"model": model, "feature_names": FEATURE_NAMES}, os.path.join(out_dir, "model.joblib"))
    _write_meta(out_dir, "sklearn", version)
    print(f"[train] 模型已保存: {out_dir}（版本 {version}）")


def cmd_train(args: argparse.Namespace) -> None:
    src = os.path.join(args.data, "train.jsonl")
    if not os.path.exists(src):
        src = os.path.join(args.data, "train.features.jsonl")
    if not os.path.exists(src):
        sys.exit(f"[train] 找不到训练数据: {src}（先运行 extract）")
    rows = load_rows(src)
    X = [r["x"] for r in rows]
    y = [r["y"] for r in rows]
    out_dir = os.path.join(args.models_dir, args.lang)
    version = _next_version(out_dir)
    if _train_vegaml(X, y, out_dir, version):
        return
    _train_sklearn(X, y, out_dir, version)


# ─── 评测 ───


def cmd_eval(args: argparse.Namespace) -> None:
    import joblib

    model_path = os.path.join(args.models_dir, args.lang, "model.joblib")
    if not os.path.exists(model_path):
        sys.exit(f"[eval] 找不到模型: {model_path}（先运行 train；vegaml 原生模型暂不支持 eval）")
    bundle = joblib.load(model_path)
    model = bundle["model"]

    dev = os.path.join(args.data, "dev.jsonl")
    if not os.path.exists(dev):
        dev = os.path.join(args.data, "dev.features.jsonl")
    if not os.path.exists(dev):
        sys.exit(f"[eval] 找不到评测数据: {dev}（先运行 extract）")
    rows = load_rows(dev)
    X = [r["x"] for r in rows]
    y_true = [r["y"] for r in rows]
    y_prob = model.decision_function(X) if hasattr(model, "decision_function") else model.predict_proba(X)[:, 1]
    y_pred = [1 if p >= 0.5 else 0 for p in y_prob]

    tp = sum(1 for t, p in zip(y_true, y_pred) if t == 1 and p == 1)
    fp = sum(1 for t, p in zip(y_true, y_pred) if t == 0 and p == 1)
    fn = sum(1 for t, p in zip(y_true, y_pred) if t == 1 and p == 0)
    tn = sum(1 for t, p in zip(y_true, y_pred) if t == 0 and p == 0)
    acc = (tp + tn) / max(1, len(y_true))
    prec = tp / max(1, tp + fp)
    rec = tp / max(1, tp + fn)
    f1 = 2 * prec * rec / max(1e-9, prec + rec)
    print(f"[eval] dev {len(y_true)} 句: acc={acc:.3f} P={prec:.3f} R={rec:.3f} F1={f1:.3f}")
    print(f"[eval] 混淆: TP={tp} FP={fp} FN={fn} TN={tn}")

    # 错误类型召回分布（仅 label=1）
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

    p = sub.add_parser("train", help="训练句级二分类模型")
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

