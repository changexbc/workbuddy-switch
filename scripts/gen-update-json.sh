#!/bin/bash
# 生成 tauri updater 版本清单 latest-macos-aarch64.json
# 用法：release 构建后执行 `sh scripts/gen-update-json.sh [owner] [repo]`
# 产物：src-tauri/target/release/bundle/macos/latest-macos-aarch64.json
# 发布时把 .app.tar.gz + .app.tar.gz.sig + 本 json 一起传到 GitHub Release 即可。
set -e
cd "$(dirname "$0")/.." || exit 1

OWNER="${1:-wb-switch}"
REPO="${2:-wb-switch}"
VERSION=$(grep '^version' src-tauri/Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
SIG_FILE="src-tauri/target/release/bundle/macos/wb-switch.app.tar.gz.sig"
JSON_FILE="src-tauri/target/release/bundle/macos/latest-macos-aarch64.json"

if [ ! -f "$SIG_FILE" ]; then
  echo "gen-update-json: 未找到 $SIG_FILE（先运行 release 构建）"
  exit 0
fi

SIGNATURE=$(cat "$SIG_FILE")
PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

cat > "$JSON_FILE" << EOF
{
  "version": "$VERSION",
  "notes": "",
  "pub_date": "$PUB_DATE",
  "platforms": {
    "macos-aarch64": {
      "signature": "$SIGNATURE",
      "url": "https://github.com/$OWNER/$REPO/releases/latest/download/wb-switch.app.tar.gz"
    }
  }
}
EOF
echo "gen-update-json: 已生成 $JSON_FILE"
