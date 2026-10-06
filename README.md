# workbuddy-switch

WorkBuddy、CodeBuddy IDE、CodeBuddy CLI 与 VS Code CodeBuddy 插件账号切换桌面 App（Tauri），四者均支持国内版 / 国际版，并提供积分到期与 Token 用量监控。

<p align="center">
  <img src="public/icon-transparent.png" alt="WorkBuddy Switch 图标" width="128" />
</p>

多账号共享登录态，一键切换 WorkBuddy 登录账号。**会话复制**：把当前账号的会话以新 id 复制给目标账号，源账号数据不受影响，云端归属目标账号。**关联会话**：复制过的会话会自动建立跨账号关联，集中查看各账号副本状态，并把新增内容同步过去。

**在线演示**：[打开 GitHub Pages 演示](https://changexbc.github.io/workbuddy-switch/)（只读演示；账号、积分与请求记录均为虚构数据，所有业务操作均已禁用，另含只读的会话悬浮栏演示）

## 快速开始

前往 [GitHub Releases](https://github.com/changexbc/workbuddy-switch/releases/latest) 下载对应平台的安装包：

| 平台 | 安装包 | 安装方式 |
| --- | --- | --- |
| macOS Apple Silicon（M 系列，arm64） | `workbuddy-switch_<版本>_aarch64.dmg` | 打开 DMG，将 `workbuddy-switch.app` 拖入「应用程序」 |
| macOS Intel（x86_64） | `workbuddy-switch_<版本>_x86_64.dmg` | 打开 DMG，将 `workbuddy-switch.app` 拖入「应用程序」 |
| Windows x64 | `workbuddy-switch_<版本>_x64-setup.exe` | 运行安装程序并按提示完成安装 |
| Linux x64 | `workbuddy-switch_<版本>_amd64.deb` / `workbuddy-switch_<版本>_amd64.AppImage` | Debian/Ubuntu 安装 `.deb`；其他发行版可给 AppImage 添加执行权限后直接运行 |

macOS 首次启动若提示无法验证开发者，先在 Finder 中按住 Control 点击应用并选择「打开」，或前往「系统设置 → 隐私与安全性」选择「仍要打开」。仅当安装包来自上述官方 Releases、且系统仍提示「已损坏」时，再执行：

```bash
xattr -rd com.apple.quarantine "/Applications/workbuddy-switch.app"
```

应用能启动但切换账号时提示无权限，请参阅下方 [macOS 权限说明](#macos-权限说明)。

另有 npm / webui 版本可在浏览器中使用，见文末 [npm / webui 版本](#npm--webui-版本)。

## 功能

| 模块 | 说明 |
| --- | --- |
| 账号管理 | OAuth 扫码登录、导入导出账号、删除账号 |
| 账号切换 | 一键切换 WorkBuddy 登录账号，切换过程实时显示进度 |
| 会话复制 | 把当前账号勾选的会话复制给目标账号，源账号数据不受影响 |
| 关联会话 | 复制过的会话自动建立跨账号关联；按客户端集中查看同一会话在各账号中的副本状态，支持增量同步、分歧处理与新增 / 解除关联 |
| 积分到期查询 | 自动查询每个账号的积分剩余量与到期时间；7 天内到期高亮，并按紧迫程度排序、标注「建议优先使用」 |
| 积分统计 | 汇总官方请求用量：总览、近 30 天趋势、模型分类、账号消耗与请求明细 |
| Token 统计 | 按来源查看 Token 总览与趋势，含构成占比、活跃热力图、项目/模型 Top 10 与会话排行 |
| CodeBuddy CLI | 与 WorkBuddy 复用同一账号库，默认账号独立；切换后立即生效，无需重启 CLI |
| CodeBuddy IDE | 支持切换 CodeBuddy IDE 桌面客户端账号，并可在弹窗中勾选复制会话，与 CodeBuddy CLI 相互独立 |
| VS Code CodeBuddy 插件 | 支持切换 VS Code 内的 CodeBuddy 插件账号；VS Code 运行时可自动关闭并在写入后重新打开 |
| JetBrains IDE 插件 | 支持切换 IntelliJ IDEA / PyCharm 内的 CodeBuddy 插件账号，一次切换写入所有装了插件的 IDE；IDE 运行时可自动关闭并在写入后重新打开 |
| 插件会话复制 | 切换插件账号时，可把当前插件账号的会话复制给目标账号（加法，源账号不变） |
| 自动轮换 | 后台把积分最紧迫的账号设为 CodeBuddy CLI 后续启动账号；检测到 CLI 会话运行时会跳过 |
| 自动更新 | 从 GitHub Releases 检查新版本，整包更新经签名校验 |
| 会话悬浮窗 | 桌面版内置 Agent Companion 悬浮栏，在桌面集中显示 Codex / WorkBuddy / CodeBuddy / Codeg 会话的运行中 / 待确认 / 已完成状态；悬停查看详情，支持跳转时点击回到原会话，托盘可临时隐藏 |
| 权限检测 | macOS 授权引导（App 管理 / 完全磁盘访问拖拽授权 + 自动检测） |

## 支持的工具

| 工具 | 账号切换 | 会话复制 | 自动关闭重开 | 自动轮换 | 悬浮窗监听 |
| --- | :---: | :---: | :---: | :---: | :---: |
| WorkBuddy | ✅ | ✅ | ✅ | — | ✅ |
| CodeBuddy IDE | ✅ | ✅ | ✅ | — | ✅ |
| CodeBuddy CLI | ✅ | — | — | ✅ | — |
| VS Code CodeBuddy 插件 | ✅ | ✅ | ✅ | — | ✅ |
| JetBrains IDE 插件（IDEA / PyCharm） | ✅ | — | ✅ | — | — |

✅ 表示支持，— 表示不支持。设置 →「支持工具」可按客户端逐个开启 / 关闭入口；关闭后该端入口与状态轮询一并隐藏，不影响账号库与其它端；JetBrains 端默认关闭，可在设置中随时打开。

CodeBuddy CLI 切换时会先关闭正在运行的 CLI，当前会话会中断且不会自动重开；其余各端可在客户端运行时自动完成切换。

### 会话悬浮窗（Agent Companion）

桌面版内置 [Agent Companion](https://github.com/changexbc/agent-companion) 悬浮栏：把各 AI Agent 的任务状态集中到桌面，一眼看出谁还在运行、谁需要你确认，支持跳转时点击即可回到原会话；悬浮栏可拖动调整位置，托盘可随时显示 / 隐藏。

| 监听来源 | 跳转到指定会话 | 点击后的行为 |
| --- | :---: | --- |
| Codex（Desktop / CLI） | ✅ | 打开 Codex Desktop 中的指定任务 |
| WorkBuddy（国内版 / 国际版） | ✅ | 打开对应版本中的指定对话 |
| CodeBuddy IDE（国内版 / 国际版） | — | 有工程路径时打开工程，否则只唤起 CodeBuddy |
| CodeBuddy VS Code 插件 | — | 尝试打开会话所属的 VS Code 工程，无法确定时只唤起 VS Code |
| Codeg | ✅ | 打开 Codeg 中的指定聊天会话 |

各来源都会显示运行中 / 待确认 / 已完成状态；CodeBuddy CLI 与 JetBrains 插件不在监听范围内。开启方式：左下角悬浮窗图标，或设置 → Agent Companion；首次使用在「悬浮窗设置」中完成接入（依赖对应客户端的 Hooks / Webhook），监听来源与外观样式也在那里调整。

网页演示里的悬浮栏：一个已完成的 Codex 会话与一个失败的 WorkBuddy 会话，各自弹出信息卡（截自[在线演示](https://changexbc.github.io/agent-companion/)，数据为虚构）。

![Agent Companion 悬浮栏演示：已完成与失败两种状态各自弹出信息卡](docs/images/agent-companion-demo-rail.png)

更多说明与独立版见 [Agent Companion 仓库](https://github.com/changexbc/agent-companion) · [在线演示](https://changexbc.github.io/agent-companion/)

## 使用

1. **添加与导出账号**：账号页 →「OAuth 扫码登录」「导入备份」；「导出」可将勾选账号备份为 JSON
2. **切换账号与账号信息**：账号卡片 →「切换」，可勾选复制当前会话；「账号信息」可给账号添加备注，并选择卡片上显示账号名 / 手机号 / 备注
3. **查看积分与统计**：账号页自动查询各账号积分到期情况，点「刷新积分」手动更新；侧栏进入「积分统计」「Token 统计」查看用量明细
4. **切换各客户端账号**：CodeBuddy CLI、CodeBuddy IDE、VS Code CodeBuddy 插件均可在账号卡片一键切换；CodeBuddy IDE 与 VS Code 插件支持在弹窗中勾选复制当前账号的会话。CodeBuddy IDE 首次使用前需先手动打开并登录一次
5. **管理关联会话**：侧栏「关联会话」按客户端查看同一会话在各账号中的副本状态，把新增内容增量同步到目标账号、处理内容分歧；「新增关联会话」可把会话复制到新账号并建立关联
6. **开关各端入口**：设置 →「支持工具」可按客户端逐个开启 / 关闭入口；关闭后该端在账号页隐藏、不再轮询状态，不影响账号库。JetBrains 端默认关闭
7. **自动轮换**：设置 → CodeBuddy CLI 自动轮换，开启后按积分紧迫程度自动设置默认账号
8. **更新**：应用会自动检查公开 GitHub Releases；发现新版本后可在左下角直接升级，也可从设置页打开 Release 页面手动下载

## 界面预览

### 管理 WorkBuddy 与 CodeBuddy 账号

账号卡片集中展示登录状态、积分余额和到期资源，临期积分直接标注在对应卡片内，并按紧迫程度优先排列。

![账号管理页面（账号信息已脱敏）](docs/images/accounts-overview.png)

### 关联会话

复制到其他账号的会话会自动建立关联：按客户端（WorkBuddy / CodeBuddy IDE / CodeBuddy 插件）集中展示同一会话在各账号中的副本状态，可把来源账号的新增内容增量同步到目标账号；出现内容分歧时选择要保留的一份，也可把会话复制到新账号并建立关联。

![关联会话页面](docs/images/session-links.png)

打开任一关联组可查看「会话关联图」：同一会话在各账号中的副本以分支图呈现，内容分歧时标出共同旧版与各自的独立更新，选定要保留的一份即可统一到其他账号。

![关联会话详情：内容分歧时的分支图](docs/images/session-links-branches.png)

### 积分统计

积分统计页展示官方请求用量、每日趋势、模型分布、账号消耗和请求明细，数据来源与更新时间会明确显示。

![积分统计页面](docs/images/credit-statistics.png)

### Token 统计

Token 统计页按来源展示 Token 总览与趋势、构成占比、活跃热力图、项目/模型 Top 10 与会话排行。

![Token 统计页面](docs/images/token-statistics.png)

## macOS 权限说明

切换账号需要写入 WorkBuddy 认证文件，macOS 要求授权「App 管理」（或「完全磁盘访问」）：

1. 首次切换报「无权限」时，点「打开系统设置」
2. 优先在 **App 管理** 里打开 workbuddy-switch 开关；若没有，则去 **完全磁盘访问** 把 workbuddy-switch 拖进带箭头的框
3. 授权后重启本应用生效；设置页「权限检测」可随时验证

## npm / webui 版本

### 命令行账号管理与切换

账号命令直接执行本地业务逻辑，无需启动 webui。账号库与桌面版共用。

`wb-switch` 是 `workbuddy-switch` 的正式命令别名，以下命令均可使用短名称，例如 `wb-switch accounts list`。

```bash
workbuddy-switch accounts add                       # 浏览器 OAuth 登录并保存账号
workbuddy-switch accounts add --variant ai           # 添加国际版账号
workbuddy-switch accounts add --no-open              # 只打印登录地址，手动打开
workbuddy-switch accounts add --local                # 收集本机 WorkBuddy 当前登录账号
workbuddy-switch accounts add --file accounts.json   # 导入备份中的全部账号
wb-switch accounts list                            # 本地账号元数据，含 index（不含凭据）
wb-switch accounts list --lite                     # 精简表格，实时查询积分
wb-switch credits 2                                # 查询第 2 个账号的积分及全部积分包
wb-switch credits all --lite                       # 查询全部账号积分并输出表格

workbuddy-switch switch workbuddy <account-id>
wb-switch switch ide 2                             # 使用第 2 个账号
wb-switch switch cli 2
wb-switch switch 2 --lite                           # 依次切换全部服务
wb-switch switch cli soonest --lite                 # 可用积分最快过期的账号
wb-switch switch ide richest --lite                 # 可用积分最多的账号
workbuddy-switch switch vscode <account-id> copy true syn true
workbuddy-switch switch jetbrains <account-id> restart true
workbuddy-switch --help
```

目标账号可用 `accounts list` 输出的 `index` 或 `id`。`index` 按账号库顺序从 1 编号，删除账号后会变化，固定脚本建议使用 `id`。`ide` 根据目标账号选择国内/国际版，`cli` 指 CodeBuddy CLI，旧的长服务名兼容保留。

`credits <index|id>` 查询单账号，`credits all` 查询全部账号。完整 JSON 的 `credits` 字段包含 `totalRemaining`（剩余总积分）、`totalCapacity`（套餐总额度）、`soonestExpireAt`（有余额积分包的最早过期时间）、`expiringSoonRemaining`、`expiredRemaining`、`updatedAt` 与 `resources`（积分包额度、余额、已使用、过期时间和状态）。时间戳单位为毫秒，表格时间使用本机时区。查询失败显示未知值和错误，不会当作零积分。

`switch [服务] soonest` / `richest` 实时查询积分后选择目标，排除查询失败、需要重新登录和没有可用余额的账号；已过期积分包不参与排名。`soonest` 还要求存在未来的过期时间；`richest` 允许无过期时间。并列时选列表中靠前的账号。

快捷选择只比较查询成功的账号；失败项记录在 `selectionErrors` 中，表格模式也会提示，此时退出码为 `2`。积分查询逐账号执行，每个账号最多等待 90 秒，避免并发刷新凭据时覆盖账号库更新。

`--lite` 适用于添加、列表、积分和切换命令，使用精简表格。列表只显示 index、ac id、剩余积分、积分最近过期时间、nickname、needrelogin、服务。WorkBuddy 服务归属读取登录文件，CLI 从当前认证配置匹配；IDE/插件显示工具记录的最近切换账号（外部手动切号可能使记录过时）。普通 `accounts list` 无网络查询；`accounts list --lite` 实时查询积分。

省略服务时，顺序尝试 WorkBuddy、IDE、CLI、VS Code、JetBrains，逐项返回成功/失败；任一服务失败仍继续其余服务，不执行跨服务回滚。未安装或未初始化的服务会明确报错。全服务模式的复制/同步只应用于支持的服务，`share` 只用于 WorkBuddy，`restart` 不传给 CLI。

| 参数 | 默认 | 行为 |
| --- | --- | --- |
| `copy true/false` | `false` | 复制当前服务账号的全部可复制会话 / 不复制 |
| `syn true/false` | `false` | 处理当前账号到目标账号的全部关联会话 / 不同步 |
| `overwrite true/false` | `false` | 同步时允许以源内容覆盖冲突 / 跳过冲突并报告 |
| `restart true/false` | `true` | 自动关闭并重开客户端 / 使用原有手动退出模式 |
| `share true/false` | `false` | WorkBuddy 原有共享会话行为 / 不共享 |

所有布尔参数也支持 `--copy true`、`--copy=true` 等写法；不支持指定个别会话。`syn true` 保留后端可同步权限和预览校验：相同内容无需写入，目标领先或无法比较的会话会报告跳过，冲突须加 `overwrite true` 才覆盖。复制不改变源会话。

WorkBuddy 会话操作需要 `restart true`。CodeBuddy CLI 与 JetBrains 不支持会话复制/同步；JetBrains 一次处理全部安装了插件的 IDE。CodeBuddy CLI 切换会关闭运行中的 CLI，不自动重开，也不接受 `restart` 参数。

命令输出 JSON，错误写入 stderr。退出码：`0` 完成、`1` 参数或操作错误、`2` 已执行但存在会话失败/跳过或待恢复项；退出码 `2` 时账号可能已经切换，请查看操作报告。未知命令会报错，不会启动网页服务。

源码开发时先执行 `npm ci && npm run build` 生成 webui 资源，再执行 `cargo build -p wb-switch-server`；可用 `cargo run -p wb-switch-server -- accounts list` 运行本分支的命令。npm 已安装版本只有更新到包含此功能的构建后才会提供这些命令。

```bash
npm i -g workbuddy-switch
workbuddy-switch              # 启动本地服务 + 自动打开浏览器
workbuddy-switch status       # 终端查看当前账号
```

界面与桌面 App 一致，功能覆盖上方全部模块，但不提供会话悬浮窗（桌面版专属）。webui 模式下的 macOS 权限由启动服务的终端进程决定；若终端已授权完全磁盘访问则无需额外操作。

## 致谢

感谢 [Linux.do](https://linux.do) 社区。

## 许可

[MIT](./LICENSE)
