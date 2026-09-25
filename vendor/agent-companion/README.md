<p align="center">
  <img src="assets/branding/transparent-source.png" alt="Agent Companion 图标" width="128" />
</p>

<h1 align="center">Agent Companion</h1>

<p align="center">在桌面上看见每个 AI Agent 的工作状态。</p>

Agent Companion 是一款桌面悬浮会话助手。把 Codex、WorkBuddy、CodeBuddy 和 Codeg 的任务状态集中到一条悬浮栏里：看一眼就知道谁还在工作、谁需要你确认，点击即可回到支持跳转的原会话。

适合同时运行多个 Agent、在不同项目之间切换，又不想反复打开窗口检查进度的人。

### 在线演示

[打开 GitHub Pages 在线演示](https://changexbc.github.io/agent-companion/)（只读演示；会话、项目与状态均为虚构数据，不连接 Hook / Webhook，也不打开原会话。）

## 快速开始

### 下载与安装

在 [Releases](https://github.com/changexbc/agent-companion/releases/latest) 页面下载对应平台与芯片架构的安装包（当前版本 **0.1.0**）。下表的链接始终指向最新发布版本：

| 平台 | 下载 | 安装方式 |
| --- | --- | --- |
| macOS Apple Silicon（M 系列） | [Agent-Companion_mac-arm64.zip](https://github.com/changexbc/agent-companion/releases/latest/download/Agent-Companion_mac-arm64.zip) | 解压后把 `Agent Companion.app` 拖入「应用程序」 |
| macOS Intel | [Agent-Companion_mac-x64.zip](https://github.com/changexbc/agent-companion/releases/latest/download/Agent-Companion_mac-x64.zip) | 同上，注意选择 Intel 版本 |
| Windows x64 | [Agent-Companion_windows_x86_64-setup.exe](https://github.com/changexbc/agent-companion/releases/latest/download/Agent-Companion_windows_x86_64-setup.exe) | 下载后双击，按安装向导完成安装 |
| Linux x64 | [Agent-Companion_linux_x86_64.AppImage](https://github.com/changexbc/agent-companion/releases/latest/download/Agent-Companion_linux_x86_64.AppImage) | 在文件属性中允许执行后直接运行；`.deb` 见 Releases 页（文件名带版本号），用软件安装器打开 |

macOS 要求 **12 或更高版本**。首次打开遇到系统拦截，请参阅下方 [macOS 注意事项](#macos-注意事项)。Windows / Linux 尚未完成完整实机验收，遇到的问题欢迎在 Issues 反馈。

### 应用内更新

安装**首个带更新器的版本**之后，后续版本可以在应用内升级：

1. 独立版启动 15 秒后自动检查一次，之后每 30 分钟检查公开发布的最新稳定版；也可以在设置 →「更新」中手动检查。
2. 发现新版本后点「下载更新」，进度同时显示在设置页和托盘菜单；下载完成后点「重启并安装」，重启即完成升级。
3. 更新包只有在通过签名校验后才会进入安装阶段；发现新版本不会自动下载、自动安装或自动重启。
4. 需要显式代理时，在「更新代理」填写 HTTP/HTTPS 地址（例如 `http://127.0.0.1:7897`）并保存；留空表示不使用显式代理。该设置只作用于更新检查与安装包下载。

`0.1.0` 及更早版本没有内置更新器，需要手动安装一次带更新器的版本，此后才能在应用内升级。macOS 包仍未做开发者签名与公证，更新签名不改变 Gatekeeper 的首次启动提示。嵌入其他 Tauri 宿主时由宿主负责自身更新，Agent Companion 的设置页不会触发独立版的更新流程。

### 开始使用

1. **启动 Agent Companion**：桌面上会出现悬浮栏。
2. **接入 Agent**：从托盘进入设置，选择需要监听的 Codex、WorkBuddy、CodeBuddy 或 Codeg。接入依赖对应客户端的 Hooks / Webhook。
3. **开始任务**：在对应客户端发起或继续会话，收到新的事件后，悬浮栏会显示会话伙伴。
4. **查看进度**：悬停查看任务信息；需要确认时会显示提示卡，有跳转入口时点击即可返回原会话。

可以拖动悬浮栏调整位置，也可以在设置中切换伙伴风格、显示数量和动画。

## 功能

| 功能 | 说明 |
| --- | --- |
| 多 Agent 会话 | 集中展示运行中、待确认、已完成等状态，并标识会话来源 |
| 桌面伙伴 | 10 种小动物与 3 种几何伙伴造型，可切换风格、调整显示数量与动画 |
| 提问提醒 | 需要你确认时展示提示卡；可以关闭本轮提示 |
| 完成提醒 | 任务完成时自动弹出卡片，并在头像上显示完成标记 |
| 会话跳转 | 有可用跳转链接时，点击头像或卡片回到原会话 |
| 悬浮交互 | 支持拖动、悬停查看详情；右键任意任务头像可关闭该任务的本次监听，透明区域可点击穿透 |
| 本机设置 | 按来源开关监听，调整外观与开机启动；通过托盘管理应用 |
| 应用内更新 | 设置页检查、下载并重启安装签名更新包，可为更新单独配置 HTTP/HTTPS 代理 |
| 原生监控 | Rust 本地服务接收 Hooks / Webhook；打包后的应用无需 Node.js 或 Python |

## 界面预览

网页演示里的悬浮栏：Codex 会话已完成、WorkBuddy 会话运行失败，两种状态都会自动弹出信息卡，头像上分别是绿色对勾与红色叉号（截图取自[在线演示](https://changexbc.github.io/agent-companion/)，数据全部为虚构）。

![网页演示的悬浮栏：一个已完成的 Codex 会话与一个失败的 WorkBuddy 会话，各自弹出信息卡](docs/images/demo-rail-states.png)

## Agent 支持范围

以下是四个内置来源在当前 macOS 版本中的能力。✅ 表示支持，— 表示不支持或不使用该接入方式；CodeBuddy IDE 与 VS Code 插件共用一个来源，但跳转目标不同。

| Agent / 客户端 | Hooks | Webhook | 运行、待确认、结束状态 | 跳到指定会话 | 点击后的实际行为 |
| --- | :---: | :---: | :---: | :---: | --- |
| Codex（Desktop／CLI） | ✅ | — | ✅ | ✅ | 打开 Codex Desktop 中的指定任务；CLI 会话也跳转到 Desktop |
| WorkBuddy（国内版／国际版） | ✅ | — | ✅ | ✅ | 打开对应版本中的指定对话 |
| CodeBuddy IDE（国内版／国际版） | ✅ | — | ✅ | — | 有工程路径时打开工程，否则只唤起 CodeBuddy |
| CodeBuddy VS Code 插件 | ✅ | — | ✅ | — | 尝试打开该会话所属的 VS Code 工程；无法确定工程时只唤起 VS Code |
| Codeg | — | ✅ | ✅ | ✅ | 打开 Codeg 中的指定聊天会话 |

以上状态由 Hook / Webhook 事件更新，悬浮栏不会导入全部历史会话。原生版会恢复此前已跟踪的 Codex 任务，并补齐监听服务关闭期间收到的结束事件；完成任务仍按悬浮栏的保留时间退出。跳转还要求目标客户端已安装且系统能够打开对应链接。CodeBuddy 接入面向 IDE 与 VS Code 插件，不包含独立 CodeBuddy CLI。

**Codex 监听保持轻量：**任务的运行、等待和结束状态只由 Hook 更新，不读取 Codex 的任务数据库、会话文件或 transcript。重启恢复使用 Agent Companion 自己保存的少量已跟踪状态（最多 128 个会话、512 KiB）。如果结束 Hook 完全没有触发或送达，悬浮栏无法可靠判定任务已完成；可右键对应头像选择“关闭本次监听”，下一次收到该任务的活动 Hook 时会重新显示。独立的“已读后收起”功能只在有待处理的已完成任务时检查 Codex 的 `.codex-global-state.json`，最多每分钟一次、文件上限 8 MiB；文件未变化时不读取正文，它不用于判断任务是否结束。

Codeg 内运行的 Codex、Grok 等 Agent 会显示内部 Agent 标识，但接入来源仍是 Codeg，点击进入的是 Codeg 会话。Codeg 接入前需在客户端启用 Web Service；其他来源也需成功安装对应 Hook，仅打开监听开关不会产生会话。

**Codex 已知限制：**权限请求 Hook 会在自动审批前触发，不能据此确定真的需要人工操作。悬浮栏会先显示“权限检查中”；检查持续较久时提醒你查看 Codex。同步提问可以识别为“待确认”，但目前只显示通用提示，不能展示完整问题内容。

## macOS 注意事项

### 首次启动提示无法验证开发者

发布包未做开发者签名与公证。macOS 首次启动若提示无法验证开发者，先在 Finder 中按住 Control 点击应用并选择「打开」，或前往「系统设置 → 隐私与安全性」选择「仍要打开」。

**仅当安装包来自本仓库官方 Releases、且系统仍提示「已损坏」时**，再执行下面这条命令移除隔离标记（其余情况不要执行）：

```bash
xattr -rd com.apple.quarantine "/Applications/Agent Companion.app"
```

具体操作见 [Apple 官方说明](https://support.apple.com/zh-cn/102445)。通过应用内更新安装的版本不带隔离标记，不会触发该提示。

### 没有显示会话

- 确认对应 Agent 客户端正在运行，且已启用该来源的监听。
- 检查 Hooks / Webhook 是否成功接入；仅开启监听开关不代表接入成功。
- Codex CLI 如果在安装或修复 Hooks 后要求重新信任，请确认 Agent Companion 安装的 Hook 命令；未信任时 CLI 不会把事件送到悬浮栏。
- 发起一次新任务或继续现有对话。悬浮栏根据新事件更新状态，不会恢复全部历史会话。
- CodeBuddy 接入面向 CodeBuddy IDE 与 VS Code 插件，不能当作独立 CodeBuddy CLI 的接入使用。

## 从源码运行

需要 Node.js 22.13+、Rust stable 和对应平台的编译工具链。macOS 可通过 `xcode-select --install` 安装命令行编译工具。

```sh
npm ci
npm run dev             # 启动桌面开发模式
npm run desktop:build   # 当前配置构建 macOS .app
npm run demo:dev        # 开发网页演示（虚构数据，端口 4190）
```

构建产物位于 `src-tauri/target/release/bundle/macos/Agent Companion.app`。本地构建不生成更新产物（发布构建由 CI 注入签名公钥），因此开发构建的设置页只提示未配置更新签名公钥，不会误报可更新。更多内容见 [开发文档](docs/development.md)、[构建与发布](docs/ci-build.md) 与 [验证记录](docs/validation.md)。

## 致谢

感谢 [Linux.do](https://linux.do) 社区。

## 许可

[MIT](./LICENSE)
