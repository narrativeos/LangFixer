#!/bin/sh
# 评测通道数据保留清理：删除 N 天前的检测文本与用量日账（默认 7 天）。
# 由 langfixer-retention.timer 每日触发；口径见 docs/GATEWAY_AUTH.md「数据红线」。
# 单机形态不适用（本脚本只清网关侧积累的数据，对单机库同样按天数执行，谨慎指定 GEC_DB_FILE）。
set -eu

DB="${GEC_DB_FILE:-/srv/langfixer/data/langfixer.db}"
AUTH_DB="${LANGFIXER_AUTH_DB:-/srv/langfixer/data/auth.db}"
DAYS="${LANGFIXER_RETENTION_DAYS:-7}"

# 检测文本正文（detect_history）：超期整行删除
sqlite3 "$DB" "
  PRAGMA wal_checkpoint(TRUNCATE);
  DELETE FROM detect_history WHERE created_at < datetime('now', '-$DAYS day');
"

# 配额日账 + 已吊销令牌的历史伪会话归属：随保留期收敛
if [ -f "$AUTH_DB" ]; then
    sqlite3 "$AUTH_DB" "
      PRAGMA wal_checkpoint(TRUNCATE);
      DELETE FROM gw_usage WHERE day < date('now', '-$DAYS day');
      DELETE FROM gw_call_log WHERE ts < strftime('%s', 'now', '-$DAYS day');
      DELETE FROM gw_session_owner
        WHERE token_id NOT IN (SELECT id FROM access_tokens);
      DELETE FROM gw_sessions WHERE expires_at <= strftime('%s','now');
    "
fi

echo "retention ok: db=$DB days=$DAYS"
