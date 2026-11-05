#!/usr/bin/env bash
# LangFixer 发布推送：把当前分支与全部标签同步到发布地
#   origin  = Gitea (gitea.jiulu.ltd)
#   public  = GitHub (github.com)
#   atomgit = AtomGit/GitCode (atomgit.com)
# 用法: scripts/publish.sh [origin|public|atomgit ...] [--dry-run]
#   不带源参数 = 全部源；指定源 = 只推这些源。单个源失败不中断其余源。
set -uo pipefail
cd "$(dirname "$0")/.."

ALL_REMOTES=(origin public atomgit)
DRY=0
REMOTES=()
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY=1 ;;
        *)         REMOTES+=("$arg") ;;
    esac
done
[[ ${#REMOTES[@]} -eq 0 ]] && REMOTES=("${ALL_REMOTES[@]}")

PUSH_ARGS=(--tags)
if [[ $DRY -eq 1 ]]; then
    PUSH_ARGS+=(--dry-run)
    echo "==> dry-run 模式（不会真正推送）"
fi

branch="$(git rev-parse --abbrev-ref HEAD)"
echo "==> 当前分支: $branch ($(git rev-parse --short HEAD))"

failed=()
for remote in "${REMOTES[@]}"; do
    if ! git remote get-url "$remote" >/dev/null 2>&1; then
        echo "!! 跳过：remote '$remote' 不存在"
        failed+=("$remote")
        continue
    fi
    echo "==> push $remote ($branch + tags)"
    if ! git push "$remote" "$branch" "${PUSH_ARGS[@]}"; then
        echo "!! push $remote 失败（若该源领先本地: git pull --rebase $remote ${branch}）"
        failed+=("$remote")
    fi
done

if [[ ${#failed[@]} -gt 0 ]]; then
    echo "==> 发布推送未全部完成: ${failed[*]}"
    exit 1
fi
echo "==> 发布推送完成"
