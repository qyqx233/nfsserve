#!/usr/bin/env bash
set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
UPSTREAM_BRANCH="main"
MY_BRANCH="${MY_BRANCH:-my-main}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info()  { echo -e "${BLUE}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[OK]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
err()   { echo -e "${RED}[ERR]${NC} $*"; }

usage() {
    cat <<EOF
Usage: ./$SCRIPT_NAME <command> [args]

Commands:
  init <your-fork-url>     初始化 fork 工作流（重命名 origin -> upstream，添加你的 fork）
  sync [your-branch]       同步上游更新到 main，并 rebase 你的分支（默认: $MY_BRANCH）
  feature <name>           基于 upstream/main 创建新功能分支
  finish [your-branch]     合并你的开发分支到 main，推送至你的 fork
  push [branch]            推送当前/指定分支到你的 fork
  status                   查看仓库 fork 状态
  help                     显示此帮助

环境变量:
  MY_BRANCH                设置你的长期开发分支名（默认: my-main）

Examples:
  # 1. 初始化（你已经在 huggingface/nfsserve 的 clone 中）
  ./$SCRIPT_NAME init https://github.com/<你的用户名>/nfsserve.git

  # 2. 创建功能分支开发
  ./$SCRIPT_NAME feature add-single-binary

  # 3. 完成功能，合并到你的主分支
  ./$SCRIPT_NAME finish

  # 4. 上游更新了，同步到本地
  ./$SCRIPT_NAME sync

  # 5. 推送你的主分支到自己的 fork
  ./$SCRIPT_NAME push main
EOF
}

check_git_repo() {
    if ! git rev-parse --git-dir > /dev/null 2>&1; then
        err "当前目录不是 Git 仓库"
        exit 1
    fi
}

cmd_init() {
    local fork_url="${1:-}"
    if [[ -z "$fork_url" ]]; then
        err "请提供你的 fork 地址，例如: ./$SCRIPT_NAME init https://github.com/<user>/nfsserve.git"
        exit 1
    fi

    check_git_repo

    info "检查 remote 配置..."
    local current_origin
    current_origin=$(git remote get-url origin 2>/dev/null || true)

    if [[ -z "$current_origin" ]]; then
        err "没有找到 origin remote"
        exit 1
    fi

    # 提取 fork 的 owner/repo 用于比较
    local fork_repo
    fork_repo=$(echo "$fork_url" | sed -E 's|https?://[^/]+/||; s|\.git$||')

    # 如果 origin 已经是你的 fork，不做任何事
    if [[ "$current_origin" == *"$fork_repo"* ]] || [[ "$current_origin" == "$fork_url" ]]; then
        ok "origin 已经是你的 fork: $current_origin"
    else
        info "将现有 origin 重命名为 upstream: $current_origin"
        git remote rename origin upstream
        ok "upstream 设置完成"
    fi

    # 添加你的 fork 为 origin（如果不存在）
    if ! git remote get-url origin > /dev/null 2>&1; then
        info "添加你的 fork 为 origin: $fork_url"
        git remote add origin "$fork_url"
    else
        local existing_origin
        existing_origin=$(git remote get-url origin)
        if [[ "$existing_origin" != "$fork_url" ]]; then
            warn "origin 已存在且地址不同: $existing_origin"
            read -rp "是否替换为 $fork_url? [y/N] " ans
            if [[ "$ans" =~ ^[Yy]$ ]]; then
                git remote set-url origin "$fork_url"
            else
                info "跳过替换 origin"
            fi
        fi
    fi

    ok "初始化完成！"
    cmd_status
}

cmd_sync() {
    local branch="${1:-$MY_BRANCH}"
    check_git_repo

    info "拉取上游 ($UPSTREAM_BRANCH) 最新代码..."
    git fetch upstream

    local has_changes
    has_changes=$(git rev-list --count "HEAD..upstream/$UPSTREAM_BRANCH" 2>/dev/null || echo "0")
    if [[ "$has_changes" -eq 0 ]]; then
        ok "上游没有新提交，已经是最新"
    else
        info "上游有 $has_changes 个新提交"
    fi

    # 确保本地 main 跟踪 upstream/main
    if git show-ref --verify --quiet "refs/heads/$UPSTREAM_BRANCH"; then
        git checkout "$UPSTREAM_BRANCH"
        git reset --hard "upstream/$UPSTREAM_BRANCH"
    else
        git checkout -B "$UPSTREAM_BRANCH" "upstream/$UPSTREAM_BRANCH"
    fi
    ok "本地 $UPSTREAM_BRANCH 已更新到上游最新"

    # 推送 main 到自己的 fork（保持同步）
    if git remote get-url origin > /dev/null 2>&1; then
        info "推送 $UPSTREAM_BRANCH 到你的 fork..."
        git push origin "$UPSTREAM_BRANCH" --force-with-lease
    fi

    # 如果用户分支存在，rebase 到最新 main
    if git show-ref --verify --quiet "refs/heads/$branch"; then
        info "将 $branch rebase 到 $UPSTREAM_BRANCH..."
        git checkout "$branch"
        if git rebase "$UPSTREAM_BRANCH"; then
            ok "$branch 同步完成"
        else
            err "rebase 出现冲突，请解决后执行: git rebase --continue"
            exit 1
        fi
    else
        warn "分支 $branch 不存在，跳过 rebase"
    fi
}

cmd_feature() {
    local name="${1:-}"
    if [[ -z "$name" ]]; then
        err "请提供功能分支名，例如: ./$SCRIPT_NAME feature add-auth"
        exit 1
    fi

    check_git_repo
    git fetch upstream

    local branch_name="feat/$name"
    info "创建功能分支: $branch_name"
    git checkout -b "$branch_name" "upstream/$UPSTREAM_BRANCH"
    ok "已创建并切换到 $branch_name"
    info "开发完成后运行: ./$SCRIPT_NAME finish"
}

cmd_finish() {
    local branch="${1:-$MY_BRANCH}"
    check_git_repo

    local current_branch
    current_branch=$(git branch --show-current)

    if [[ "$current_branch" == "$UPSTREAM_BRANCH" ]]; then
        err "不要在 $UPSTREAM_BRANCH 上直接开发。请切换到功能分支"
        exit 1
    fi

    info "合并 $current_branch -> $branch..."

    # 确保目标分支存在
    if ! git show-ref --verify --quiet "refs/heads/$branch"; then
        info "创建开发分支 $branch"
        git checkout -b "$branch" "$UPSTREAM_BRANCH"
    else
        git checkout "$branch"
    fi

    git merge "$current_branch" --no-ff -m "Merge feature: $current_branch"
    ok "已合并到 $branch"

    # 清理功能分支
    read -rp "是否删除功能分支 $current_branch? [y/N] " ans
    if [[ "$ans" =~ ^[Yy]$ ]]; then
        git branch -d "$current_branch"
    fi
}

cmd_push() {
    local branch="${1:-$(git branch --show-current)}"
    check_git_repo

    if ! git remote get-url origin > /dev/null 2>&1; then
        err "没有配置 origin（你的 fork），请先运行: ./$SCRIPT_NAME init <你的fork地址>"
        exit 1
    fi

    info "推送 $branch 到你的 fork..."
    git push -u origin "$branch"
    ok "推送完成"
}

cmd_status() {
    check_git_repo

    echo ""
    echo "========== Fork 状态 =========="
    echo ""

    echo "[Remotes]"
    git remote -v
    echo ""

    echo "[当前分支]"
    git branch -vv
    echo ""

    echo "[上游提交差异]"
    local ahead behind
    ahead=$(git rev-list --count "upstream/$UPSTREAM_BRANCH..HEAD" 2>/dev/null || echo "?")
    behind=$(git rev-list --count "HEAD..upstream/$UPSTREAM_BRANCH" 2>/dev/null || echo "?")
    echo "  当前分支领先 upstream/$UPSTREAM_BRANCH: $ahead 提交"
    echo "  当前分支落后 upstream/$UPSTREAM_BRANCH: $behind 提交"
    echo ""

    echo "[工作区状态]"
    if git diff --quiet && git diff --cached --quiet; then
        ok "工作区干净"
    else
        git status -s
    fi
    echo ""
    echo "==============================="
}

# Main
case "${1:-help}" in
    init)       shift; cmd_init "$@" ;;
    sync)       shift; cmd_sync "$@" ;;
    feature)    shift; cmd_feature "$@" ;;
    finish)     shift; cmd_finish "$@" ;;
    push)       shift; cmd_push "$@" ;;
    status)     shift; cmd_status "$@" ;;
    help|--help|-h) usage ;;
    *)
        err "未知命令: $1"
        usage
        exit 1
        ;;
esac
