# 开发结构与约定

本页记录前端维护约定；安装与运行方式见 [README](../README.md)。

供其他 Tauri 应用打包时，运行 `npm run build:embed`，将完整的 `dist-embed/` 内容复制到宿主资源目录（例如 `dist/companion/`），窗口 URL 使用 `companion/desktop.html` 与 `companion/desktop-settings.html`。此构建使用相对资源基准；独立版继续使用 `npm run build` 输出根路径资源到 `dist/`。宿主应让插件的 `Config.assets` 指向相同目录，并从同一源码修订构建插件与运行时。

## 前端结构

两个窗口是两个独立的 Vite 入口，各自拥有一个 React root；跨窗口状态继续走原生事件与存储，没有共享的 Context。

- `src/desktop/rail.tsx` / `settings.tsx`：入口，只负责挂载。
- `src/desktop/components/`：视图。`SettingsForm` 是设置页；`Rail`／`SessionAvatar`／`SessionCard` 是悬浮栏。
- `src/desktop/rail-controller.ts`：悬浮栏唯一的状态所有者，持有模型、静音集合、菜单、提示和欢迎动画。
- `src/desktop/rail-animations.ts`：所有 Web Animations 调用。
- `src/desktop/hit-regions.ts`：点击穿透用的显式表面注册表。
- `src/components/ui/`：按需引入的 shadcn 源码，目前只有设置页在用。

三条约定，违反它们会让「React 与控制器同时写同一个 DOM 属性」这类问题重新出现：

1. `#desktop-rail` 是 React 的**容器**而不是 React 元素，所以 React 不写它的属性；`desktop-inactive`、`companion-motion-paused`、`welcome-blocking` 等由控制器写。
2. 悬浮卡、自动问题卡、提示条是**按布局放置**的（是否可见取决于头像列表滚到哪里），它们的 `hidden` 与 `style.top` 属于控制器，JSX 里不为它们声明 `style` 或 `hidden`。
3. 动画只在 `afterCommit()` 里启动，它跑在每次提交后的 layout effect 中，用「上一次提交结束时测量到的位置」作为起点。FLIP 基线用 `offsetTop` 而不是 rect：rect 会把正在跑的动画的 transform 算进去。

悬浮栏不引入 Tailwind：那里没有 shadcn 组件，而 preflight 会覆盖旧样式表从未声明过的 UA 默认值，设置页迁移时就因此出过四个回归。

### Node 共享的 JS 例外

以下文件保持 JS 形式，因为 collector 直接用 Node 加载它们，不能要求转译：

`src/settings-config.js`、`src/monitor/model.js`、`src/monitor/session-visibility.js`（及其 `codex-internal-prompts.json`）。

它们都带 `// @ts-check` 与 JSDoc 类型，所以 `npm run typecheck` 会检查它们——`checkJs` 是关闭的，靠 pragma 逐个开启，避免把 `scripts/`、`tests/` 和 `collector/` 一起拖进来。

## 目录

- `src/desktop/`：悬浮框、SVG 头像、欢迎动画和设置（React + TypeScript）。
- `src/monitor/`：会话生命周期、展示、提醒和跳转；其中 `model.js` 与 `session-visibility.js` 是被 collector 共享的 JS。
- `crates/agent-studio-core/`：监控与状态归一化。
- `crates/agent-studio-runtime/`：本地共享监控服务及客户端。
- `crates/agent-studio-desktop/`：Tauri 桌面集成，仅提供会话栏和设置窗口。
- `src-tauri/`：Agent Companion 独立应用壳。应用内更新（`update_service.rs`、托盘更新项、更新命令）只属于这一层：插件被嵌入其他宿主时由宿主负责自身升级，所以共享插件不包含更新器。设置页的更新区读 `src/types/update.ts` 的契约，并在没有独立版命令时整块隐藏。

本地 `npm run desktop:build` 通过 `--config` 关闭 `bundle.createUpdaterArtifacts`：没有签名密钥也能打包试装，开发构建不会误报可更新。发布构建由 CI 注入公钥并签名，见 [构建与发布](ci-build.md)。
