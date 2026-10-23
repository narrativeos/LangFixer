#!/usr/bin/env python3
"""门控 A/B 评测 harness：对 /v1/systemone 兼容端点跑中文「有无错误」门控评测。

臂（arm）：
  kev4b   → ollama turintech/kev-4b-artemis（Qwen3.5-4B 底座 + 决策 LoRA）
  laya_en → ollama laya:latest（ModernBERT-large，英文检查点，参照臂）
  laya_ml → 本地 laya shim（convaiinnovations/laya-multilingual，mmBERT 多语言）
  sklearn → 旧决策模型（对照；其训练池=MuC dev，本评测集与之零重叠）

指标：AUC / ECE / 阈值扫描 / 操作点（clean-skip 漏检率 ≤2%、≤5% 时的跳过率）/ 延迟。
温度校准：折半拟合 T，在另一半上报告校准前后 ECE。

用法（推理）：
  .venv/bin/python gate_eval.py run --arm kev4b [--limit N]
  .venv/bin/python gate_eval.py run --arm laya_ml --endpoint http://127.0.0.1:11435 --model laya-multilingual
  .venv/bin/python gate_eval.py run --arm sklearn
评分：
  .venv/bin/python gate_eval.py score --arm kev4b
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
EVAL_FILE = os.path.join(HERE, "gate_eval", "zh_gate_1k.jsonl")

# 统一问题 schema：两选项 choice（模型卡建议：noul 会低估 true）
QUESTIONS = {
    "has_error": {
        "type": "choice",
        "instructions": "Does the state text contain a language error that needs correction (typo, grammar, punctuation, or word usage)?",
        "criteria": {
            "A": "The state text contains a language error that needs correction.",
            "B": "The state text is correct and needs no correction.",
        },
    }
}

ARMS = {
    "kev4b": {"endpoint": "http://127.0.0.1:11434", "model": "turintech/kev-4b-artemis"},
    "kev9b": {"endpoint": "http://127.0.0.1:11434", "model": "turintech/kev-9b-artemis"},
    "laya_en": {"endpoint": "http://127.0.0.1:11434", "model": "laya"},
    "laya_ml": {"endpoint": "http://127.0.0.1:11435", "model": "laya-multilingual"},
}


def systemone_perror(endpoint: str, model: str, text: str, timeout: int = 120) -> tuple[float, float]:
    import urllib.request

    body = json.dumps({"model": model, "state": text, "questions": QUESTIONS}).encode()
    req = urllib.request.Request(f"{endpoint}/v1/systemone", data=body,
                                 headers={"Content-Type": "application/json"})
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        data = json.loads(resp.read())
    ms = (time.time() - t0) * 1000
    probs = data["answers"]["has_error"]["probabilities"]
    return float(probs["A"]), ms


def perror_sklearn(text: str) -> tuple[float, float]:
    import joblib

    sys.path.insert(0, HERE)
    from vega_finetune import extract_features  # 旧特征提取（16 维 surface 特征）

    bundle = joblib.load(os.path.join(HERE, "models", "zh", "model.joblib"))
    model = bundle["model"]
    x = np.array([extract_features(text)], dtype=float)
    t0 = time.time()
    p = float(model.predict_proba(x)[0, 1])
    return p, (time.time() - t0) * 1000


def cmd_run(args: argparse.Namespace) -> None:
    rows = [json.loads(l) for l in open(os.path.join(os.path.dirname(EVAL_FILE), args.dataset), encoding="utf-8")]
    if args.limit:
        rows = rows[: args.limit]
    out_path = args.out or os.path.join(HERE, "gate_eval", f"results_{args.arm}.jsonl")
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    done = set()
    if os.path.exists(out_path):
        for l in open(out_path, encoding="utf-8"):
            done.add(json.loads(l)["text"])

    if args.arm == "sklearn":
        fn = perror_sklearn
    else:
        cfg = ARMS[args.arm]
        endpoint, model = args.endpoint or cfg["endpoint"], args.model or cfg["model"]
        fn = lambda t: systemone_perror(endpoint, model, t)

    f = open(out_path, "a", encoding="utf-8")
    n = 0
    for r in rows:
        if r["text"] in done:
            continue
        try:
            p, ms = fn(r["text"])
        except Exception as e:
            print(f"[run] 跳过（调用失败）: {e}", file=sys.stderr)
            continue
        f.write(json.dumps({"text": r["text"], "label": r["label"], "source": r["source"],
                            "p_error": p, "ms": ms}, ensure_ascii=False) + "\n")
        f.flush()
        n += 1
        if n % 50 == 0:
            print(f"[run] {args.arm}: {n} 句")
    f.close()
    print(f"[run] {args.arm}: 新增 {n} → {out_path}")


def auc(y: np.ndarray, p: np.ndarray) -> float:
    order = np.argsort(p)
    ranks = np.empty(len(p))
    ranks[order] = np.arange(1, len(p) + 1)
    n1, n0 = int(y.sum()), int((1 - y).sum())
    return float((ranks[y == 1].sum() - n1 * (n1 + 1) / 2) / (n1 * n0))


def ece(y: np.ndarray, p: np.ndarray, bins: int = 10) -> float:
    total, e = len(p), 0.0
    for i in range(bins):
        lo, hi = i / bins, (i + 1) / bins
        m = (p >= lo) & (p < hi) if i < bins - 1 else (p >= lo) & (p <= hi)
        if m.sum():
            e += m.sum() / total * abs(p[m].mean() - y[m].mean())
    return float(e)


def fit_temperature(y: np.ndarray, p: np.ndarray) -> float:
    """折半拟合温度（最小化 NLL）。"""
    def nll(T: float, pp: np.ndarray) -> float:
        lo = np.log(np.clip(pp, 1e-9, 1)) - np.log1p(-pp + 1e-9)
        z = lo / T
        return float(-np.mean(y * z - np.logaddexp(0, z)))

    best, best_t = nll(1.0, p), 1.0
    for T in np.arange(0.25, 8.01, 0.05):
        v = nll(float(T), p)
        if v < best:
            best, best_t = v, float(T)
    return best_t


def apply_T(p: np.ndarray, T: float) -> np.ndarray:
    lo = np.log(np.clip(p, 1e-9, 1)) - np.log1p(-p + 1e-9)
    z = lo / T
    return 1.0 / (1.0 + np.exp(-z))


def operating_points(y: np.ndarray, p: np.ndarray) -> None:
    print("  阈值扫描（clean-skip：p<τ 判干净→跳过 LLM）")
    print("   τ    跳过率  漏检率  漏检句数")
    for tau in [0.10, 0.15, 0.20, 0.25, 0.30, 0.40, 0.50]:
        skip = p < tau
        if not skip.sum():
            print(f"  {tau:.2f}    0.0%     -      -")
            continue
        leak = int(y[skip].sum())
        print(f"  {tau:.2f}   {skip.mean()*100:5.1f}%  {leak/skip.sum()*100:5.1f}%  {leak:4d}")
    for budget in (0.02, 0.05):
        best_tau, best_rate = None, -1.0
        for tau in np.arange(0.01, 0.91, 0.01):
            skip = p < tau
            if skip.sum() == 0:
                continue
            if y[skip].sum() / skip.sum() <= budget and skip.mean() > best_rate:
                best_rate, best_tau = skip.mean(), float(tau)
        if best_tau is not None:
            print(f"  操作点 漏检率≤{budget:.0%}: τ={best_tau:.2f} 跳过率={best_rate*100:.1f}%")


def cmd_score(args: argparse.Namespace) -> None:
    path = args.results or os.path.join(HERE, "gate_eval", f"results_{args.arm}.jsonl")
    rows = [json.loads(l) for l in open(path, encoding="utf-8")]
    y = np.array([r["label"] for r in rows])
    p = np.array([r["p_error"] for r in rows])
    ms = np.array([r["ms"] for r in rows])
    print(f"[score] {args.arm or path}: n={len(rows)} 有错率={y.mean():.3f}")
    print(f"  AUC={auc(y, p):.3f}  ECE(raw)={ece(y, p):.3f}")
    rng = np.random.RandomState(7)
    idx = rng.permutation(len(p))
    a, b = idx[: len(p) // 2], idx[len(p) // 2:]
    T = fit_temperature(y[a], p[a])
    pc = apply_T(p[b], T)
    print(f"  温度: 拟合 T={T:.2f} → 折外 ECE {ece(y[b], p[b]):.3f} → {ece(y[b], pc):.3f}")
    operating_points(y, p)
    print(f"  延迟 p50={np.percentile(ms,50):.0f}ms p95={np.percentile(ms,95):.0f}ms")
    for src in sorted({r["source"] for r in rows}):
        m = np.array([r["source"] == src for r in rows])
        print(f"  [{src}] n={m.sum()} AUC={auc(y[m], p[m]):.3f}")


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--arm", required=True, choices=list(ARMS) + ["sklearn"])
    r.add_argument("--endpoint")
    r.add_argument("--model")
    r.add_argument("--limit", type=int)
    r.add_argument("--out")
    r.add_argument("--set", dest="dataset", default="zh_gate_1k.jsonl")
    r.set_defaults(fn=cmd_run)
    s = sub.add_parser("score")
    s.add_argument("--arm", choices=list(ARMS) + ["sklearn"])
    s.add_argument("--results")
    s.set_defaults(fn=cmd_score)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
