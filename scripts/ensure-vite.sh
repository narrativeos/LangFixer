#!/usr/bin/env bash
# 确保 Vite dev server 在 4805 运行（Tauri 桌面端 devUrl 依赖它）
# - 4805 已有服务且 PID 存活：直接通过
# - PID 文件存在但进程已死（如 VS Code 任务 shell 退出时回收了进程组）：重新拉起
# - 否则后台拉起 `npm run dev`（nohup + disown 脱离任务 shell 的进程组，日志 /tmp/langfixer-vite.log）
# 作为 VS Code preLaunchTask 使用（前台任务，完成即退出）
set -uo pipefail
cd "$(dirname "$0")/../web-ui"

PID_FILE=/tmp/langfixer-vite.pid
LOG_FILE=/tmp/langfixer-vite.log

vite_alive() {
  # 端口有响应 且（若有 PID 文件）该进程仍存活
  curl -s -o /dev/null --max-time 2 http://127.0.0.1:4805 || return 1
  if [[ -f "$PID_FILE" ]]; then
    local pid
    pid=$(cat "$PID_FILE" 2>/dev/null || true)
    if [[ -n "$pid" ]] && ! kill -0 "$pid" 2>/dev/null; then
      return 1
    fi
  fi
  return 0
}

if vite_alive; then
  echo "==> Vite 已在运行（http://localhost:4805）"
  exit 0
fi

# 端口无响应或 PID 已死：清理残留 PID 文件后重新拉起
if [[ -f "$PID_FILE" ]]; then
  echo "==> 检测到 Vite 进程已退出（$(cat "$PID_FILE" 2>/dev/null)），重新拉起"
  rm -f "$PID_FILE"
fi

echo "==> 启动 Vite dev server（日志：${LOG_FILE}）"
nohup npm run dev > "$LOG_FILE" 2>&1 &
VITE_PID=$!
disown "$VITE_PID" 2>/dev/null || true
echo "$VITE_PID" > "$PID_FILE"

for _ in $(seq 1 30); do
  sleep 1
  if vite_alive; then
    echo "==> Vite 就绪（http://localhost:4805，PID ${VITE_PID}）"
    exit 0
  fi
done

echo "!! Vite 启动失败，详见 $LOG_FILE"
exit 1