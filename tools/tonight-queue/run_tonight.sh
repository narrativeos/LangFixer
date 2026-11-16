#!/bin/bash
# 今晚任务链（2026-11-16 排定）：expand 收工 → p̂ 重赛两遍 → 归因/wo_exempt → v2.3 rollout
# 启动（key 只走启动 env，不落盘）：
#   LANGFIXER_LLM_KEY=... nohup bash tools/tonight-queue/run_tonight.sh > ~/tmp/tonight.log 2>&1 &
set -uo pipefail
R=$HOME/github/narrativeos/LangFixer
TG=$HOME/tmp/true_gold
FM=$R/feature-model
L() { echo "[tonight $(date '+%H:%M:%S')] $*"; }

LABELED="--labels $TG/router_labels_train_newgold.jsonl --llm-cache /tmp/router_llm_arm_gold.jsonl --only-cached"
CNTS="--counts $TG/gold_counts.jsonl --gain-cache /tmp/router_llm_arm_train.jsonl"

L "phase1 等 train-cache-expand 收工（每 5 分钟查）"
while pgrep -f "train-cache-expand/expand.py" >/dev/null 2>&1; do sleep 300; done
L "expand 进程退出；缓存行数=$(wc -l < /tmp/router_llm_arm_train.jsonl)  expand侧=$(test -f /tmp/router_llm_arm_train.jsonl.expand && wc -l < /tmp/router_llm_arm_train.jsonl.expand || echo 无)"
tail -4 /tmp/train_cache_expand.log

L "phase2 eval 重赛（真 p̂）"
(cd "$FM" && .venv/bin/python3 eval_router_arms.py $LABELED $CNTS > $TG/eval_router_ev_cache_v2.txt 2>$TG/eval_router_ev_cache_v2.err)
grep -E "总体判定|63.1% ←锚" $TG/eval_router_ev_cache_v2.txt | tail -3

L "phase2b 归因对照（--ev-uniform-p）"
(cd "$FM" && .venv/bin/python3 eval_router_arms.py $LABELED $CNTS --ev-uniform-p > $TG/eval_router_ev_uniformp_v2.txt 2>$TG/eval_router_ev_uniformp_v2.err)
grep -E "总体判定|63.1% ←锚" $TG/eval_router_ev_uniformp_v2.txt | tail -2

L "phase3 p̂ 分侧归因 + wo_exempt 判定"
python3 "$R/tools/type-layer-experiments/attribute_p.py" | tee $TG/p_attribution_v2.txt

L "phase4 v2.3 rollout（VER=v23，journal 续跑；需启动 env 里有 LLM key）"
cd "$R"
VER=v23 GP_DATA=$HOME/tmp/gate-probe OUT=$HOME/tmp/dataset_v23 \
  python3 tools/label-rollout/label_rollout.py || L "WARN v2.3 退出码非零——重跑同命令即续（journal）"

L "全部收工。产物：eval_router_ev_{cache,uniformp}_v2.txt / p_attribution_v2.txt / ~/tmp/dataset_v23/"
