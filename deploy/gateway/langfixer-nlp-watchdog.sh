#!/bin/sh
# 中文 NLP sidecar（narrative-operator-nlp）探活看门狗。
# 由 langfixer-nlp-watchdog.timer 每 5 分钟触发。
#
# 背景（2026-11-23 实发）：容器 Up 但 uvicorn 静默挂死——端口在听、请求无响应、
# 日志停更、无 OOM 无重启记录，LangFixer 侧只逐请求降级（每轮白烧 10s 连接超时），
# 挂死持续数小时无人知晓。`--restart unless-stopped` 只救进程退出，救不了应用僵死。
#
# 语义：单次探测失败记一次数，连续 FAIL_THRESHOLD 次（≈10 分钟）才重启容器；
# 重启后 COOLDOWN 秒内不再重启（防模型重载期误判成循环重启）。一切走 stdout 进
# journald（journalctl -u langfixer-nlp-watchdog.service）。
set -eu

ENDPOINT="${HANLP_ENDPOINT:-http://172.18.0.1:8000}"
CONTAINER="${NLP_WATCHDOG_CONTAINER:-narrative-operator-nlp}"
STATE=/srv/langfixer/nlp-watchdog.state
MARK=/srv/langfixer/nlp-watchdog.restarted
FAIL_THRESHOLD=2
COOLDOWN=900

now() { date -u +%FT%TZ; }

code=$(curl -s -m 5 -o /dev/null -w "%{http_code}" -X POST "$ENDPOINT/analyze" \
  -H 'content-type: application/json' -d '{"text":"看门狗探活。"}' || true)

if [ "$code" = "200" ]; then
  echo 0 > "$STATE"
  echo "$(now) healthy http=$code"
  exit 0
fi

fails=$(cat "$STATE" 2>/dev/null || echo 0)
fails=$((fails + 1))
echo "$fails" > "$STATE"
echo "$(now) probe-fail http=${code:-none} fails=$fails/$FAIL_THRESHOLD"
[ "$fails" -lt "$FAIL_THRESHOLD" ] && exit 0

if [ -f "$MARK" ]; then
  last=$(stat -c %Y "$MARK" 2>/dev/null || echo 0)
  if [ $(( $(date +%s) - last )) -lt "$COOLDOWN" ]; then
    echo "$(now) restart-skipped cooldown"
    exit 0
  fi
fi

if docker restart "$CONTAINER"; then
  touch "$MARK"
  echo 0 > "$STATE"
  echo "$(now) restarted $CONTAINER after $fails consecutive failures"
else
  echo "$(now) restart-FAILED $CONTAINER" >&2
  exit 1
fi
