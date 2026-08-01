#!/usr/bin/env bash
# tape 一键发版脚本：bump 版本 -> 生成 CHANGELOG -> 提交 -> 打 tag -> 推送（触发 CI 自动发布）
# 用法：./release.sh [--dry-run] <版本号>     例：./release.sh 0.1.2 或 ./release.sh v0.1.2
set -euo pipefail

REPO_URL="https://github.com/yujinping/tape"

DRY=0
if [ "${1:-}" = "--dry-run" ]; then
    DRY=1
    shift
fi

if [ $# -ne 1 ]; then
    echo "用法: ./release.sh [--dry-run] <版本号>"
    echo "  例: ./release.sh 0.1.2   或   ./release.sh v0.1.2"
    exit 1
fi

VER="${1#v}"
TAG="v$VER"

if ! echo "$VER" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "错误: 版本号格式应为 x.y.z，收到 '$VER'"
    exit 1
fi

# ---- 前置检查 ----
command -v git-cliff >/dev/null 2>&1 || { echo "错误: 缺少 git-cliff，请先安装（cargo install git-cliff）"; exit 1; }
[ "$(git branch --show-current)" = "main" ] || { echo "错误: 请先切换到 main 分支"; exit 1; }
if [ -n "$(git status --porcelain)" ]; then
    echo "错误: 工作区有未提交改动，请先提交或 stash："
    git status --short
    exit 1
fi
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    echo "错误: 本地 tag $TAG 已存在"
    exit 1
fi
if git ls-remote --exit-code --tags origin "$TAG" >/dev/null 2>&1; then
    echo "错误: 远程已存在 tag $TAG（可先 git push origin :$TAG 删除）"
    exit 1
fi

# RELEASE_NOTES.md 若仍是模板占位则提醒
if grep -q '（示例：' RELEASE_NOTES.md 2>/dev/null; then
    echo "提醒: RELEASE_NOTES.md 仍是模板占位，建议先编辑「本版本变更」小节"
fi

echo "================================================"
echo "将发布 tape $TAG"
echo "  1. bump Cargo.toml / Cargo.lock  ->  $VER"
echo "  2. git cliff 生成 CHANGELOG.md"
echo "  3. 提交 'chore: release $TAG' 并打 tag"
echo "  4. push main 与 tag（触发 CI 自动构建并发布 Release）"
echo "================================================"
if [ "$DRY" = "1" ]; then
    echo "[dry-run] 未执行任何修改，以上为将执行的操作"
    exit 0
fi
read -rp "确认继续? [y/N] " ans
case "$ans" in
    y | Y) ;;
    *) echo "已取消"; exit 1 ;;
esac

# ---- bump 版本（Cargo.toml 与 Cargo.lock 中 tape 包）----
python3 - "$VER" <<'PY'
import re, sys
ver = sys.argv[1]
p = "Cargo.toml"
s = open(p).read()
s = re.sub(r'^version = "[^"]+"', f'version = "{ver}"', s, count=1, flags=re.M)
open(p, "w").write(s)
p = "Cargo.lock"
s = open(p).read()
s = re.sub(r'(name = "tape"\nversion = ")[^"]+(")', rf"\g<1>{ver}\g<2>", s, count=1)
open(p, "w").write(s)
print(f"bump Cargo.toml / Cargo.lock -> {ver}")
PY

# ---- 生成 CHANGELOG ----
git cliff -o CHANGELOG.md
echo "已生成 CHANGELOG.md"

# ---- 提交 / 打 tag / 推送 ----
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore: release $TAG"
git tag "$TAG"

git push origin main
git push origin "$TAG"
echo "已发布 $TAG：$REPO_URL/releases/tag/$TAG"
