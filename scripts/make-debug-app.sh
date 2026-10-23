#!/usr/bin/env bash
# 组装 debug 版 .app bundle：target/debug/LangFixer Desktop.app
# - Dock 名称取自 Info.plist 的 CFBundleName（"LangFixer Desktop"），而非裸二进制名
# - CFBundleIconFile 指向 icon.icns，Dock/程序坞显示自定义图标
# 可执行文件为 target/debug/desktop-app 的拷贝；VS Code 调试配置直接 launch bundle 内二进制
# 作为 VS Code preLaunchTask 使用（幂等：每次运行都刷新二进制与 bundle 元数据）
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="LangFixer Desktop"
APP_DIR="target/debug/${APP_NAME}.app"
MACOS_DIR="${APP_DIR}/Contents/MacOS"
RES_DIR="${APP_DIR}/Contents/Resources"

mkdir -p "$MACOS_DIR" "$RES_DIR"

echo "==> 复制 debug 二进制到 ${APP_NAME}.app"
cp target/debug/desktop-app "${MACOS_DIR}/desktop-app"

cat > "${APP_DIR}/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>LangFixer Desktop</string>
  <key>CFBundleDisplayName</key>
  <string>LangFixer Desktop</string>
  <key>CFBundleExecutable</key>
  <string>desktop-app</string>
  <key>CFBundleIdentifier</key>
  <string>com.langfixer.desktop</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>0.1.0</string>
  <key>CFBundleIconFile</key>
  <string>icon</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.13</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

if [[ -f desktop-app/icons/icon.icns ]]; then
  cp desktop-app/icons/icon.icns "${RES_DIR}/icon.icns"
else
  echo "!! 缺少 desktop-app/icons/icon.icns（先运行 python3 scripts/make_icon.py）"
  exit 1
fi

# 刷新 Finder/Dock 图标缓存
touch "$APP_DIR"
echo "==> 已组装 ${APP_DIR}（Dock 名称：${APP_NAME}）"