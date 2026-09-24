<p align="center">
  <img src="src-tauri/icons/icon.png" alt="Agent Companion 图标" width="128" />
</p>

<h1 align="center">Agent Companion · Agent 小伴</h1>

<p align="center">让正在工作的 AI Agent，成为桌面上看得见的小伙伴。</p>

Agent 小伴是一款桌面悬浮会话助手。把 Codex、WorkBuddy、CodeBuddy 和 Codeg 的任务状态集中到一条悬浮栏里：看一眼就知道谁还在工作、谁需要你确认，点击即可回到支持跳转的原会话。

适合同时运行多个 Agent、在不同项目之间切换，又不想反复打开窗口检查进度的人。

## 快速开始

### 下载与安装

发布后，在本项目 GitHub 仓库的 **Releases** 页面下载对应系统和芯片架构的安装包。

> 安装包尚待发布，下载链接将在首次发布后补充。目前仅完成 macOS 验证，Windows / Linux 尚未验证；下表是对应格式发布后的安装指引，实际可用平台以 Release 附件为准。

| 平台 | 选择的安装包 | 安装方式 |
| --- | --- | --- |
| macOS Apple Silicon（M 系列） | `aarch64` / `arm64` 版本 | 若为 `.dmg`，打开后将 Agent Companion 拖入「应用程序」；若为压缩包，解压后将 `.app` 移入「应用程序」 |
| macOS Intel | `x86_64` / `x64` 版本 | 同上，注意选择 Intel 版本 |
| Windows x64 | `.exe` 安装程序 / `.msi` | 下载后双击，按安装向导完成安装 |
| Linux x64 | `.deb` / `.AppImage` | Debian / Ubuntu 使用软件安装器打开 `.deb`；AppImage 在文件属性中允许执行后运行 |

macOS 要求 **12 或更高版本**。首次打开遇到系统拦截，请参阅下方 [macOS 注意事项](#macos-注意事项)。

### 开始使用

1. **打开小伴**：启动 Agent Companion，桌面上会出现悬浮栏。
2. **接入 Agent**：从托盘或悬浮栏右键菜单进入设置，选择需要监听的 Codex、WorkBuddy、CodeBuddy 或 Codeg。接入依赖对应客户端的 Hooks / Webhook。
3. **开始任务**：在对应客户端发起或继续会话，收到新的事件后，悬浮栏会显示会话伙伴。
4. **查看进度**：悬停查看任务信息；需要确认时会显示提示卡，有跳转入口时点击即可返回原会话。

可以拖动悬浮栏调整位置，也可以在设置中切换伙伴风格、显示数量和动画。

## 功能

| 功能 | 说明 |
| --- | --- |
| 多 Agent 会话 | 集中展示运行中、待确认、已完成等状态，并标识会话来源 |
| 桌面伙伴 | 10 种小动物与 3 种几何伙伴造型，可切换风格、调整显示数量与动画 |
| 提问提醒 | 需要你确认时展示提示卡；可以关闭本轮提示 |
| 安静完成 | 完成任务用小标记提示，不自动弹出完成卡片 |
| 会话跳转 | 有可用跳转链接时，点击头像或卡片回到原会话 |
| 悬浮交互 | 支持拖动、悬停查看详情、右键菜单，透明区域可点击穿透 |
| 本机设置 | 按来源开关监听，调整外观与开机启动；通过托盘管理应用 |
| 原生监控 | Rust 本地服务接收 Hooks / Webhook；打包后的应用无需 Node.js 或 Python |

## macOS 注意事项

### 首次启动提示无法验证开发者

当前本地构建未进行开发者签名或公证。如果 macOS 阻止打开，确认应用来自可信来源后：

1. 尝试打开一次 Agent Companion。
2. 进入「系统设置 → 隐私与安全性」。
3. 找到该应用的拦截提示，点击「仍要打开」，按系统提示确认。

具体操作见 [Apple 官方说明](https://support.apple.com/zh-cn/102445)。

### 没有显示会话

- 确认对应 Agent 客户端正在运行，且已启用该来源的监听。
- 检查 Hooks / Webhook 是否成功接入；仅开启监听开关不代表接入成功。
- 发起一次新任务或继续现有对话。小伴根据新事件更新状态，不会将全部历史会话恢复到悬浮栏。
- CodeBuddy 接入面向 IDE 客户端，不能当作独立 CodeBuddy CLI 的接入使用。

### 同时运行旧版 Agent Office

如果桌面出现两套悬浮栏，可以隐藏旧应用的悬浮栏或退出旧应用。两者可能共享本机监控服务，修改监听来源会影响共享服务的其他宿主。

## 从源码运行

需要 Node.js 22.13+、Rust stable 和对应平台的编译工具链。macOS 可通过 `xcode-select --install` 安装命令行编译工具。

```sh
npm ci
npm run dev             # 启动桌面开发模式
npm run desktop:build   # 当前配置构建 macOS .app
```

构建产物位于 `src-tauri/target/release/bundle/macos/Agent Companion.app`。更多内容见 [开发文档](docs/development.md) 与 [验证记录](docs/validation.md)。

## 许可

项目许可证待补充。已有版权与许可证声明继续保留；Agent 图标归各自权利人所有，详见 [图标来源说明](public/icons/agents/README.md)。
