#!/usr/bin/env bash
# 三段式「重演专家结果」驱动（MULTIMODAL.md §18）
#
#   ① 推演：跑载体 → 检测 → 把推演结果写成 candidates.json（含溯源与 gold 序号）
#   ② 勾选：人工在 candidates.json 里把 `checked` 改成 false/true，直接改各列内容
#   ③ 导出：candidates.json → 与专家正误表同构的 report.docx / .md / .json / .html
#
# 用法：
#   ./scripts/media_expert_replay.sh replay     # ①（有凭据跑真实载体，否则跑离线上限评测）
#   $EDITOR target/media-replay/candidates.json # ②
#   ./scripts/media_expert_replay.sh export     # ③
#   ./scripts/media_expert_replay.sh all        # ①③（跳过人工勾选 = 全量导出）
#
# 真实重演（①）前提：
#   LANGFIXER_MEDIA_DIR=<存着 origin_pic_*.jpg 的目录>
#   LLM_API_KEY / LLM_BASE_URL / GEC_MEDIA_VISION_MODEL=<视觉模型名>
#   photocraft-cli 已安装（载体探测）；wordcraft-cli 已安装（③ 出 Word）
set -euo pipefail

cd "$(dirname "$0")/.."
REPLAY_DIR="${LANGFIXER_REPLAY_DIR:-target/media-replay}"
# 必须是绝对路径：cargo test 的工作目录是包目录（api-server/），
# 相对路径会写出 `api-server/target/...`，与脚本读的仓库根 `target/...` 不是同一个地方
case "$REPLAY_DIR" in
  /*) ;;
  *) REPLAY_DIR="$(pwd)/$REPLAY_DIR" ;;
esac
mkdir -p "$REPLAY_DIR"
CANDIDATES="$REPLAY_DIR/candidates.json"
GOLD="datasets/media_gold.json"
step="${1:-all}"

have() { command -v "$1" >/dev/null 2>&1; }

replay() {
  export LANGFIXER_REPLAY_DIR="$REPLAY_DIR"
  if [ -n "${LANGFIXER_MEDIA_DIR:-}" ] && [ -n "${LLM_API_KEY:-}" ]; then
    echo "① 推演（真实载体：原图 → 视觉抽取 → 检测 → 回映射）"
    cargo test -p api-server --test media_gold_eval -- --ignored --nocapture
  else
    echo "① 推演（离线上限评测：gold 原文当完美抽取——缺 LANGFIXER_MEDIA_DIR / LLM_API_KEY）"
    cargo test -p api-server --test media_gold_eval -- --nocapture
  fi
  [ -f "$CANDIDATES" ] || { echo "FAIL: 没写出 $CANDIDATES" >&2; exit 1; }
  echo "候选文件：${CANDIDATES}（改 \"checked\": false 即取消勾选；各列内容可直接编辑）"
}

export_step() {
  [ -f "$CANDIDATES" ] || { echo "FAIL: 找不到 ${CANDIDATES}，先跑 replay" >&2; exit 1; }
  echo "③ 导出正误表 → $REPLAY_DIR/exported"
  cargo run -q -p api-server --bin media-report -- "$CANDIDATES" \
    --out "$REPLAY_DIR/exported" --gold "$GOLD"
  if have wordcraft-cli; then
    echo "Word 产物：$REPLAY_DIR/exported/report.docx"
    have python3 && echo "（核对：wordcraft-cli text $REPLAY_DIR/exported/report.docx）"
  else
    echo "提示：未安装 wordcraft-cli，只有 json/md/html，没有 Word" >&2
  fi
}

case "$step" in
  replay) replay ;;
  export|export_step) export_step ;;
  all) replay; export_step ;;
  *) echo "用法：$0 [replay|export|all]" >&2; exit 2 ;;
esac
