#!/bin/bash
# 统一 bump 版本：usage: sh scripts/bump-version.sh 0.1.5
set -e
V=$1
[ -z "$V" ] && echo "用法: sh scripts/bump-version.sh <新版本>" && exit 1
cd "$(dirname "$0")/.."
perl -pi -e "s/\"version\": \"[0-9.]+\"/\"version\": \"$V\"/" package.json src-tauri/tauri.conf.json npm/package.json
perl -pi -e "s/^version = \"[0-9.]+\"/version = \"$V\"/" src-tauri/Cargo.toml crates/wb-switch-core/Cargo.toml crates/wb-switch-server/Cargo.toml
echo "所有版本已同步为 $V"
