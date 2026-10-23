#!/usr/bin/env python3
"""zh 决策模型门控评测（工作流 C，2026-10-18）。

问题：决策模型（l3_finetune 句级二分类，22 维聚合特征）能否替代/加强
启发式门控（`L3_GRAY_ZONE_LOW`），把 MuCGEC 弃权率进一步放宽（0.40 档）？

方法：模型对四个数据集打分（CSCD/NaCGEC/MuCGEC gold + confirmed 真实用户
clean，全部已从训练池与 n-gram 表去污染），与启发式对照：
- AUC（err vs clean，跨域）
- 门控操作点：clean 放行率（成本收益）vs err 放行率（漏检）

结论（2026-10-18，新表特征重训练后 dev F1=0.549）：
- 模型跨域门控**不可用**：AUC cscd 0.63 / nacgec 0.50 / mucgec 0.50；
  概率整体偏高（几乎全部 ≥0.4），clean 放行率 ≈0%——dev F1 掩盖了
  「概率分布不分离」的本质（README「聚合特征无法区分语法错句 vs 干净句」）
- max(模型, 启发式) 被模型高概率吞没，同样不可用
- 因此门控保持启发式（correct.zh_heuristic_prob）；决策模型仅保留
  类型分类用途。进一步放宽弃权率的真正兜底是**字级判别模型**
  （CEC ELECTRA discriminator，见 CEC_ADOPTION.md D.2，未做）

用法：
    .venv/bin/python eval_zh_model_gate.py            # 需先 train 出 models/zh
"""

from __future__ import annotations

import bisect
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))

import joblib  # noqa: E402

import correct  # noqa: E402
import l3_finetune as LF  # noqa: E402


def auc(pos: list[float], neg: list[float]) -> float:
    neg_s = sorted(neg)
    tot = 0.0
    for a in pos:
        lo = bisect.bisect_left(neg_s, a)
        hi = bisect.bisect_right(neg_s, a)
        tot += lo + 0.5 * (hi - lo)
    return tot / (len(pos) * len(neg))


def main() -> None:
    model_path = HERE / "models" / "zh" / "model.joblib"
    if not model_path.is_file():
        sys.exit(f"找不到模型 {model_path}（先运行 l3_finetune.py train --lang zh）")
    bundle = joblib.load(model_path)
    bin_model = bundle["bin"]
    table, bg = correct.load_zh_ngram_tables(
        str(HERE / "ngram_table.json"), str(HERE / "bigram_table_zh.json"))

    def probs(sents: list[str]) -> list[float]:
        if not sents:
            return []
        X = [LF.extract_features(s, "zh") for s in sents]
        return [p[1] for p in bin_model.predict_proba(X)]

    def heur(s: str) -> float:
        return correct.zh_heuristic_prob(s, table, bg)

    groups: dict[str, dict[str, list[str]]] = {}
    for nm in ("cscd_gold.json", "nacgec_gold.json", "mucgec_gold.json"):
        g = json.load(open(REPO / "datasets" / nm, encoding="utf-8"))
        groups[nm] = {"err": [e["src"] for e in g if e.get("edits")],
                      "clean": [e["src"] for e in g if not e.get("edits")]}
    conf = []
    p = HERE / "l3_data" / "zh" / "confirmed.jsonl"
    if p.is_file():
        for ln in open(p, encoding="utf-8"):
            ln = ln.strip()
            if ln:
                r = json.loads(ln)
                if int(r.get("label", 1)) == 0:
                    conf.append(r["text"])
    groups["confirmed"] = {"err": [], "clean": conf}

    print("== AUC（err vs clean；跨域泛化）==")
    for nm, g in groups.items():
        if not g["err"] or not g["clean"]:
            continue
        mp, mq = probs(g["err"]), probs(g["clean"])
        hp, hq = [heur(s) for s in g["err"]], [heur(s) for s in g["clean"]]
        xp = [max(a, b) for a, b in zip(mp, hp)]
        xq = [max(a, b) for a, b in zip(mq, hq)]
        print(f"  {nm:20s} model={auc(mp, mq):.3f} heur={auc(hp, hq):.3f} max={auc(xp, xq):.3f}")

    print("\n== 门控操作点（放行 = prob < T；clean 放行=成本收益，err 放行=漏检）==")
    scores = {}
    for nm, g in groups.items():
        scores[nm] = (probs(g["err"]), probs(g["clean"]),
                      [heur(s) for s in g["err"]], [heur(s) for s in g["clean"]])
    for T in (0.20, 0.30, 0.40):
        print(f"\n  -- T={T} --")
        for nm, (m_err, m_cl, h_err, h_cl) in scores.items():
            def pr(v: list[float]) -> str:
                return f"{sum(1 for x in v if x < T) / max(1, len(v)):.0%}"
            x_err = [max(a, b) for a, b in zip(m_err, h_err)]
            x_cl = [max(a, b) for a, b in zip(m_cl, h_cl)]
            print(f"    {nm:20s} model: clean放行={pr(m_cl):>4s} err漏检={pr(m_err):>4s} | "
                  f"heur: clean放行={pr(h_cl):>4s} err漏检={pr(h_err):>4s} | "
                  f"max: clean放行={pr(x_cl):>4s} err漏检={pr(x_err):>4s}")


if __name__ == "__main__":
    main()
