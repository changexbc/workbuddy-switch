# wb-switch-rust

WorkBuddy（腾讯 AI 编程助手）账号切换工具 —— Rust / Tauri 桌面版。

多账号共享登录态（`workbuddy-desktop.info`），一键切换 WorkBuddy 登录账号，并支持将当前账号的会话复制给目标账号（云端归属目标）。

> 与 Python 版（仓库根目录 `server.py`）功能对齐，共享同一数据目录 `~/.wb-switch/`，互不破坏数据。

## 功能

| 模块 | 说明 |
| --- | --- |
| 账号管理 | OAuth 扫码登录、从本机导入、手动添加 token、删除账号 |
| 账号切换 | 备份认证文件 → 关闭 WorkBuddy → 写入目标账号 → 重启，切换过程实时进度反馈 |
| 会话复制 | 将当前账号勾选的会话以新 id 复制给目标账号（jsonl 正文 + `workbuddy.db` 索引 + edge-sync 注册） |
| 自动签到 | 每日在配置时间段内随机时刻签到；一键全部签到；30 天签到日志 |
| Token 保活 | 惰性刷新（操作前不足阈值刷新）+ 每日保活（不足阈值天数刷新），避免 refresh token 过期 |
| 自动更新 | 配置 GitHub Releases 源检查新版本；整包更新经签名校验（tauri-updater） |
| 权限检测 | macOS 授权引导（App 管理 / 完全磁盘访问拖拽授权 + 自动检测） |

## 使用

1. **添加账号**：账号页 →「扫码登录」（OAuth device flow）或「从本机导入」「手动添加」
2. **切换账号**：账号卡片 →「切换」，可勾选复制当前会话
3. **自动签到**：设置 → 自动签到，开启并配置时间段
4. **更新**：设置 → 自动更新，填写 GitHub owner/repo/token 后检查更新

### macOS 权限说明

切换账号需要写入 WorkBuddy 认证文件，macOS 要求授权「App 管理」（或「完全磁盘访问」）：

1. 首次切换报「无权限」时，点「打开系统设置」
2. 优先在 **App 管理** 里打开 wb-switch 开关；若没有，则去 **完全磁盘访问** 把 wb-switch 拖进带箭头的框
3. 授权后重启本应用生效；设置页「权限检测」可随时验证

## 开发

环境要求：Node.js ≥ 20、Rust stable、macOS（或 Windows/Linux）。

```bash
npm install
npm run tauri dev        # 开发模式
npm run build:app        # 构建 debug .app（含前端资源补丁）
npm run build:app:release  # 构建 release .app + 签名更新包
```

签名密钥（自动更新用）存放于 `~/.wb-switch/wb-switch-updater.key`，构建脚本通过
`TAURI_SIGNING_PRIVATE_KEY` 注入。发布新版本时：

1. `npm run build:app:release` 生成 `.app.tar.gz` + `.sig`
2. `sh scripts/gen-update-json.sh <owner> <repo>` 生成 `latest-macos-aarch64.json`
3. 将 `.app.tar.gz`、`.sig`、`latest-*.json` 一并上传到 GitHub Release

## 目录结构

```
src-tauri/
  src/
    commands.rs      # Tauri command 薄包装（对应 Python 版 HTTP API）
    modules/
      account.rs     # 账号库 ~/.wb-switch/accounts.json
      auth_file.rs   # workbuddy-desktop.info 读写/备份
      oauth.rs       # OAuth 登录
      process.rs     # WorkBuddy 进程控制
      switch.rs      # 切换流程（备份→关→复制会话→写认证→启动）
      session.rs     # 会话列表 / 复制
      checkin.rs     # 签到
      refresh.rs     # token 刷新/保活
      update.rs      # 更新检查
      config.rs      # 常量/路径/工具
src/
  components/        # React 组件
  pages/             # 账号页/设置页
  lib/               # API 封装/类型
```

## 许可

[MIT](./LICENSE)
