#!/bin/bash
# 纯命令行打 dmg（不依赖 Finder/AppleScript，CI 无头环境可用）
# 用法：sh scripts/make-dmg.sh <版本> <arch> [.app 路径]
#   arch: aarch64 | x64
#   .app 路径默认 target/release/bundle/macos/workbuddy-switch.app
#   DMG 内含「应用程序」文件夹链接，拖入即安装到 /Applications
#
# dmgbuild writes the Finder layout and invokes hdiutil under the hood.  Do
# not fall back to a plain hdiutil image: that would silently lose the large
# icon positions and drag arrow this script is responsible for.
#
# Keep this wrapper POSIX-shell compatible: CI invokes it with `sh`, and macOS
# users commonly do the same when running the release script locally.
set -eu

usage() {
  echo "用法: sh scripts/make-dmg.sh <版本> <arch> [.app 路径]" >&2
}

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  usage
  exit 1
fi

V=$1
ARCH=$2
APP=${3:-target/release/bundle/macos/workbuddy-switch.app}

if [ -z "$V" ] || [ -z "$ARCH" ]; then
  usage
  exit 1
fi

if ! command -v dmgbuild >/dev/null 2>&1; then
  echo "未找到 dmgbuild；请先安装固定版本的 dmgbuild（CI 会自动准备）" >&2
  exit 1
fi

if [ ! -d "$APP" ]; then
  echo "未找到 .app: $APP" >&2
  exit 1
fi

APP=$(cd "$APP" && pwd -P)
SCRIPT_DIR=$(dirname "$0")
SETTINGS_DIR=$(cd "$SCRIPT_DIR" && pwd -P)
SETTINGS="$SETTINGS_DIR/dmgbuild-settings.py"
if [ ! -f "$SETTINGS" ]; then
  echo "未找到 dmgbuild 设置文件: $SETTINGS" >&2
  exit 1
fi

OUT="workbuddy-switch_${V}_${ARCH}.dmg"
TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/workbuddy-switch-dmg.XXXXXX")
TMP_OUT="$TMP_DIR/$OUT"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "打 dmg: $APP → $OUT"
dmgbuild \
  -s "$SETTINGS" \
  -D "app=$APP" \
  "workbuddy-switch" \
  "$TMP_OUT"

# Publish atomically after dmgbuild has completed.  A failed build therefore
# cannot leave a partial DMG at the release path.
mv -f "$TMP_OUT" "$OUT"
echo "✓ 完成: $OUT ($(stat -f%z "$OUT") 字节)"
