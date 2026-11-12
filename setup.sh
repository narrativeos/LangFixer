#!/usr/bin/env bash
# LangFixer 首次安装并启动
#
#   ./setup.sh local            本机形态：网关后端(:4801) + Vite(:4805)，密钥与隔离库落在 .dev/
#   ./setup.sh server           服务器形态：/srv/langfixer 首次铺齐（源码 / 资产 / systemd）并启动
#   ./setup.sh server --web     顺带把前端 dist 也铺到静态根（默认不铺，交给 update.sh）
#
# 幂等：已存在的密钥、env、systemd 单元一律不覆盖（换钥匙是显式动作，见 docs/GATEWAY_AUTH.md）。
# 日常更新请用 ./update.sh。

set -euo pipefail
# shellcheck source=scripts/deploy-common.sh
. "$(dirname "$0")/scripts/deploy-common.sh"

usage() {
  printf '%s\n' "用法: ./setup.sh local|server [--web] [--web-only]"
  printf '  %-26s %s\n' "local" "本机网关 + 隔离库（.dev/），起 4801 与 4805"
  printf '  %-26s %s\n' "server" "首次铺 $LF_SSH 并 enable --now $LF_SERVICE"
  printf '  %-26s %s\n' "--web" "server：顺带构建并铺前端 dist 到静态根"
  printf '  %-26s %s\n' "--web-only" "server：只铺前端（服务已在跑，只想刷新界面时）"
  printf '  %-26s %s\n' "环境变量" "LF_SSH LF_URL LF_DIR LF_WEB_ROOT LF_SERVICE LF_PORT LF_FORCE_UNITS"
}

MODE="${1:-}"
WEB=0
WEB_ONLY=0
shift || true
for a in "$@"; do
  case "$a" in
    --web) WEB=1 ;;
    --web-only) WEB_ONLY=1 ;;
    -h | --help) usage && exit 0 ;;
    *) usage && die "未知参数: $a" ;;
  esac
done

[ "$MODE" != "--help" ] || { usage && exit 0; }

# ── 前端 dist 铺静态根（web 根里只放 dist：源码 / env / 库一律不得入内） ──
deploy_web() {
  step "前端：构建 dist → $LF_SSH:$LF_WEB_ROOT"
  if [ ! -d "$REPO_ROOT/web-ui/node_modules" ]; then
    say "npm install（首次会慢）"
    (cd "$REPO_ROOT/web-ui" && npm install --no-audit --no-fund >/dev/null)
  fi
  (cd "$REPO_ROOT/web-ui" && npm run build >"$TMP/web-build.log" 2>&1) ||
    die "前端构建失败，见 $TMP/web-build.log：$(tail -5 "$TMP/web-build.log" | tr '\n' ' ')"
  say "dist 入口 $(grep -o 'index-[A-Za-z0-9_-]*\.js' "$REPO_ROOT/web-ui/dist/index.html" | head -1)"
  # 先 tar 落 /tmp 再解包，**index.html 用 mv 原子替换**：静态资源先就位、入口最后切，
  # 避免刷新拿到「新 HTML + 还没有的 js」这种半更新态
  tar -C "$REPO_ROOT/web-ui/dist" --exclude index.html -czf "$TMP/web.tgz" .
  scp -q "$TMP/web.tgz" "$LF_SSH:/tmp/langfixer-web.tgz"
  ssh_root "install -d $LF_WEB_ROOT && tar -xzf /tmp/langfixer-web.tgz -C $LF_WEB_ROOT && rm -f /tmp/langfixer-web.tgz" 2>&1 |
    grep -v 'LIBARCHIVE.xattr' || true
  ssh_root "cat > /tmp/langfixer-index.html" <"$REPO_ROOT/web-ui/dist/index.html"
  ssh_root "mv /tmp/langfixer-index.html $LF_WEB_ROOT/index.html && chmod 644 $LF_WEB_ROOT/index.html"
  web_probe
}

case "$MODE" in
# ══════════════════════════════════ 本机 ══════════════════════════════════
local)
  step "本机首次安装（网关形态 + 隔离库）"
  mk_tmp
  require_repo local
  install -d -m 700 "$DEV_DIR"
  if [ ! -f "$DEV_ENV" ]; then
    command -v openssl >/dev/null 2>&1 || die "需要 openssl 生成超管密钥"
    TOKEN="$(openssl rand -hex 32)"
    umask 077
    cat >"$DEV_ENV" <<EOF
# 本机 dev 网关实例（.dev/ 整目录已 gitignore）。这把密钥只用于本机联调，不是真实凭证。
# 换它 = 改这一行（重启不会自动换）。脚本此后只报指纹，不再打印明文。
LANGFIXER_GATEWAY=1
LANGFIXER_ADMIN_TOKEN=$TOKEN
LANGFIXER_COOKIE_SECURE=0
PORT=$LF_PORT
LANGFIXER_AUTH_DB=$DEV_DIR/auth.db
GEC_DB_FILE=$DEV_DIR/core.db
EOF
    chmod 600 "$DEV_ENV"
    # 首次生成本机密钥必须让人拿到这一次，否则没人登得进去
    printf '  已生成本机超管密钥（只打印这一次）：%s\n' "$TOKEN"
  else
    say "复用 .dev/gateway.env（重启不换钥匙）"
  fi
  for f in "$DEV_ENV"; do grep -q '^LANGFIXER_ADMIN_TOKEN=.\{32,\}' "$f" || die "密钥不足 32 字符"; done

  step "编译 debug 二进制（dev 形态用 debug：改完代码 update.sh 只要几秒）"
  cargo build -p api-server 2>&1 | tail -3

  step "启动后端 :$LF_PORT"
  local_stop
  local_start
  probe_json "$LF_LOCAL_URL" | head -c 120 >/dev/null
  probe_gate "$LF_LOCAL_URL"
  local_report
  if [ "$WEB_ONLY" = 0 ]; then
    step "启动前端 :4805（Vite，反代 /gec /auth /admin）"
    local_web_stop
    local_web_start
  fi
  step "完成"
  usage_hint_local
  say "数据落在 .dev/auth.db 与 .dev/core.db（刻意与仓库根的 langfixer.db 隔离）"
  say "日常更新：./update.sh local"
  ;;

# ══════════════════════════════════ 服务器 ══════════════════════════════════
server)
  step "服务器首次安装：$LF_SSH"
  mk_tmp
  require_repo server
  ssh_root 'command -v cargo >/dev/null && command -v python3 >/dev/null && command -v systemctl >/dev/null' \
    || die "$LF_SSH 缺 cargo / python3 / systemctl（交叉编译路径已废弃，见 deploy/gateway/README.md）"
  ssh_root "install -d -m 755 $LF_DIR $LF_DIR/bin $LF_DIR/data $LF_DIR/log $LF_SRC $LF_WEB_ROOT"
  say "目录就位：$LF_DIR{,/bin,/data,/src} 与静态根 $LF_WEB_ROOT"

  if ! ssh_root "test -f $LF_DIR/langfixer.env"; then
    step "生成 $LF_DIR/langfixer.env（600）"
    # 首装这一次**必须**把明文交出去，否则没人能登录；此后所有脚本只报指纹。
    # 来源是 env → 轮换端点会回 409，这把的换法是「改文件 + 重启」。
    TOKEN="$(ssh_root "openssl rand -hex 32")"
    ssh_root "umask 077; sed 's/^LANGFIXER_ADMIN_TOKEN=.*/LANGFIXER_ADMIN_TOKEN=$TOKEN/' >$LF_DIR/langfixer.env" \
      <"$REPO_ROOT/deploy/gateway/langfixer.env.example"
    ssh_root "chmod 600 $LF_DIR/langfixer.env && grep -c . $LF_DIR/langfixer.env >/dev/null"
    printf '  超管密钥（只打印这一次，立刻存进密码管理器）：%s\n' "$TOKEN"
    say "别忘了填 LLM_API_KEY / LLM_BASE_URL / LLM_MODEL（env 或界面上的设置，设置优先于 env）"
  else
    say "$LF_DIR/langfixer.env 已存在，未改动"
  fi

  if [ "$WEB_ONLY" = 1 ]; then
    deploy_web
    step "完成（只铺了前端）"
    exit 0
  fi

  step "铺源码：git archive HEAD → ${LF_SRC}（这棵树不是 git 树，由本脚本铺）"
  git -C "$REPO_ROOT" archive --format=tar HEAD | ssh_root "tar -x -C $LF_SRC"
  say "HEAD $HEAD_SHA 已铺"

  step "铺运行时资产与 systemd 单元"
  for d in "${LF_ASSET_DIRS[@]}"; do
    ssh_root "install -d -m 755 $LF_DIR/$d && cp -a $LF_SRC/$d/. $LF_DIR/$d/"
  done
  say "资产：${LF_ASSET_DIRS[*]}（models/ 的 gguf 不铺，服务器走 GEC_OFFLINE=1）"
  for u in langfixer.service langfixer-retention.service langfixer-retention.timer; do
    if ssh_root "test -f /etc/systemd/system/$u"; then
      if ssh_root "cmp -s $LF_SRC/deploy/gateway/$u /etc/systemd/system/$u"; then
        say "单元 $u 与仓库一致"
      else
        warn "单元 /etc/systemd/system/$u 与仓库不同 → 未覆盖（要覆盖：LF_FORCE_UNITS=1 重跑）"
      fi
    else
      ssh_root "cp $LF_SRC/deploy/gateway/$u /etc/systemd/system/$u"
      say "单元 $u 已装"
    fi
  done
  if [ "${LF_FORCE_UNITS:-0}" = 1 ]; then
    for u in langfixer.service langfixer-retention.service langfixer-retention.timer; do
      ssh_root "cp $LF_SRC/deploy/gateway/$u /etc/systemd/system/$u"
    done
    warn "LF_FORCE_UNITS=1：三个单元已按仓库覆写"
  fi
  ssh_root "install -m 755 $LF_SRC/deploy/gateway/langfixer-retention.sh $LF_DIR/bin/langfixer-retention.sh"
  ssh_root "systemctl daemon-reload && systemctl enable --now langfixer-retention.timer"
  say "保留期清理 timer 已 enable（LANGFIXER_RETENTION_DAYS，默认 7 天）"

  step "服务器本机编译 release（4 核冷编译约 2-8 分钟）"
  ssh_root "install -d $LF_DIR/log && : >$LF_DIR/log/lf-setup.log"
  build_start_remote "$LF_DIR/log/lf-setup.log"
  build_wait "$LF_DIR/log/lf-setup.log"

  step "装二进制并启动服务"
  ssh_root "install -m 755 $LF_SRC/target/release/api-server $LF_DIR/bin/api-server && \
    systemctl enable --now $LF_SERVICE && systemctl restart $LF_SERVICE && sleep 3 && \
    systemctl is-active $LF_SERVICE" | sed 's/^/  langfixer: /'

  deploy_web
  step "健康与身份"
  server_report
  ssh_root "printf '  保留期 timer: %s\n' \"\$(systemctl is-active langfixer-retention.timer)\""
  ssh_root "printf '%s %s\n' '$HEAD_SHA' \"\$(date -Is)\" >$LF_DIR/.deployed"

  step "还差两步（脚本刻意不动反向代理与外部容器）"
  cat <<EOF
  1) Caddy 站点：把 deploy/gateway/Caddyfile.snippet 追加进
     /dnmp/services/caddy/config/caddyfile，然后
     docker exec caddy caddy reload --config /config/caddyfile
  2) 中文分词 sidecar（可选）：见 deploy/gateway/README.md「中文 NLP sidecar」；
     不起也行，zh 管线会自动退化为纯规则离线模式
  冒烟清单也在同一份 README 末尾。
EOF
  ;;

*)
  usage
  die "用法: ./setup.sh local|server [--web|--web-only]"
  ;;
esac
