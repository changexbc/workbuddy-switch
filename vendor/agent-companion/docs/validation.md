# 迁移验证记录

验证环境：macOS，本地独立新仓库；未使用旧仓库的 node_modules、target 或路径依赖。

## 已通过

- Node 回归：85 项通过；默认跳过的 Codeg 原生测试在指定新构建二进制后补跑通过（Codeg 文件全部 11 项通过）。
- Rust workspace：35 项测试通过。
- Rust/Node 快照一致性：轮次、提问、完成、旧事件与 token 状态一致。
- 原生服务：隔离用户目录中的 Codex / IDE Hooks、禁用接入、双客户端、服务锁、通知 owner 交接及 RPC 鉴权通过。
- 前端构建与资源检查：两个入口，约 139 KB，无 Three.js、办公室模型、光照贴图或旧仓库路径依赖。
- 浏览器实际页面：空闲、运行、待确认、完成状态；提问自动提醒；完成不弹提醒；设置保存恢复；保留共享配置字段；无页面错误与失败资源请求。
- macOS 打包：独立 Agent Companion.app，约 17 MB。
- 原生诊断构建：隔离目录下收到 Hook 后显示 1 个待确认会话；设置加载成功；仅有 rail / settings 两个窗口，打开 office 返回“未知视图”。
- 来源保护：已迁移源文件的 SHA-256 与复制前一致，来源仓库 Git 状态也未改变。

## 证据

- `artifacts/ui/`：浏览器页面截图与检查报告。
- `artifacts/native-*/`：原生 WebView 截图、窗口列表与检查报告；以含有 `summary.json` 的成功运行目录为准。
- `docs/migration-source.json`：来源基线及文件哈希。

上述 artifacts 是本机可重建的验证产物，不进入 Git。

## 验证边界

- 原生 Codex 适配器沿用通用“需要你确认”文案，本次未扩展提问文本解析。浏览器完整提问文案检查使用模拟快照。
- 原生应用验证使用真实 Rust Hook 通道和隔离会话，未代替用户对日常真实会话、实际跳转、通知授权与开机启动的验收。
- 未验证 Windows/Linux，未签名公证、安装、发布或修改旧办公室/WB Switch 的集成。

---

# 前端迁移验证记录（React + shadcn/ui + TypeScript）

把两个窗口的前端从手写 DOM 代码迁移到 React + TypeScript；设置页同时引入 shadcn/ui 与 Tailwind。分四个阶段完成，提交 `3eff739`（工具链与类型）、`b05404a`（设置页）、`ac64a21`（悬浮栏）、`8c38620`（按独立复查修正）。

## 已通过（自动化）

| 验收矩阵行 | 命令 | 结果 |
| --- | --- | --- |
| 工具链 | `npm run typecheck`、`npm run lint`、`npm test` | 通过。共享 JS 用 `// @ts-check` 逐个开启检查 |
| 模型 | `npm test` | 95 项，94 通过 / 0 失败 / 1 跳过 |
| 问题卡 | `npm run test:ui`、`tests/reminders.test.js` | 通过。wait 自动卡、done 安静、静音按轮次与问题清除 |
| 桥接生命周期 | `tests/bridge.test.js` | 通过。活动订阅数归零、迟到的 listen 被立即释放、乱序初始响应不覆盖新快照 |
| 定时清理 | `npm test`、`npm run test:ui` | 通过。模型使用可控时钟；销毁后不再响应事件也不再与宿主通信 |
| 设置 | `npm run test:ui` | 通过。读失败重试、部分保存、双击保存、保留 `source.path` 与 `scene`、跨窗口同步 |
| 可访问性 | `npm run test:ui`、`focus-diff.mjs` | 通过。设置页 9 个 Tab 停靠点焦点环一致；悬浮栏 Escape 关闭并归还焦点 |
| 集成 | `npm run test:rust` / `test:runtime` / `test:native-app` / `test:bundle` | 通过。打包产物 422492 字节，无 3D 资源 |
| 视觉（设置页） | `style-diff.mjs` + `pixdiff` | 与迁移前逐像素一致（480x937）；属性对照 29 条，全部为按需挂载的结构性差异 |
| 视觉（悬浮栏） | `rail-diff.mjs` + `rail-pixels.mjs` | 11 个场景。语义状态差异 0；属性差异 20 条，全部是同一个结构性事实；像素对照多数逐像素一致 |

一次完整运行及逐条日志见 `artifacts/frontend-migration/after/acceptance.json` 与 `after/logs/`。

## 迁移后新增的检查手段

- `artifacts/frontend-migration/rail-diff.mjs`：从 `80d6c55` 逐字节取出的迁移前悬浮栏，与迁移后逐元素对照计算样式、几何，加一个语义探针。先做过自检（同一份代码对自己跑出 11 场景 0 差异）。
- `focus-diff.mjs`：伪类样式在默认态的计算样式里看不见，所以焦点环必须单独探。
- `artifacts/frontend-migration/{rail-pixels,measure-rail-startup,measure-idle}.mjs`：像素、启动、空闲资源。

## 已知的测量限制

- **悬浮栏的 1x 像素对照不是确定性的。** Chromium 对这个 app 的 SVG 在每次冷载入时可能选不同的栅格化路径，差异是抗锯齿边缘上 15–20 个像素；8 倍放大下同一区域逐字节相同，矢量几何没有变化。实测**迁移前**的构建产物自身在 12 次冷载入里就产生两种渲染（10 和 2），所以这不是迁移引入的。因此像素关卡允许「至多 64 像素且落在 32x32 的框内」，并把数量、盒子和两侧渲染种类数打印出来。**盲区**：约 16 像素、局限在 SVG 边缘的改动会被它放过，只能靠确定性的属性对照兜住——属性清单已包含 `fill`/`stroke`/`filter` 等 SVG 呈现属性（用 1/255 的填充改动验证过会被抓到）。
- **冷启动到「悬浮栏可交互」的原生耗时未测量**，没有埋点。同机生产包对照（Chromium 代理）：首帧中位 14 → 27 ms，首屏传输 61.0 → 289.1 KB 未压缩。
- **空闲 CPU / 内存：测到了，但不足以断言没有回归。** 迁移前、迁移后、以及 2026-09-22 的一次重跑（3 次 × 60 秒，机载 4.39）三次会话的数据分别在 `after/idle-resources.json` 与 `after/idle-resources-rerun.json`。app 进程 CPU 三次会话都落在 2.0–2.9%，没有变化；WebContent 进程一致偏高：CPU 约 2.1% → 3.1%，RSS 32–39 MB → 46–52 MB，与悬浮栏窗口的载荷从约 43 KB 涨到约 271 KB 相符。因为三次会话的机载不可比，这里记录为**观察到的代价**，不是回归判定。
- **`cargo test --workspace` 偶发失败，先于本次前端迁移**：约 8 次里失败 2 次，都在 `crates/agent-studio-core/tests/lifecycle.rs`（第 455 行取不到刚 ingest 的会话；第 23 行 `atomic_json` 报 ENOENT）。`git diff 3eff739~1..HEAD -- crates/` 为空，所以与本任务无关。两次失败都发生在另一个子代理正在本机跑浏览器自动化与变异测试时；清掉它残留的后台服务后连续 13 次通过。最可能是资源竞争，但没有构造出可控复现，建议单独排查而不是重试掩盖。
- 悬浮栏的窗口加载量从约 43 KB 涨到约 271 KB（React 变成两个窗口共享的 chunk）；总 JS 315.5 → 320.7 KB。悬浮栏 CSS 逐字节不变。

## 原生交互已手工验证（2026-09-22）

拖动 grip、点击穿透、非激活悬停、右键菜单四项已在真实 app 中跑完，4/4 通过，原始数据在 `after/native-interaction.json`（用隔离的 `AGENT_STUDIO_HOME` 与会话 fixture，未触碰真实数据）。要点：窗口按拖动增量移动且尺寸不变（−300/+60）；失焦状态下移动真实鼠标出头像卡片、光标变手型；透明区穿透到后面的 Finder，而同排的区域点被消费，被消费的范围与绘制范围 + 10px 外扩吻合；点「悬浮窗设置」打开设置窗口。

## 仍待人工确认

- **用户对迁移后外观的确认**：逐张看 `artifacts/ui/*.png` 与 `artifacts/frontend-migration/rail-shots-built/*-after*.png`。
- **Portal 挂载的表面被测量**：悬浮栏当前没有 Portal，没有可运行的反例。注册表是显式注册而不是子树遍历，这是它在结构上可行的原因；引入第一个 Portal 弹层时补上断言。
- **冷启动到「悬浮栏可交互」的原生耗时**：没有埋点。
- **屏幕阅读器能否读到悬浮栏**：AX 读取对两个窗口都拿不到任何 web 内容（悬浮栏窗口 1 个元素，设置窗口 145 个但全是窗口按钮与应用菜单栏），因此无法区分「这个 app 整体没有暴露 web 内容」与「这种读法看不到 WebKit 的 web area」。需要真实 VoiceOver 走查。
- **Windows / Linux、签名、公证、发布**：不在本次范围。

## 两项先于本次迁移的既有行为

1. **悬浮栏在独立 app 里是纯鼠标的。** 窗口以 `.focusable(false)` 创建（`crates/agent-studio-desktop/src/lib.rs:395`），键盘事件到不了它的 WebView：桌面自动化的前台按键投递直接报「窗口无法获得焦点」，后台 Escape 也关不掉菜单。旧实现绑的是同一套键盘处理，且 `crates/` 全程逐字节未变。菜单的方向键代码只在浏览器套件里执行得到；嵌入到可聚焦的宿主窗口时是否可达，未验证。
2. **指针在两个区域之间移动不产生事件。** `hit_test.rs::report` 只在指针进出所有区域的并集时回调，所以从一个区域移到另一个区域不触发任何 JS 回调，上一个头像的 `.native-hover` 与它的卡片会留着。旧处理器结构相同。

自动化测试通过、原生诊断通过、用户视觉接受是三件不同的事；本文件只覆盖前两者。
