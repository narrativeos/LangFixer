#!/usr/bin/env bash
# 知识源导入管线 LLM 冒烟测试（真 LLM 全链路：分块挖掘 → 草稿 → 归并预览）
#
# 前提：api-server 已启动且已配置 LLM（设置界面或环境变量），且 prompts/{lang}_ingest_prompt.json 存在。
# 默认不发布（跑到归并预览后放弃会话，不污染知识库）；PUBLISH=1 时执行一键发布。
# 冒烟材料为自写语法讲解片段（不使用任何教材正文，避免版权与 IP 边界问题）。
#
# 用法：BASE=http://127.0.0.1:4801 [PUBLISH=1] [TOKEN=xxx] ./scripts/ingest_smoke.sh
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:4801}"
PUBLISH="${PUBLISH:-0}"
TOKEN="${TOKEN:-}"
LANG="${LANG:-en}"
SID="src-smoke-$(date +%s)"

api() {
  local method=$1 path=$2 body=${3:-}
  local args=(-sS -X "$method" "$BASE$path" -H 'Content-Type: application/json')
  [ -n "$TOKEN" ] && args+=(-H "X-Tables-Update-Token: $TOKEN")
  [ -n "$body" ] && args+=(-d "$body")
  curl "${args[@]}"
}

fail() { echo "FAIL: $*" >&2; exit 1; }

echo "== 冒烟：$BASE 源=$SID lang=$LANG =="

# ① 服务与 LLM 可用性
api GET /gec/knowledge/sources >/dev/null || fail "服务不可达：$BASE"

# ② 自写冒烟正文（多段，max_chunk_chars 小以确保多块）；用 python 安全构造 JSON
BODY=$(SID="$SID" LANG="$LANG" python3 - <<'PY'
import json, os
text = """冒烟语法材料 第一节。不定冠词 a 用于辅音音素开头的单词前；an 用于元音音素开头的单词前。例如 an hour, a university。考生常错写 a hour。

冒烟语法材料 第二节。动词 agree 后接 that 从句时，从句用虚拟语气（should + 动词原形）。例如: The law requires that every driver should carry a license. 常见错误: requires that every driver carries。

冒烟语法材料 第三节。双重否定表肯定：not uncommon 意为 common。He is not unfamiliar with the topic 表示他对话题熟悉。"""
print(json.dumps({
    "source_id": os.environ["SID"],
    "title": "冒烟自写语法材料",
    "author": "smoke",
    "edition": "v1",
    "lang": os.environ["LANG"],
    "license_scope": "public",
    "text": text,
    "max_chunk_chars": 300,
}))
PY
)

RESP=$(api POST /gec/knowledge/ingest "$BODY")
echo "start: $RESP"
echo "$RESP" | grep -q '"chunks_total"' || fail "开启导入失败（LLM 未配置 / 缺 ingest 模板？）"

# ③ 轮询挖掘进度（最长 10 分钟）
echo "== 轮询挖掘进度 =="
for i in $(seq 1 300); do
  DETAIL=$(api GET "/gec/knowledge/ingest/$SID")
  STATUS=$(echo "$DETAIL" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("status",""))')
  DONE=$(echo "$DETAIL" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(str(d["chunks_done"])+"/"+str(d["chunks_total"]))')
  echo "  [$i] status=$STATUS chunks=$DONE"
  [ "$STATUS" = "mined" ] || [ "$STATUS" = "failed" ] && break
  sleep 2
done
[ "$STATUS" = "mined" ] || [ "$STATUS" = "failed" ] || fail "挖掘超时"

# ④ 草稿与归并预览
DRAFTS=$(api GET "/gec/knowledge/ingest/$SID/drafts")
PLAN=$(api GET "/gec/knowledge/ingest/$SID/plan")
NDRAFT=$(echo "$DRAFTS" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)))')
NNEW=$(echo "$PLAN" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["new_points"]))')
NEND=$(echo "$PLAN" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["endorsements"]))')
echo "草稿 $NDRAFT 条；归并预览：新点 $NNEW / 背书 $NEND"
[ "$NDRAFT" -gt 0 ] || fail "未产出任何草稿（agent 挖掘失败）"

if [ "$PUBLISH" = "1" ]; then
  REPORT=$(api POST "/gec/knowledge/ingest/$SID/publish")
  echo "publish: $REPORT"
  echo "$REPORT" | grep -q '"total_points"' || fail "发布失败"
  POINTS=$(api GET "/gec/knowledge/points?lang=$LANG&source_id=$SID")
  echo "发布后本源关联知识点：$(echo "$POINTS" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)))')"
else
  api POST "/gec/knowledge/ingest/$SID/discard" >/dev/null
  echo "已放弃冒烟会话（不污染知识库；PUBLISH=1 可启用真实发布）"
fi

echo "PASS: 导入管线冒烟通过"