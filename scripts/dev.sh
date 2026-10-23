#!/usr/bin/env bash
# LangFixer 本地开发：并行启动 API 服务（:4801）与 Vite 前端（:4805）
set -uo pipefail
cd "$(dirname "$0")/.."

trap 'kill 0' EXIT

echo "==> 启动 api-server (http://localhost:4801)"
cargo run -p api-server &

echo "==> 启动 web-ui (http://localhost:4805)"
cd web-ui
[ -d node_modules ] || npm install --no-audit --no-fund
npm run dev
