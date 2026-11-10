#!/usr/bin/env bash
# LangFixer Windows 构建脚本（scripts/build.sh 的 Windows 对应版）
#
# 用法：
#   scripts/build-win.sh              # release 构建 workspace + web-ui 类型检查/单测/打包
#   scripts/build-win.sh --debug      # 走 debug profile（增量更快）
#   scripts/build-win.sh --test       # 额外跑 cargo test --workspace + sqlite feature 测试
#   scripts/build-win.sh --bundle     # 额外用 tauri CLI 打安装包（需网络拉 @tauri-apps/cli）
#   scripts/build-win.sh --gate       # 额外跑评测回归门（需 datasets/eval_reports/baseline_rules.json）
#
# 前置（本机实测）：
#   - Rust：rustup 装的 stable-x86_64-pc-windows-msvc
#   - 链接器：VS 2022 BuildTools（MSVC 14.4x）+ Windows SDK 10.x
#   - Node 20+（CI 用 22；本机 v26 实测通过）
#
# 多用户坑：rustup 可能装在别的用户目录下（本机在 C:\Users\jiulu）。
# 此时当前账户既没有 RUSTUP_HOME/CARGO_HOME，PATH 上也没有 cargo，
# rustup 会报「no default is configured」。脚本会自动探测并指过去。
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="release"
RUN_TEST=0
RUN_BUNDLE=0
RUN_GATE=0
for arg in "$@"; do
  case "$arg" in
    --debug)  PROFILE="debug" ;;
    --test)   RUN_TEST=1 ;;
    --bundle) RUN_BUNDLE=1 ;;
    --gate)   RUN_GATE=1 ;;
    *) echo "未知参数：$arg" >&2; exit 2 ;;
  esac
done

# ---------- 工具链定位 ----------
find_cargo() {
  if command -v cargo >/dev/null 2>&1; then return 0; fi
  local home="${USERPROFILE:-$HOME}"
  local cand
  for cand in \
    "$home/.cargo/bin/cargo.exe" \
    /c/Users/*/.cargo/bin/cargo.exe \
    "/c/Program Files/Rustup/toolchains"/*/bin/cargo.exe; do
    if [ -x "$cand" ]; then
      export CARGO_HOME="$(cd "$(dirname "$cand")/.." && pwd)"
      # rustup home 约定与 cargo home 同级（~/.rustup）
      local rh="$(cd "$CARGO_HOME/../.rustup" 2>/dev/null && pwd || true)"
      [ -n "$rh" ] && export RUSTUP_HOME="$rh"
      export PATH="$(dirname "$cand"):$PATH"
      echo "==> 使用 $cand（CARGO_HOME=${CARGO_HOME:-默认} RUSTUP_HOME=${RUSTUP_HOME:-默认}）"
      return 0
    fi
  done
  echo "找不到 cargo。请装 rustup（https://rustup.rs）或指定 PATH。" >&2
  exit 1
}

find_cargo
command -v cargo >/dev/null || { echo "cargo 仍不可用" >&2; exit 1; }
echo "==> cargo: $(cargo -V)"
echo "==> rustc: $(rustc -V)"

# MSVC 链接器自检（缺失时报错比让 rustc 报「link.exe not found」好读）
if [ -x "/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe" ]; then
  link_ok=$("/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe" \
    -products '*' -all -find 'VC/Tools/MSVC/*/bin/Hostx64/x64/link.exe' | head -1)
  if [ -z "$link_ok" ]; then
    echo "警告：没找到 MSVC link.exe，链接会失败。请给 VS BuildTools 装「使用 C++ 的桌面开发」。" >&2
  fi
fi

CARGO_FLAGS=(--workspace --"$PROFILE")

echo "==> cargo build ${CARGO_FLAGS[*]}"
cargo build "${CARGO_FLAGS[@]}"

echo "==> web-ui: npm ci（已有 node_modules 时很快）"
(cd web-ui && npm ci --no-audit --no-fund)

echo "==> web-ui: typecheck"
(cd web-ui && npm run typecheck)

echo "==> web-ui: 单测"
(cd web-ui && npm test)

echo "==> web-ui: vite build（desktop-app 的 frontendDist）"
(cd web-ui && npm run build)

if [ "$RUN_TEST" = 1 ]; then
  echo "==> cargo test ${CARGO_FLAGS[*]}"
  cargo test "${CARGO_FLAGS[@]}"
  echo "==> cargo test -p shared-model --features sqlite"
  cargo test --release -p shared-model --features sqlite
fi

if [ "$RUN_GATE" = 1 ]; then
  echo "==> 评测回归门（规则层金集）"
  cargo run --release -p gec-eval -- --check datasets/eval_reports/baseline_rules.json
fi

if [ "$RUN_BUNDLE" = 1 ]; then
  echo "==> tauri 打包（Windows 安装包）"
  (cd desktop-app && npx --yes @tauri-apps/cli@2 build)
fi

echo "==> 完成。产物：target/$PROFILE/*.exe"
ls -1 "target/$PROFILE"/*.exe 2>/dev/null || true
if [ "$RUN_BUNDLE" = 1 ]; then
  echo "==> 安装包："
  ls -1 target/release/bundle/*/*.exe target/release/bundle/*/*.msi 2>/dev/null || true
fi
