#!/bin/bash
# 生成 tauri updater 版本清单 latest-macos-aarch64.json
# 用法：release 构建后执行 `sh scripts/gen-update-json.sh [owner] [repo]`
# 产物：target/release/bundle/macos/latest-macos-aarch64.json
# 发布时把 .app.tar.gz + .app.tar.gz.sig + 本 json 一起传到 GitHub Release 即可。
set -e
cd "$(dirname "$0")/.." || exit 1

OWNER="${1:-changexbc}"
REPO="${2:-workbuddy-switch}"
VERSION=$(grep '^version' src-tauri/Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
BUNDLE_DIR="${BUNDLE_DIR:-target/release/bundle/macos}"
if [ ! -d "$BUNDLE_DIR" ] && [ -d "src-tauri/target/release/bundle/macos" ]; then
  BUNDLE_DIR="src-tauri/target/release/bundle/macos"
fi
SIG_FILE=$(find "$BUNDLE_DIR" -maxdepth 1 -type f -name '*.app.tar.gz.sig' -print -quit)
ARCHIVE_FILE="${SIG_FILE%.sig}"
JSON_FILE="$BUNDLE_DIR/latest-macos-aarch64.json"

if [ -z "$SIG_FILE" ] || [ ! -f "$SIG_FILE" ] || [ ! -f "$ARCHIVE_FILE" ]; then
  echo "gen-update-json: 未找到签名更新包（先运行 release 构建：$BUNDLE_DIR）"
  exit 0
fi

SIGNATURE=$(cat "$SIG_FILE")
ARCHIVE_NAME=$(basename "$ARCHIVE_FILE")
PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

cat > "$JSON_FILE" << EOF
{
  "version": "$VERSION",
  "notes": "",
  "pub_date": "$PUB_DATE",
  "platforms": {
    "macos-aarch64": {
      "signature": "$SIGNATURE",
      "url": "https://github.com/$OWNER/$REPO/releases/latest/download/$ARCHIVE_NAME"
    }
  }
}
EOF
echo "gen-update-json: 已生成 $JSON_FILE"
