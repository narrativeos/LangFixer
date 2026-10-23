#!/usr/bin/env python3
"""决策模型判别力体检：加载 models/<lang>/model.joblib，在 dev 集上 dump P(error) 分布。
与 EVOLUTION.md Phase 0.2 对 L3 的体检同构。"""
import json
import sys
import numpy as np
import joblib

lang = sys.argv[1] if len(sys.argv) > 1 else "zh"
bundle = joblib.load(f"models/{lang}/model.joblib")
model = bundle["model"] if isinstance(bundle, dict) else bundle
feats = json.load(open(f"vegaml_data/{lang}/feature_names.json"))
names = feats if isinstance(feats, list) else feats.get("feature_names", [])
rows = [json.loads(l) for l in open(f"vegaml_data/{lang}/dev.jsonl")]

X = np.array([r["x"] for r in rows], dtype=float)
proba = model.predict_proba(X)[:, 1]
y = np.array([r["y"] for r in rows])

print(f"[{lang}] dev={len(rows)} 有错率={y.mean():.3f} backend={type(model).__name__} n_feat={len(names)}")
print(f"P(error): min={proba.min():.3f} p25={np.percentile(proba,25):.3f} "
      f"med={np.median(proba):.3f} p75={np.percentile(proba,75):.3f} max={proba.max():.3f}")
# AUC
order = np.argsort(proba)
ranks = np.empty(len(proba)); ranks[order] = np.arange(1, len(proba)+1)
n1, n0 = int(y.sum()), int((1-y).sum())
auc = (ranks[y==1].sum() - n1*(n1+1)/2) / (n1*n0)
print(f"AUC={auc:.3f}  (n_pos={n1} n_neg={n0})")
# 概率分布带（对照 L3 的 [0.40,1.0] 平台）
for lo, hi in [(0,0.25),(0.25,0.4),(0.4,0.5),(0.5,0.7),(0.7,0.9),(0.9,1.01)]:
    m = (proba>=lo)&(proba<hi)
    if m.sum():
        print(f"  [{lo:.2f},{hi:.2f}): {m.sum():4d} 句, gold率={y[m].mean():.3f}")
# 阈值 0.5 门控口径（决策层实际用法：prob>=0.5 判定有错，否则弃权/放行）
pred = proba >= 0.5
tp = int(((pred)&(y==1)).sum()); fp = int(((pred)&(y==0)).sum())
fn = int(((~pred)&(y==1)).sum()); tn = int(((~pred)&(y==0)).sum())
p = tp/max(tp+fp,1); r = tp/max(tp+fn,1)
print(f"τ=0.5 门控: P={p:.3f} R={r:.3f} F1={2*p*r/max(p+r,1e-9):.3f}  (TP={tp} FP={fp} FN={fn} TN={tn})")
