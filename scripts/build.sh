#!/usr/bin/env bash
# LangFixer 构建脚本：Rust workspace + Web UI
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo build (workspace)"
cargo build --workspace

echo "==> web-ui: npm install"
cd web-ui
npm install --no-audit --no-fund

echo "==> web-ui: npm run build"
npm run build

echo "==> 构建完成"
