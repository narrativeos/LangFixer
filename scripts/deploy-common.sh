#!/usr/bin/env bash
# 部署共用件：**被 source 的函数库**，不要直接执行（根目录 setup.sh / update.sh 用）。
#
# 落地约定与数据红线见 deploy/gateway/README.md 与 docs/GATEWAY_AUTH.md。
# 三条与身份/数据有关的红线集中在这里，两个脚本共用，别在调用处各写一遍：
#   1. 永不打印超管密钥——只打印指纹（SHA-256 前 8 位）与来源；
#   2. 永不换密钥——换钥匙是显式动作（改 env 后重启，或 POST /admin/admin-token/rotate），
#      重启本身不换钥匙（优先级：env → gw_meta.admin_token → 才生成并落库）；
#   3. 同步路径里永远不含 env、数据库、langfixer-settings.json（铺的是 git 跟踪的文件）。

set -euo pipefail

# ── 可用环境变量覆盖的部署目标 ────────────────────────────────────────────
LF_SSH="${LF_SSH:-47.94.172.217}"                  # root 直连；也可给 ~/.ssh/config 别名
LF_URL="${LF_URL:-https://langfixer.jiulu.ltd}"   # 公网入口（只由 Caddy 持有）
LF_DIR="${LF_DIR:-/srv/langfixer}"                # 服务根：bin/ data/ *.env 都在这（web 根之外）
LF_SRC="${LF_SRC:-$LF_DIR/src}"                   # 编译树：**不是** git 树，由 git archive 铺
LF_WEB_ROOT="${LF_WEB_ROOT:-/dnmp/www/langfixer}" # Caddy 容器挂载的静态根：根放官网页（site/），产品 dist 放 app/
LF_WEB_APP="${LF_WEB_APP:-$LF_WEB_ROOT/app}"      # 网页产品 dist 的落位（对应 Caddy 的 handle /app/*）
LF_WEB_BASE="${LF_WEB_BASE:-/app/}"               # 服务器形态前端构建用的 vite --base（桌面版仍是默认 /，不受影响）
LF_SERVICE="${LF_SERVICE:-langfixer}"
LF_PORT="${LF_PORT:-4801}"
LF_LOCAL_URL="${LF_LOCAL_URL:-http://127.0.0.1:$LF_PORT}"
LF_BUILD_TIMEOUT="${LF_BUILD_TIMEOUT:-1800}" # 服务器 4 核冷编译留足余量
LF_ALLOW_DIRTY="${LF_ALLOW_DIRTY:-}" # 非空 = 明知工作区脏仍继续（铺的是 HEAD）
LF_ASSET_DIRS=(decision-tables lexicons prompts) # 运行时资产；models/ 的 gguf 一律不铺

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEV_DIR="$REPO_ROOT/.dev" # 本机实例的密钥 / 隔离库 / 日志（整目录已 gitignore）
DEV_ENV="$DEV_DIR/gateway.env"
DEV_LOG="$DEV_DIR/gateway.log"
DEV_PID="$DEV_DIR/gateway.pid"
DEV_WEB_LOG="$DEV_DIR/web.log"
DEV_WEB_PID="$DEV_DIR/web.pid"

say() { printf '  %s\n' "$*"; }
warn() { printf '  ! %s\n' "$*" >&2; }
die() { printf '\n  ✗ %s\n' "$*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }
ssh_root() { ssh -o BatchMode=yes "$LF_SSH" "$@"; }

TMP=""
mk_tmp() {
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/langfixer-deploy-XXXXXX")"
  trap 'rm -rf "$TMP"' EXIT
}

# ── 仓库状态 ─────────────────────────────────────────────────────────────
require_repo() { # $1 = local|server（两种形态对「未提交改动」的语义不同）
  git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
    || die "$REPO_ROOT 不是 git 仓库"
  HEAD_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
  say "HEAD $(git -C "$REPO_ROOT" log -1 --format='%h %s' | cut -c1-72)"
  if [ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]; then
    if [ "${1:-server}" = local ]; then
      say "工作区有未提交改动：本机编译的是**工作树**源码，这些改动会生效"
    elif [ -n "$LF_ALLOW_DIRTY" ]; then
      warn "工作区有未提交改动且 LF_ALLOW_DIRTY 已设：铺出去的仍是 HEAD（${HEAD_SHA}）"
    else
      warn "工作区有未提交改动：铺出去的是 HEAD（${HEAD_SHA}），这些改动**不会**上服务器"
      warn "要一起上：先 git commit，或 LF_ALLOW_DIRTY=1 重跑"
    fi
  fi
}

# ── 探活 ─────────────────────────────────────────────────────────────────
# GET /auth/whoami 无需凭证也回 200，是同时证明「HTTP 栈活 + 路由挂载对」的最轻探针。
probe_json() { curl -sf -m 10 "$1/auth/whoami" 2>/dev/null || return 1; }
local_healthy() { probe_json "$LF_LOCAL_URL" >/dev/null 2>&1; }

# 网关隔离必须**可见**：无凭证 detect 应回 401。回 200 意味着访问隔离等于没开——比挂了更糟。
# $1 给了走本机 URL，留空走服务器侧 curl。
probe_gate() {
  local code
  if [ -n "${1:-}" ]; then
    code="$(curl -s -o /dev/null -m 10 -w '%{http_code}' -X POST -H 'content-type: application/json' \
      -d '{"lang":"zh","text":"他做事很任信。"}' "$1/gec/detect" 2>/dev/null || echo 000)"
  else
    code="$(ssh_root "curl -s -o /dev/null -m 10 -w '%{http_code}' -X POST -H 'content-type: application/json' -d '{\"lang\":\"zh\",\"text\":\"测试\"}' $LF_URL/gec/detect" || echo 000)"
  fi
  case "$code" in
    401) say "鉴权门在位：无凭证 detect → 401" ;;
    200) die "无凭证 detect 竟回 200：网关形态疑似未生效（LANGFIXER_GATEWAY=1？），访问隔离等于没开" ;;
    *) warn "鉴权探针回 ${code}（预期 401；单机形态本来就是开放的，可忽略本条）" ;;
  esac
}

# ── 服务器侧健康与身份（只报指纹） ────────────────────────────────────────
# 绕开 Caddy 直连网关，并用超管密钥真实跑一次 detect：只证明「HTTP 200」不够，
# 管线炸了或密钥对不上谁都不知道。整段走 base64 传一个临时脚本，避免四层引号转义。
SMOKE_SH='set -u
D="'"$LF_DIR"'"
B=$(grep "^LANGFIXER_BIND=" "$D/langfixer.env" | cut -d= -f2- | tr -d "\"" | head -1)
P=$(grep "^PORT=" "$D/langfixer.env" | cut -d= -f2- | tr -d "\"" | head -1)
A=$(grep "^LANGFIXER_ADMIN_TOKEN=" "$D/langfixer.env" | cut -d= -f2- | tr -d "\"" | head -1)
U="http://${B:-127.0.0.1}:${P:-4801}"
printf "  直连 %s\n" "$U"
W=$(curl -sS -m 15 "$U/auth/whoami" || true)
[ -n "$W" ] || { printf "  ✗ whoami 无响应\n" >&2; exit 1; }
printf "%s" "$W" | python3 -c "import json,sys;d=json.load(sys.stdin);print(\"  whoami: gateway=%s locked=%s\" % (d.get(\"gateway\"), d.get(\"locked\")))" || {
  printf "  ✗ whoami 不是 JSON: %s\n" "$(printf "%s" "$W" | head -c 120)" >&2; exit 1; }
G=$(curl -s -o /dev/null -m 10 -w "%{http_code}" "$U/admin/users")
case "$G" in
  401) printf "  鉴权门在位：无凭证 admin/users → 401\n" ;;
  200) printf "  ✗ 无凭证 admin/users 竟回 200：网关形态未生效，访问隔离等于没开\n" >&2; exit 1 ;;
  *) printf "  ! 鉴权探针回 %s（预期 401）\n" "$G" ;;
esac
N=$(curl -s -o /dev/null -m 40 -w "%{http_code}" -X POST -H "content-type: application/json" -H "Authorization: Bearer $A" -d "{\"lang\":\"zh\",\"text\":\"他做事很任信。\"}" "$U/gec/detect")
case "$N" in
  200) printf "  带超管密钥 detect → 200（管线与密钥都对）\n" ;;
  401) printf "  ✗ 带 env 那把密钥 detect 回 401：密钥与库里摘要不匹配\n" >&2; exit 1 ;;
  *) printf "  ! 带超管密钥 detect 回 %s\n" "$N" ;;
esac
printf "  env 那把：长度 %s 指纹 %s\n" "${#A}" "$(printf "%s" "$A" | sha256sum | cut -c1-8)"
M=$(python3 -c "import sqlite3;print((sqlite3.connect(\"$D/data/auth.db\").execute(\"select value from gw_meta where key='"'"'admin_token'"'"'\").fetchone() or [\"\"])[0])" 2>/dev/null || true)
if [ -n "$M" ]; then
  printf "  gw_meta 那把：指纹 %s（env 有值时它不会被使用，启动日志有 WARN）\n" "$(printf "%s" "$M" | sha256sum | cut -c1-8)"
else
  printf "  gw_meta 无值 → 这把归 env 管，rotate 端点会回 409\n"
fi
python3 -c "import json,os;p=os.environ.get(\"LANGFIXER_SETTINGS\",\"\") or \"$D/langfixer-settings.json\";d=json.load(open(p)) if os.path.exists(p) else {};print(\"  生效目录（settings 优先于 env）: tables=%s lexicons=%s\" % (d.get(\"tables_dir\",\"(未覆写)\"), d.get(\"lexicons_dir\",\"(未覆写)\")))" || true
printf "  服务 %s · MemoryCurrent %s · 启动于 %s\n" "$(systemctl is-active '"$LF_SERVICE"' 2>/dev/null)" \
  "$(systemctl show '"$LF_SERVICE"' -p MemoryCurrent --value 2>/dev/null)" \
  "$(systemctl show '"$LF_SERVICE"' -p ExecMainStartTimestamp --value 2>/dev/null)"'

# 服务器侧健康与身份：探活 + 密钥来源与指纹 + 生效目录 + 服务状态
server_report() {
  local b64
  b64="$(printf '%s' "$SMOKE_SH" | base64 | tr -d '\n')"
  ssh_root "echo $b64 | base64 -d >/tmp/.lf-smoke.sh && bash /tmp/.lf-smoke.sh; rc=\$?; rm -f /tmp/.lf-smoke.sh; exit \$rc"
}

# 公网入口（Caddy：根 = 官网页，/app/ = 网页产品）
web_probe() {
  local code
  code="$(curl -sL -o /dev/null -m 20 -w '%{http_code}' "$LF_URL/" 2>/dev/null || echo 000)"
  [ "$code" = 200 ] || die "官网入口 $LF_URL/ 回 $code"
  say "官网入口 $LF_URL/ → 200"
  code="$(curl -sL -o /dev/null -m 20 -w '%{http_code}' "$LF_URL/app/" 2>/dev/null || echo 000)"
  [ "$code" = 200 ] || die "网页产品入口 $LF_URL/app/ 回 $code"
  say "网页产品入口 $LF_URL/app/ → 200"
}

# ── 编译哨兵 ─────────────────────────────────────────────────────────────
# 用日志哨兵判定，**不要**改成 pgrep / pkill -f "cargo build --release"：那条命令自身
# 的命令行里就含这串字，会匹配到自己（脚本自杀 / 自匹配死循环），已经踩过两次。
build_wait() { # $1 = 远端日志路径
  local log="$1" waited=0 out
  while :; do
    out="$(ssh_root "tail -n 80 '$log'" 2>/dev/null || true)"
    if printf '%s\n' "$out" | grep -Eq '^error(\[|:)'; then
      printf '%s\n' "$out" | grep -E '^error' -A3 | head -12 >&2
      die "编译失败，完整日志：$LF_SSH:$log"
    fi
    if printf '%s\n' "$out" | grep -q 'Finished .*profile'; then
      say "编译完成（$((waited / 60)) 分 $((waited % 60)) 秒）"
      return 0
    fi
    sleep 10
    waited=$((waited + 10))
    [ "$waited" -ge "$LF_BUILD_TIMEOUT" ] && die "编译超过 ${LF_BUILD_TIMEOUT}s，见 $LF_SSH:$log"
  done
}

build_wait_local() { # $1 = 本地日志路径
  local log="$1" waited=0 out
  while :; do
    out="$(tail -n 80 "$log" 2>/dev/null || true)"
    if printf '%s\n' "$out" | grep -Eq '^error(\[|:)'; then
      printf '%s\n' "$out" | grep -E '^error' -A3 | head -12 >&2
      die "编译失败，完整日志：$log"
    fi
    if printf '%s\n' "$out" | grep -q 'Finished .*profile'; then
      say "编译完成（$((waited / 60)) 分 $((waited % 60)) 秒）"
      return 0
    fi
    sleep 5
    waited=$((waited + 5))
    [ "$waited" -ge "$LF_BUILD_TIMEOUT" ] && die "编译超过 ${LF_BUILD_TIMEOUT}s，见 $log"
  done
}

# ── 运行时资产对账 ───────────────────────────────────────────────────────
# 为什么要对账：规则开关会被服务**写回** lexicons/ 的 JSON（防漂移设计），决策表 PUT 同样
# 写回文件。无脑用 HEAD 覆盖会静默抹掉运维在界面上做的开关，所以先比指纹、再备份、才铺。
FP_PY='import hashlib,os,sys
h=hashlib.sha256()
for p in sys.stdin.read().split("\n"):
    p=p.strip()
    if p:
        h.update(p.encode()+b"|")
        h.update(open(p,"rb").read() if os.path.exists(p) else b"<missing>")
print(h.hexdigest()[:8])'

_asset_fp() { # $1 = 目录  $2 = 基准目录（默认本机仓库根；给远端绝对路径则走 ssh）
  local dir="$1" base="${2:-$REPO_ROOT}" b64
  git -C "$REPO_ROOT" ls-files -- "$dir" | LC_ALL=C sort >"$TMP/manifest.txt"
  [ -s "$TMP/manifest.txt" ] || die "git 清单为空：${dir}（路径写错了？）"
  if [ "$base" = "$REPO_ROOT" ]; then
    (cd "$REPO_ROOT" && python3 -c "$FP_PY" <"$TMP/manifest.txt")
  else
    b64="$(printf '%s' "$FP_PY" | base64 | tr -d '\n')"
    # 清单走 stdin、程序走 base64：不多一次 scp，也不在服务器上留脚本
    ssh_root "echo $b64 | base64 -d >/tmp/.lf-fp.py && cd $base && python3 /tmp/.lf-fp.py" \
      <"$TMP/manifest.txt"
  fi
}

# 铺之前说清哪些运行时资产会被 HEAD 覆盖：规则开关会被服务**写回** lexicons/ 的 JSON，
# 决策表 PUT 同样写回文件，无脑覆盖等于静默抹掉运维在界面上做的改动。
assets_report() {
  local d l r
  for d in "${LF_ASSET_DIRS[@]}"; do
    if ! ssh_root "test -d $LF_DIR/$d"; then
      warn "$d/ 在服务器上还不存在（setup.sh 首次铺）"
      continue
    fi
    l="$(_asset_fp "$d")"
    r="$(_asset_fp "$d" "$LF_DIR")"
    if [ "$l" = "$r" ]; then
      say "$d/ 与 HEAD 一致（${l}）"
    else
      warn "$d/ 运行时副本与 HEAD 不同（$l → ${r}）：服务器侧改动将被 HEAD 覆盖（已备份到 $LF_DIR/.backup/）"
    fi
  done
}

# 覆盖之前把运行时资产留档，运维在界面上做的开关被 HEAD 抹掉时还能捞回来
assets_backup() {
  local ts="$1" d
  for d in "${LF_ASSET_DIRS[@]}"; do
    ssh_root "test -d $LF_DIR/$d && tar -C $LF_DIR -czf $LF_DIR/.backup/$ts-$d.tgz $d" 2>/dev/null || true
  done
  say "运行时资产留档：$LF_DIR/.backup/$ts-*.tgz"
}

# 远端后台起 release 编译：stdin 关掉 + setsid，ssh 立刻返回而编译继续。
# 存活判定只走日志哨兵（build_wait），**不要**用 pgrep/pkill -f "cargo build --release"。
build_start_remote() { # $1 = 远端日志路径
  ssh_root "cd $LF_SRC && setsid nohup cargo build --release -p api-server >'$1' 2>&1 </dev/null & sleep 2; echo '  编译已在服务器后台启动'"
}

# ── 本机实例（网关形态 + 隔离库） ────────────────────────────────────────
local_listener() { lsof -nP -ti "tcp:$LF_PORT" 2>/dev/null | head -1; }

local_stop() {
  local pid="" cmd
  [ -f "$DEV_PID" ] && pid="$(cat "$DEV_PID" 2>/dev/null || true)"
  if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then pid="$(local_listener)"; fi
  if [ -z "$pid" ]; then
    say "$LF_PORT 空闲"
    return 0
  fi
  cmd="$(ps -o comm= -p "$pid" 2>/dev/null || echo '?')"
  case "$cmd" in
    *api-server*)
      kill "$pid" 2>/dev/null || true
      sleep 2
      kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
      say "已停本机实例（pid ${pid}）"
      ;;
    *) die "$LF_PORT 被 ${cmd}（pid ${pid}）占用，且不是 api-server：请先腾出端口" ;;
  esac
  rm -f "$DEV_PID"
}

local_start() { # 需要 .dev/gateway.env 就位（由 setup.sh local 生成）
  [ -f "$DEV_ENV" ] || die "缺 ${DEV_ENV}——先跑 ./setup.sh local"
  set -a
  # shellcheck disable=SC1090
  . "$DEV_ENV"
  set +a
  local bin="$REPO_ROOT/target/debug/api-server"
  [ -x "$bin" ] || die "缺 ${bin}——先 cargo build -p api-server"
  # 两条库路径必须显式给：缺省会落仓库根的 langfixer.db（真实产品库），
  # 而 DELETE /gec/history 是真删——见 docs/GATEWAY_AUTH.md 的事故记录。
  : "${LANGFIXER_AUTH_DB:=$DEV_DIR/auth.db}"
  : "${GEC_DB_FILE:=$DEV_DIR/core.db}"
  export LANGFIXER_AUTH_DB GEC_DB_FILE PORT="$LF_PORT"
  nohup "$bin" >"$DEV_LOG" 2>&1 &
  echo $! >"$DEV_PID"
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    if local_healthy; then
      say "本机实例已起：pid $(cat "$DEV_PID")，${LF_LOCAL_URL}（日志 .dev/gateway.log）"
      return 0
    fi
    sleep 1
  done
  tail -12 "$DEV_LOG" >&2
  die "本机实例起了但 whoami 不通，见 .dev/gateway.log"
}

local_web_start() { # vite（:4805，反代 /gec /auth /admin → 4801）
  # 4805 已被占用时**不再另起一个**：Vite 会静默换端口到 4806，反代与文档全都对不上。
  # 常见情形是 scripts/dev.sh 或手工起的 Vite 还在跑——那正是我们要的，沿用即可。
  if [ -n "$(lsof -nP -ti tcp:4805 2>/dev/null | head -1)" ]; then
    say "4805 已有前端在跑，沿用（不重复起，避免 Vite 换端口到 4806）"
    return 0
  fi
  if command -v pnpm >/dev/null 2>&1; then
    (
      cd "$REPO_ROOT/web-ui"
      [ -d node_modules ] || pnpm install --frozen-lockfile
      nohup pnpm dev >"$DEV_WEB_LOG" 2>&1 &
      echo $! >"$DEV_WEB_PID"
    )
  elif command -v npm >/dev/null 2>&1; then
    (
      cd "$REPO_ROOT/web-ui"
      [ -d node_modules ] || npm install --no-audit --no-fund
      nohup npm run dev >"$DEV_WEB_LOG" 2>&1 &
      echo $! >"$DEV_WEB_PID"
    )
  else
    die "找不到 pnpm/npm，无法起前端"
  fi
  sleep 5
  say "前端已起：http://localhost:4805（pid $(cat "$DEV_WEB_PID")，日志 .dev/web.log）"
}

local_web_stop() {
  local pid=""
  [ -f "$DEV_WEB_PID" ] && pid="$(cat "$DEV_WEB_PID" 2>/dev/null || true)"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    pkill -P "$pid" 2>/dev/null || true
    kill "$pid" 2>/dev/null || true
    say "已停前端（pid ${pid}）"
  fi
  rm -f "$DEV_WEB_PID"
}

local_report() {
  local who
  who="$(probe_json "$LF_LOCAL_URL" || true)"
  [ -n "$who" ] || die "$LF_LOCAL_URL 没在跑"
  printf '%s' "$who" |
    python3 -c 'import json,sys;d=json.load(sys.stdin);print("  whoami: gateway=%s locked=%s" % (d["gateway"], d["locked"]))'
  say "登录：$LF_LOCAL_URL/?token=<.dev/gateway.env 里的 LANGFIXER_ADMIN_TOKEN>"
  say "首次打开界面会要求初始化 admin 账号（激活密钥＝上面那把 LANGFIXER_ADMIN_TOKEN）"
}

# 用法提示里两份共用文本
usage_hint_local() {
  cat <<EOF
  本机形态（网关 + 隔离库，全部落在 .dev/）：
    界面   http://localhost:4805
    后端   $LF_LOCAL_URL/auth/whoami
    密钥   .dev/gateway.env 里的 LANGFIXER_ADMIN_TOKEN（脚本只报指纹）
EOF
}
