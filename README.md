# workbuddy-switch

WorkBuddy（腾讯 AI 编程助手）账号切换工具。两种形态：

- **桌面 App**：下载 `.app` 双击运行（Tauri，推荐日常使用）
- **npm / webui**：`npm i -g workbuddy-switch` 后运行 `workbuddy-switch`，浏览器打开操作界面

多账号共享登录态（`workbuddy-desktop.info`），一键切换 WorkBuddy 登录账号，并支持将当前账号的会话复制给目标账号（云端归属目标）。

## 快速开始

### npm 安装（webui）

```bash
npm i -g workbuddy-switch
workbuddy-switch              # 启动本地服务 + 自动打开浏览器
workbuddy-switch status       # 终端查看当前账号
```

webui 界面与桌面 App 一致：账号管理、切换、会话复制、自动签到、token 保活、更新检查。

### 桌面 App

从 GitHub Releases 下载对应平台 `.app`（macOS）双击运行。

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
2. 优先在 **App 管理** 里打开 workbuddy-switch 开关；若没有，则去 **完全磁盘访问** 把 workbuddy-switch 拖进带箭头的框
3. 授权后重启本应用生效；设置页「权限检测」可随时验证

> webui 模式：由启动服务的终端进程权限决定；若终端已授权完全磁盘访问则无需额外操作。

## 许可

[MIT](./LICENSE)
