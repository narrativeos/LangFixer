#!/usr/bin/env bash
# LangFixer 发布构建：release 二进制 + 前端产物 + Tauri 桌面安装包
# 桌面安装包按发布源分别打包：LANGFIXER_RELEASE_SOURCE 在构建时烧入二进制，
# 用户从哪个源下载安装包，应用就自动从哪个源检查更新并获取新版（desktop-app/src/update.rs）。
set -euo pipefail
cd "$(dirname "$0")/.."

SOURCES=(github gitea atomgit)
VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
TAG="v$VERSION"

# latest.json 的 version 取自 Cargo.toml，而运行时比较用的是 tauri.conf.json 的
# version：两者一旦不一致，装好新版的用户会永远认为自己落后，反复下载安装。
CONF_VERSION="$(python3 -c 'import json;print(json.load(open("desktop-app/tauri.conf.json"))["version"])')"
if [[ "$VERSION" != "$CONF_VERSION" ]]; then
    echo "!! 版本号不一致: Cargo.toml=$VERSION, tauri.conf.json=$CONF_VERSION——发布前请先同步两处版本"
    exit 1
fi

echo "==> cargo build --release (workspace)"
cargo build --release --workspace

echo "==> web-ui: npm run build"
cd web-ui
npm install --no-audit --no-fund
npm run build
cd ..

echo "==> 生成 Tauri 图标集（PNG/icns/ico）"
python3 scripts/make_icon.py

# 更新签名密钥：有则产出 .sig / latest.json；无则本次关闭更新产物（安装包照常构建）
KEY="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.config/langfixer/updater.key}"
BUILD_ARGS=()
if [[ -f "$KEY" ]]; then
    export TAURI_SIGNING_PRIVATE_KEY_PATH="$KEY"
    export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
else
    echo "!! 缺少更新签名密钥（$KEY）：本次不产出自动更新产物"
    echo "   生成: cargo tauri signer generate -w $KEY --password ''（公钥需与 tauri.conf.json 一致）"
    BUILD_ARGS+=(--config '{"bundle":{"createUpdaterArtifacts":false}}')
fi

failed=()
for src in "${SOURCES[@]}"; do
    echo "==> tauri build（源=$src $TAG）"
    # 先清空 bundle 目录：否则上一版本残留的安装包/.sig 会被混进本次 dist 与 latest.json
    rm -rf target/release/bundle
    if ! LANGFIXER_RELEASE_SOURCE="$src" cargo tauri build ${BUILD_ARGS[@]+"${BUILD_ARGS[@]}"}; then
        echo "!! tauri build 失败（缺少打包工具链），Rust 二进制与前端产物已就绪"
        failed+=("$src")
        continue
    fi
    out="dist/$src/$TAG"
    rm -rf "$out"; mkdir -p "$out"
    find target/release/bundle -type f \
        \( -name '*.dmg' -o -name '*.AppImage' -o -name '*-setup.exe' -o -name '*.sig' \) \
        -exec cp -p {} "$out/" \;
    python3 scripts/make_latest_json.py "$src" "$TAG" "$out"
    echo "==> 源 $src 产物就绪: $out"
done

if [[ ${#failed[@]} -gt 0 ]]; then
    echo "!! 未全部构建成功: ${failed[*]}"
    exit 1
fi
echo "==> 发布构建完成。后续步骤:"
echo "   1) scripts/publish.sh          # 推送分支与标签到各源（可指定单个源）"
echo "   2) 把 dist/<源>/$TAG/ 下的安装包与 latest.json 上传为该标签在各源的 release 资产"
