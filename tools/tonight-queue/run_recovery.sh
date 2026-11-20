#!/bin/bash
# 恢复链（watcher #2，排在 run_tonight.sh 之后）：
#   A. v2.3 续跑至无端点失败行（journal 续，最多两轮）
#   B. expand 补跑（回收 ~311 端点失败行）+ 自动合并
#   C. eval/归因三刷（_v3 = 全量测量面终版）
# 启动（key 走启动 env，不落盘）：
#   LANGFIXER_LLM_KEY=… nohup bash tools/tonight-queue/run_recovery.sh > ~/tmp/tonight_recovery.log 2>&1 &
set -uo pipefail
R=$HOME/github/narrativeos/LangFixer
TG=$HOME/tmp/true_gold
FM=$R/feature-model
GP=$HOME/tmp/gate-probe
L() { echo "[recovery $(date '+%H:%M:%S')] $*"; }

LABELED="--labels $TG/router_labels_train_newgold.jsonl --llm-cache /tmp/router_llm_arm_gold.jsonl --only-cached"
CNTS="--counts $TG/gold_counts.jsonl --gain-cache /tmp/router_llm_arm_train.jsonl"

L "等 run_tonight.sh 收工…"
while pgrep -f "run_tonight.sh" >/dev/null 2>&1; do sleep 120; done

L "A. v2.3 rollout 续跑检查"
cd "$R"
for round in 1 2; do
  VER=v23 GP_DATA=$GP OUT=$HOME/tmp/dataset_v23 \
    python3 tools/label-rollout/label_rollout.py > /tmp/v23_round$round.log 2>&1
  if grep -q "因端点错误未完成" /tmp/v23_round$round.log; then
    L "  第 $round 轮仍有端点失败行，journal 续跑再来一轮"
  else
    L "  v2.3 无端点失败行，完成"
    break
  fi
done
grep -E "共识|新增|漏标" /tmp/v23_round${round}.log | tail -2

L "B. expand 补跑（回收失败行；无待跑则秒退）"
(cd "$FM" && python3 ../tools/train-cache-expand/expand.py --workers 4 > /tmp/expand_resume.log 2>&1)
tail -3 /tmp/expand_resume.log
L "  合并后缓存行数=$(wc -l < /tmp/router_llm_arm_train.jsonl)"

L "C. eval/归因三刷（_v3 终版）"
(cd "$FM" && .venv/bin/python3 eval_router_arms.py $LABELED $CNTS > $TG/eval_router_ev_cache_v3.txt 2>$TG/eval_router_ev_cache_v3.err)
(cd "$FM" && .venv/bin/python3 eval_router_arms.py $LABELED $CNTS --ev-uniform-p > $TG/eval_router_ev_uniformp_v3.txt 2>$TG/eval_router_ev_uniformp_v3.err)
grep -E "总体判定" $TG/eval_router_ev_cache_v3.txt | tail -2
python3 "$R/tools/type-layer-experiments/attribute_p.py" > $TG/p_attribution_v3.txt 2>&1
tail -3 $TG/p_attribution_v3.txt

L "全部收工。明早裁决页：python3 tools/label-adjudicate/serve.py --data-dir ~/tmp/dataset_v23"
