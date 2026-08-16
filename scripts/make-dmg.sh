#!/bin/bash
# 纯命令行打 dmg（不依赖 Finder/AppleScript，CI 无头环境可用）
# 用法：sh scripts/make-dmg.sh <版本> <arch> [.app 路径]
#   arch: aarch64 | x64
#   .app 路径默认 target/release/bundle/macos/workbuddy-switch.app
set -e

V=$1
ARCH=$2
APP=${3:-target/release/bundle/macos/workbuddy-switch.app}

[ -z "$V" ] && echo "用法: sh scripts/make-dmg.sh <版本> <arch> [.app 路径]" && exit 1
[ -z "$ARCH" ] && echo "用法: sh scripts/make-dmg.sh <版本> <arch> [.app 路径]" && exit 1
[ -d "$APP" ] || { echo "未找到 .app: $APP"; exit 1; }

OUT="workbuddy-switch_${V}_${ARCH}.dmg"
echo "打 dmg: $APP → $OUT"
hdiutil create -volname "workbuddy-switch" -srcfolder "$APP" -ov -format UDZO "$OUT"
echo "✓ 完成: $OUT ($(stat -f%z "$OUT") 字节)"
