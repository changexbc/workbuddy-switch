# workbuddy-switch

WorkBuddy、CodeBuddy IDE、CodeBuddy CLI 与 VS Code CodeBuddy 插件账号切换工具，浏览器中操作（webui），四者均支持国内版 / 国际版，并提供积分到期与 Token 用量监控。同时提供桌面 App（Tauri）版本。

多账号共享登录态，一键切换 WorkBuddy 登录账号。**会话复制**：把当前账号的会话以新 id 复制给目标账号，源账号数据不受影响，云端归属目标账号。

**在线演示**：[打开 GitHub Pages 演示](https://changexbc.github.io/workbuddy-switch/)（只读演示；账号、积分与请求记录均为虚构数据，所有业务操作均已禁用）

## 快速开始

### npm 安装（webui）

```bash
npm i -g workbuddy-switch
workbuddy-switch              # 启动本地服务 + 自动打开浏览器
workbuddy-switch status       # 终端查看当前账号
```

webui 界面与桌面 App 一致，功能覆盖下方全部模块。

### 桌面 App

从 [GitHub Releases](https://github.com/changexbc/workbuddy-switch/releases/latest) 下载 macOS / Windows / Linux 安装包（Tauri，推荐日常使用）。

> **macOS 提示「已损坏，无法打开」？** 未签名应用会触发隔离机制，在终端执行一次即可：
>
> ```bash
> xattr -rd com.apple.quarantine "/Applications/workbuddy-switch.app"
> ```

应用能启动但切换账号时提示无权限，请参阅下方 [macOS 权限说明](#macos-权限说明)。

## 功能

| 模块 | 说明 |
| --- | --- |
| 账号管理 | OAuth 扫码登录、导入导出账号、删除账号 |
| 账号切换 | 一键切换 WorkBuddy 登录账号，切换过程实时显示进度 |
| 会话复制 | 把当前账号勾选的会话复制给目标账号，源账号数据不受影响 |
| 积分到期查询 | 自动查询每个账号的积分剩余量与到期时间；7 天内到期高亮，并按紧迫程度排序、标注「建议优先使用」 |
| 积分统计 | 汇总官方请求用量：总览、近 30 天趋势、模型分类、账号消耗与请求明细 |
| Token 统计 | 按来源查看 Token 总览与趋势，含构成占比、活跃热力图、项目/模型 Top 10 与会话排行 |
| CodeBuddy CLI | 与 WorkBuddy 复用同一账号库，默认账号独立；切换后立即生效，无需重启 CLI |
| CodeBuddy IDE | 支持切换 CodeBuddy IDE 桌面客户端账号，与 CodeBuddy CLI 相互独立 |
| VS Code CodeBuddy 插件 | 支持切换 VS Code 内的 CodeBuddy 插件账号；VS Code 运行时可自动关闭并在写入后重新打开 |
| 插件会话复制 | 切换插件账号时，可把当前插件账号的会话复制给目标账号（加法，源账号不变） |
| 自动轮换 | 后台把积分最紧迫的账号设为 CodeBuddy CLI 后续启动账号；检测到 CLI 会话运行时会跳过 |
| 自动更新 | 从 GitHub Releases 检查新版本，整包更新经签名校验 |
| 权限检测 | macOS 授权引导（App 管理 / 完全磁盘访问拖拽授权 + 自动检测） |

## 使用

命令行账号管理（无需启动 webui）：

安装后同时提供 `wb-switch` 和 `workbuddy-switch`，两者行为相同；例如 `wb-switch accounts list`。

```bash
workbuddy-switch accounts add                 # OAuth 登录添加账号
workbuddy-switch accounts add --variant ai    # 国际版账号
workbuddy-switch accounts add --local         # 导入本机 WorkBuddy 登录态
workbuddy-switch accounts add --file accounts.json
wb-switch accounts list                      # 本地脱敏 JSON，含 index
wb-switch accounts list --lite               # 精简表格，实时查询积分
wb-switch credits 2                          # 单账号积分与积分包明细
wb-switch credits all --lite                 # 全部账号积分
workbuddy-switch switch workbuddy <account-id>
wb-switch switch ide 2
wb-switch switch cli 2
wb-switch switch 2 --lite                     # 依次切换全部服务
wb-switch switch cli soonest --lite           # 可用积分最快过期
wb-switch switch ide richest --lite           # 可用积分最多
workbuddy-switch switch vscode <account-id> copy true syn true
workbuddy-switch switch jetbrains <account-id>
workbuddy-switch --help
```

`copy`、`syn` 默认 `false`，`true` 分别复制全部可复制会话、处理全部关联会话，不支持逐会话选择。冲突默认跳过并报告，`overwrite true` 允许源内容覆盖冲突。`restart` 默认 `true`，支持自动关闭并重开；CodeBuddy CLI 总是关闭现有 CLI 且不重开，不接受此参数。JetBrains 操作全部安装了插件的 IDE。CLI 和 JetBrains 不支持会话操作。WorkBuddy 另有 `share true/false`，会话操作需要 `restart true`。

布尔参数也可写为 `--copy true` 或 `--copy=true`。CodeBuddy IDE 根据目标账号选择国内/国际版。退出码 `0` 完成，`1` 错误，`2` 部分会话失败/跳过或需要恢复（账号可能已经切换，查看 JSON 报告）。

目标可用 `index` 或 `id`，index 按列表顺序从 1 编号，删除后可能变化。`credits <index|id|all>` 输出积分；JSON 的 `credits` 字段包含剩余总积分 `totalRemaining`、套餐总额度 `totalCapacity`、最近过期时间 `soonestExpireAt`（毫秒）和各积分包 `resources`。表格时间使用本机时区。

`--lite` 将添加、列表、积分、切换结果显示为表格。`accounts list --lite` 仅显示 index、ac id、积分、积分最近过期时间、nickname、needrelogin、服务；普通 list 只读取本地账号元数据。IDE/插件服务归属来自工具记录的最近切换账号，外部手动切换可能使记录过时。

`soonest` / `richest` 先实时查询，排除失败、需要重新登录和没有可用积分的账号，过期积分不参与排名。soonest 要求存在未来过期时间，richest 允许无过期时间，并列按 index 选择。查询失败输出错误、退出码 2，不显示成零积分。

积分逐账号查询（每个账号最多等待 90 秒），避免并发刷新凭据覆盖账号库更新。快捷选择基于成功查询的候选，失败项写入 `selectionErrors`，表格模式也提示，此时退出码为 2。

不指定服务时依次尝试全部五种服务，一项失败后继续其余服务，无跨服务回滚。未安装服务会报告失败。copy/syn 只应用于支持会话操作的服务，share 仅用于 WorkBuddy，restart 不传给 CLI。旧的 codebuddy-ide/codebuddy-cli 服务名兼容保留。

1. **添加与导出账号**：账号页 →「OAuth 扫码登录」「导入备份」；「导出」可将勾选账号备份为 JSON
2. **切换账号与账号信息**：账号卡片 →「切换」，可勾选复制当前会话；「账号信息」可给账号添加备注，并选择卡片上显示账号名 / 手机号 / 备注
3. **查看积分与统计**：账号页自动查询各账号积分到期情况，点「刷新积分」手动更新；侧栏进入「积分统计」「Token 统计」查看用量明细
4. **切换各客户端账号**：CodeBuddy CLI、CodeBuddy IDE、VS Code CodeBuddy 插件均可在账号卡片一键切换；其中 VS Code 插件支持在弹窗中勾选复制当前账号的会话。CodeBuddy IDE 首次使用前需先手动打开并登录一次
5. **自动轮换**：设置 → CodeBuddy CLI 自动轮换，开启后按积分紧迫程度自动设置默认账号
6. **更新**：设置 → 自动更新可检查公开 GitHub Releases 源；npm 版本也可通过 `npm update -g workbuddy-switch` 升级

## macOS 权限说明

切换账号需要写入 WorkBuddy 认证文件，macOS 要求授权「App 管理」（或「完全磁盘访问」）：

1. 首次切换报「无权限」时，点「打开系统设置」
2. 优先在 **App 管理** 里打开 workbuddy-switch 开关；若没有，则去 **完全磁盘访问** 把 workbuddy-switch 拖进带箭头的框
3. 授权后重启本应用生效；设置页「权限检测」可随时验证

> webui 模式：由启动服务的终端进程权限决定；若终端已授权完全磁盘访问则无需额外操作。

## 许可

[MIT](./LICENSE)
