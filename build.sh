#!/usr/bin/env bash
# box-proxy 一键构建脚本
# 用法:
#   ./build.sh         构建当前系统版本（native，产物 dist/box-proxy）
#   ./build.sh mac     构建 macOS 版本（产物 dist/box-proxy-macos）
#   ./build.sh win     交叉编译 Windows x64 版本（产物 dist/box-proxy-windows-x64.exe，默认 UPX 压缩，UPX=0 跳过）
set -euo pipefail

cd "$(dirname "$0")"

TARGET="${1:-native}"
DIST_DIR="dist"

# 找到当前平台的 release 二进制（Windows 上为 .exe）
native_binary() {
    if [ -f target/release/box-proxy.exe ]; then
        echo "target/release/box-proxy.exe"
    else
        echo "target/release/box-proxy"
    fi
}

case "$TARGET" in
    native|"")
        echo "==> 构建当前系统版本（native）"
        cargo build --release
        mkdir -p "$DIST_DIR"
        cp "$(native_binary)" "$DIST_DIR/box-proxy"
        echo "==> 产物: $DIST_DIR/box-proxy"
        ;;
    mac|macos)
        echo "==> 构建 macOS 版本"
        cargo build --release
        mkdir -p "$DIST_DIR"
        cp "$(native_binary)" "$DIST_DIR/box-proxy-macos"
        echo "==> 产物: $DIST_DIR/box-proxy-macos"
        ;;
    win|windows)
        echo "==> 交叉编译 Windows x64 版本（x86_64-pc-windows-gnu）"
        if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
            echo "错误: 未找到 mingw 链接器 x86_64-w64-mingw32-gcc" >&2
            echo "macOS 请先安装: brew install mingw-w64" >&2
            exit 1
        fi
        rustup target add x86_64-pc-windows-gnu
        cargo build --release --target x86_64-pc-windows-gnu
        mkdir -p "$DIST_DIR"
        cp target/x86_64-pc-windows-gnu/release/box-proxy.exe "$DIST_DIR/box-proxy-windows-x64.exe"
        if [ "${UPX:-1}" = "1" ] && command -v upx >/dev/null 2>&1; then
            echo "==> 使用 UPX 压缩 Windows 可执行文件（UPX=0 可跳过）"
            upx --best "$DIST_DIR/box-proxy-windows-x64.exe"
        elif [ "${UPX:-1}" = "1" ]; then
            echo "提示: 未安装 upx，跳过压缩（安装: brew install upx）"
        fi
        echo "==> 产物: $DIST_DIR/box-proxy-windows-x64.exe"
        ;;
    *)
        echo "用法: $0 [native|mac|win]" >&2
        echo "  （默认 native：构建当前系统版本）" >&2
        exit 1
        ;;
esac
