#!/bin/zsh
# L3 服务标准启动（G 门控部署口径，README「部署口径」节）：
# 侧车（可选）→ 健康检查 → server。Ctrl-C 同时停两者。
cd "$(dirname "$0")"
./.venv_char/bin/python char_gate.py > /tmp/char_gate.out 2>&1 &
GATE=$!
for i in {1..40}; do
  curl -s -m 3 http://127.0.0.1:4811/health | grep -q ok && break
  sleep 2
done
if ! curl -s -m 3 http://127.0.0.1:4811/health | grep -q ok; then
  echo "[start_l3] 侧车未就绪（详见 /tmp/char_gate.out），server 将以纯启发式启动" 
  L3_URL=""
else
  L3_URL="http://127.0.0.1:4811/gate"
fi
L3_CHAR_GATE_URL=$L3_URL ./.venv/bin/python3 server.py
kill $GATE 2>/dev/null
