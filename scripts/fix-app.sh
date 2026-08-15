#!/bin/bash
# 构建后把前端 dist 补进 .app
# 背景：tauri bundler 在本机偶发不把 frontendDist 复制进 .app（Resources 缺 dist 导致空白/旧界面）。
# 用法：tauri build 后执行 `sh scripts/fix-app.sh`
set -e
cd "$(dirname "$0")/.." || exit 1

if [ ! -d dist ]; then
  echo "fix-app: 未找到 dist（先运行 npm run build）"
  exit 0
fi

for profile in debug release; do
  APP="src-tauri/target/$profile/bundle/macos/wb-switch.app"
  if [ -d "$APP" ]; then
    rm -rf "$APP/Contents/Resources/dist" 2>/dev/null || true
    cp -R dist "$APP/Contents/Resources/dist"
    echo "fix-app: dist 已复制到 $APP/Contents/Resources/dist"
  fi
done
