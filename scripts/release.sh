#!/usr/bin/env bash
# LangFixer 发布构建：release 二进制 + 前端产物 + Tauri 桌面安装包
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo build --release (workspace)"
cargo build --release --workspace

echo "==> web-ui: npm run build"
cd web-ui
npm install --no-audit --no-fund
npm run build
cd ..

echo "==> 生成 Tauri 图标集（PNG/icns/ico）"
python3 scripts/make_icon.py

echo "==> tauri build (桌面安装包)"
cargo tauri build || echo "!! tauri build 失败（缺少打包工具链），Rust 二进制与前端产物已就绪"

echo "==> 发布构建完成"
