#!/usr/bin/env bash
# LangFixer 日常更新：把 HEAD 铺到目标实例上，失败回滚
#
#   ./update.sh server            后端二进制 + 前端 dist + 运行时资产，全同步到公网评测实例
#   ./update.sh local             重编译并重启本机实例（前端 Vite 不重启：HMR 已自己跟上）
#   ./update.sh server --no-web   只更后端（前端有已知问题时）
#   ./update.sh server --no-assets 只更后端与前端，不碰运行时资产
#
# 两条红线（与 setup.sh 共用 scripts/deploy-common.sh）：
#   · 只报密钥指纹，绝不打印明文；也绝不说「重启换钥匙」这种话——重启就是同一把钥匙。
#   · 铺之前先对账运行时资产：规则开关会被服务**写回** lexicons/ 的 JSON，无脑覆盖等于
#     静默抹掉运维在界面上做的改动，所以先比指纹、备份，才铺。

set -euo pipefail
# shellcheck source=scripts/deploy-common.sh
. "$(dirname "$0")/scripts/deploy-common.sh"

usage() {
  printf '%s\n' "用法: ./update.sh local|server [--no-web] [--no-assets] [--dry-run]"
  printf '  %-22s %s\n' "local" "重编译 + 重启本机实例（.dev/ 里的密钥与隔离库不动）"
  printf '  %-22s %s\n' "server" "编译 release → 换二进制 → 重启 $LF_SERVICE → 探活，失败自动回滚"
  printf '  %-22s %s\n' "--no-web" "跳过前端 dist"
  printf '  %-22s %s\n' "--no-assets" "跳过运行时资产（decision-tables/lexicons/prompts）"
  printf '  %-22s %s\n' "--dry-run" "只报「打算铺什么 + 资产差异」，不动任何东西"
}

MODE="${1:-}"
WEB=1
ASSETS=1
DRY=0
shift || true
for a in "$@"; do
  case "$a" in
    --no-web) WEB=0 ;;
    --no-assets) ASSETS=0 ;;
    --dry-run) DRY=1 ;;
    -h | --help) usage && exit 0 ;;
    *) usage && die "未知参数: $a" ;;
  esac
done
[ "$MODE" != "--help" ] || { usage && exit 0; }

# 上次铺的是哪个提交 → 这次带来什么。没有记录就老实说无法比较。
# 注意：所有可能以非零收尾的尾行都要 `|| true`——调用处是 `f || step ...`，
# 函数体尾命令的失败会先触发 ERR trap，step 兜底根本没机会跑。
diff_since_deploy() { # $1 = 上次 SHA  $2 = 标签
  local n
  [ -n "$1" ] || { say "$2：这次按全量铺（没有可比的上次部署点）"; return 0; }
  n="$(git -C "$REPO_ROOT" rev-list --count "$1..HEAD" 2>/dev/null || echo '')"
  if [ -z "$n" ]; then
    say "上次记录（$2）不是本仓库提交，无法比较——按全量铺"
  elif [ "$n" = 0 ]; then
    say "与上次部署同一提交（$1）：这次只是重编译 + 重铺"
  else
    say "$2 → $HEAD_SHA 共 $n 个提交："
    git -C "$REPO_ROOT" log --oneline -12 "$1..HEAD" | sed 's/^/      /'
    [ "$n" -gt 12 ] && say "      …另 $((n - 12)) 个" || true
  fi
}

# 自上次部署被**删除**的文件：不删的话编译树与运行时目录会留残骸
prune_removed() { # $1 = 上次 SHA  $2 = 目标根
  [ -n "$1" ] || return 0
  git -C "$REPO_ROOT" diff --diff-filter=D --name-only "$1..HEAD" -- \
    api-server Cargo.toml Cargo.lock "${LF_ASSET_DIRS[@]}" 2>/dev/null |
    grep -v '^[[:space:]]*$' >"$TMP/deleted.txt" || true
  if [ ! -s "$TMP/deleted.txt" ]; then
    say "无删除文件需要清理"
    return 0
  fi
  say "清理 $(wc -l <"$TMP/deleted.txt" | tr -d ' ') 个自上次部署已删除的文件"
  # 只删 git 跟踪过的路径：lexicons/.lexicon-seed.lock 这类未跟踪的运行时锁文件不碰
  ssh_root "cd $2 && while IFS= read -r f; do case \"\$f\" in *'..'*) ;; *[![:alnum:]_./-]*) ;; *) rm -f \"\$f\" ;; esac; done" <"$TMP/deleted.txt"
}

# ── 前端：本机构建（与服务器编译并行）→ 原子铺静态根 ─────────────────────
web_build() {
  step "前端：构建 dist"
  if [ ! -d "$REPO_ROOT/web-ui/node_modules" ]; then
    say "npm install（首次会慢）"
    (cd "$REPO_ROOT/web-ui" && npm install --no-audit --no-fund >/dev/null)
  fi
  # --base：服务器形态把产品挂在 /app/ 下（根留给官网落地页 site/）；桌面端构建不走这里
  (cd "$REPO_ROOT/web-ui" && npm run build -- --base="$LF_WEB_BASE" >"$TMP/web-build.log" 2>&1) || {
    tail -15 "$TMP/web-build.log" >&2
    die "前端构建失败，见 $TMP/web-build.log"
  }
  WEB_ENTRY="$(grep -o 'index-[A-Za-z0-9_-]*\.js' "$REPO_ROOT/web-ui/dist/index.html" | head -1)"
  say "入口 $WEB_ENTRY"
}

web_deploy() {
  # dist 一律进 app/：web 根根的 /index.html 是官网落地页（site/），不再被 dist 覆盖。
  # 静态资源先落 /tmp 再解包，index.html 最后 mv 原子替换：避免刷新撞上
  # 「新 HTML 指向还没上传的 js」这种半更新态
  say "铺到 $LF_SSH:$LF_WEB_APP"
  tar -C "$REPO_ROOT/web-ui/dist" --exclude index.html -czf "$TMP/web.tgz" .
  scp -q "$TMP/web.tgz" "$LF_SSH:/tmp/langfixer-web.tgz"
  ssh_root "install -d $LF_WEB_APP && tar -xzf /tmp/langfixer-web.tgz -C $LF_WEB_APP && rm -f /tmp/langfixer-web.tgz" 2>&1 |
    grep -v 'LIBARCHIVE.xattr' || true
  ssh_root "cat > /tmp/langfixer-index.html" <"$REPO_ROOT/web-ui/dist/index.html"
  ssh_root "mv /tmp/langfixer-index.html $LF_WEB_APP/index.html && chmod 644 $LF_WEB_APP/index.html"
  # 官网落地页与产品同一趟同步（site/ 是纯静态单文件页，直接盖根）
  scp -q "$REPO_ROOT/site/index.html" "$REPO_ROOT/site/en.html" "$LF_SSH:$LF_WEB_ROOT/"
  ssh_root "chmod 644 $LF_WEB_ROOT/index.html $LF_WEB_ROOT/en.html"
  web_probe
}

case "$MODE" in
# ══════════════════════════════════ 本机 ══════════════════════════════════
local)
  step "本机更新（网关实例 :${LF_PORT}）"
  mk_tmp
  require_repo local
  [ -f "$DEV_ENV" ] || die "缺 ${DEV_ENV}——先跑 ./setup.sh local"
  if [ -f "$DEV_DIR/.deployed" ]; then
    diff_since_deploy "$(cat "$DEV_DIR/.deployed")" "$(cat "$DEV_DIR/.deployed")"
  fi
  if [ "$DRY" = 1 ]; then
    say "--dry-run：将执行 cargo build -p api-server 后重启本机实例"
    exit 0
  fi

  step "编译 debug 二进制"
  cargo build -p api-server >"$DEV_DIR/build.log" 2>&1 &
  build_wait_local "$DEV_DIR/build.log"

  step "重启后端（密钥与库路径沿用 .dev/gateway.env，不换钥匙）"
  local_stop
  local_start
  probe_gate "$LF_LOCAL_URL"
  local_report
  if [ -n "$(lsof -nP -ti tcp:4805 2>/dev/null | head -1)" ]; then
    say "前端仍在跑（Vite HMR 已跟上前端源码改动，无需重启）：http://localhost:4805"
  else
    say "前端没在跑；要起：./setup.sh local 或 ./scripts/dev.sh"
  fi
  printf '%s\n' "$HEAD_SHA" >"$DEV_DIR/.deployed"
  ;;

# ══════════════════════════════════ 服务器 ════════════════════════════════
server)
  step "服务器更新：$LF_SSH"
  mk_tmp
  require_repo server
  ssh_root "test -d $LF_SRC && test -f $LF_DIR/langfixer.env" ||
    die "服务器上还没有 $LF_SRC / langfixer.env——先跑 ./setup.sh server"
  PREV_SHA="$(ssh_root "cat $LF_DIR/.deployed 2>/dev/null | cut -d' ' -f1" || true)"
  say "当前：服务 $(ssh_root "systemctl is-active $LF_SERVICE" || true) · 已部署 ${PREV_SHA:-（无记录）}"
  diff_since_deploy "${PREV_SHA:-}" "${PREV_SHA:-无记录}"

  step "对账运行时资产（HEAD vs 服务器运行时副本）"
  assets_report
  if [ "$DRY" = 1 ]; then
    say "--dry-run：将执行源码铺放 → 服务器 release 编译 → 换二进制并重启 → 探活（失败自动回滚）"
    say "  本次范围：资产 $( [ "$ASSETS" = 1 ] && echo 会同步 || echo 跳过 ) · 前端 $( [ "$WEB" = 1 ] && echo 会构建并铺 || echo 跳过 )"
    say "上面标「不同」的资产目录会被 HEAD 覆盖（脚本先备份到 $LF_DIR/.backup/）"
    exit 0
  fi

  step "铺源码 → $LF_SRC"
  git -C "$REPO_ROOT" archive --format=tar HEAD | ssh_root "tar -x -C $LF_SRC"
  say "HEAD $HEAD_SHA 已铺"
  prune_removed "${PREV_SHA:-}" "$LF_SRC"

  step "服务器编译 release（服务此时**不动**，4 核增量约 1-3 分钟）"
  BUILD_LOG="$LF_DIR/log/lf-build.log"
  ssh_root "install -d $LF_DIR/log && : >'$BUILD_LOG'"
  build_start_remote "$BUILD_LOG"
  # 服务器在编译，本机同时构建前端：两头并行，省一轮等待
  [ "$WEB" = 1 ] && web_build
  build_wait "$BUILD_LOG"
  ssh_root "test -x $LF_SRC/target/release/api-server" || die "服务器上没有 target/release/api-server"

  if [ "$ASSETS" = 1 ]; then
    step "铺运行时资产（先留档）"
    ssh_root "install -d $LF_DIR/.backup"
    assets_backup "$(date +%Y%m%d-%H%M%S)"
    for d in "${LF_ASSET_DIRS[@]}"; do
      ssh_root "install -d $LF_DIR/$d && cp -a $LF_SRC/$d/. $LF_DIR/$d/"
    done
    say "已同步：${LF_ASSET_DIRS[*]}（models/ 的 gguf 不铺）"
    prune_removed "${PREV_SHA:-}" "$LF_DIR"
  fi

  step "换二进制并重启"
  TS="$(date +%Y%m%d-%H%M%S)"
  ssh_root "cp -a $LF_DIR/bin/api-server $LF_DIR/bin/api-server.prev.$TS 2>/dev/null || true; \
    ln -sfn api-server.prev.$TS $LF_DIR/bin/api-server.prev; \
    install -m 755 $LF_SRC/target/release/api-server $LF_DIR/bin/api-server.new && \
    mv -f $LF_DIR/bin/api-server.new $LF_DIR/bin/api-server && \
    systemctl restart $LF_SERVICE" || die "install/restart 失败（二进制未换或已换未起，见下）"
  sleep 2
  if ! ssh_root "systemctl is-active --quiet $LF_SERVICE"; then
    warn "服务未起来，回滚二进制到上一版"
    ssh_root "cp -f $LF_DIR/bin/api-server.prev $LF_DIR/bin/api-server && systemctl restart $LF_SERVICE && sleep 2 && systemctl is-active $LF_SERVICE" || true
    ssh_root "journalctl -u $LF_SERVICE --no-pager -n 20 | sed 's/^/    /'" || true
    die "已回滚到上一版二进制；这次更新没生效，别继续铺前端"
  fi
  say "服务 active"

  step "探活：直连网关 + 密钥真实调用 + 公网入口"
  if ! server_report; then
    warn "健康检查没过，回滚二进制到上一版并重启"
    ssh_root "test -f $LF_DIR/bin/api-server.prev && cp -f $LF_DIR/bin/api-server.prev $LF_DIR/bin/api-server; systemctl restart $LF_SERVICE && sleep 3 && systemctl is-active $LF_SERVICE" || true
    ssh_root "journalctl -u $LF_SERVICE --no-pager -n 20 | sed 's/^/    /'" || true
    die "已回滚到上一版二进制；这次更新没生效，前端也先别铺"
  fi
  web_probe

  if [ "$WEB" = 1 ]; then
    step "铺前端 dist"
    web_deploy
  else
    say "--no-web：前端未改动"
  fi

  step "收尾"
  server_report
  ssh_root "printf '%s %s\n' '$HEAD_SHA' \"\$(date -Is)\" >$LF_DIR/.deployed"
  say "已记录部署点：$LF_DIR/.deployed = $HEAD_SHA"
  say "回滚手段：$LF_DIR/bin/api-server.prev → 本次覆盖前的二进制"

  step "还差一步（脚本刻意不动反向代理）"
  say "Caddy 站点片段有变化时：追加 deploy/gateway/Caddyfile.snippet 进"
  say "  /dnmp/services/caddy/config/caddyfile，然后 docker exec caddy caddy reload --config /config/caddyfile"
  ;;

*)
  usage
  die "用法: ./update.sh local|server [--no-web] [--no-assets] [--dry-run]"
  ;;
esac
