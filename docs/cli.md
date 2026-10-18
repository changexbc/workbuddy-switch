# CodeBuddy CLI 命令行

一次性命令执行后退出，无需打开 GUI 或启动 webui。账号与积分共用现有账号库，目前客户端目标只支持 `codebuddy-cli`。

## 命令

| 命令 | 用途 |
| --- | --- |
| `inspect [ACCOUNT_ID] --summary` | 一次查询账号、CLI 状态、新鲜积分、到期信息和模型限额 |
| `inspect [ACCOUNT_ID]` | 同上，保留完整账号和资源明细 |
| `accounts [list]` | 账号元数据与稳定 ID，不返回登录凭据 |
| `status --target codebuddy-cli` | 当前 CLI 账号及认证配置状态 |
| `credits [ACCOUNT_ID \| --all] [--summary]` | 查询积分；不指定账号时查询全部 |
| `limits [ACCOUNT_ID]` | 读取观察到的模型限制与恢复时间 |
| `switch ACCOUNT_ID [--dry-run]` | 切号或预览影响范围 |
| `help [--json]` / `version [--json]` | 帮助或版本 |
| `serve [--port PORT] [--no-open]` | 持续运行 webui；不带命令也启动 webui |

优先使用返回的账号 `id`。UID 只有唯一匹配时可用，昵称不作为账号选择器。不带 `--target` 的旧 `status` 仍查询 WorkBuddy。

## 参数

| 参数 | 适用范围与默认值 |
| --- | --- |
| `--summary` | inspect/credits：紧凑结果，不增加请求 |
| `--text` / `--json` / `--ascii-json` | 一次性命令的输出格式；text 不可与 JSON 参数混用 |
| `--target codebuddy-cli` | 查询/切号的客户端目标，默认 codebuddy-cli |
| `--variant cn\|ai\|all` | accounts/inspect/credits/limits/switch，默认 all；旧 status 只接受 cn/ai |
| `--model MODEL` | inspect/limits，按模型名精确筛选 |
| `--timeout SECONDS` | inspect/credits，每账号默认 30 秒，范围 1–3600 |
| `--concurrency N` | inspect/credits，默认 4，范围 1–16 |
| `--wait SECONDS` | 查询/切号等待认证锁，默认 30 秒，范围 0–3600；0 为立即失败 |
| `--quiet` | 关闭 CLI 进度输出 |
| `--dry-run` | 只用于 switch；不写配置、不关闭进程，也不验证凭据 |

新查询与切号命令默认 JSON；help、version 和旧 WorkBuddy status 默认文本。`-h/--help`、`-V/--version` 是帮助和版本别名。

## 返回结果

stdout 为一个最终 JSON 对象，stderr 为进度和诊断；不要合并后解析。等待期间每 2 秒有心跳，逐账号查询会报告完成计数。JSON 进度为 JSON Lines，底层诊断也可能是文本。

公共字段：`schemaVersion`、`ok`、`command`、`target`、`scope`、`elapsedMs`、`exitCode`、`error`。成功时 error 为 null，失败时为 `{code,message,retryable,hint}`，逐账号查询错误使用相同结构。retryable 的 true/false/null 分别表示可重试、不宜直接重试、无法可靠判断。

scope 为 account（账号/积分）、client（客户端状态/切号/限额观察）或 mixed（inspect）；help/version 的 scope、target 为 null。优先读取 `clientStatus`、`activeAccountId`、`activeForTarget`；早期别名 `codebuddyCli`、`activeCliAccountId`、`activeCli` 仍保留。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功，空账号列表也算成功 |
| 1 | 操作失败，或全部积分查询失败 |
| 2 | 参数或账号选择错误 |
| 3 | 部分结果，已成功的数据保留 |
| 130 | 正常中断，执行中的认证事务已完成 |

inspect 最后的状态读取失败时保留积分与限额，以 `clientStatusError` 说明错误；状态未验证的当前标记为 null。可以单独重试 status，无需重新查询全部积分。

## 积分与到期

- `summary.complete` 表示积分查询是否全部成功；`balanceComplete` 还要求成功账号的额度明确。失败账号不计入总量。
- `balanceType` 区分 finite/unlimited/unknown。不限量账号 `unlimited=true`，余额/容量为 null；未知值也不能当成零。
- 汇总包含 `unlimitedAccounts`、`unknownBalanceAccounts` 及已知有限小计 `finiteTotalRemaining/finiteTotalCapacity`。存在不限量或未知额度时，整体总量为 null。
- 原始数值用于计算；`*Display` 是两位小数展示字符串，不表示官方最小积分单位。
- summary 省略独立联系字段和完整 resources，名称仍遵循账号展示设置。到期分组只保留正余额，排除已过期明细，未来和未知到期的条目不设数量上限。
- `expiryBalancesAsOf` 是摘要过滤的 Unix 毫秒时间。`expiryKnown=false` 表示到期时间未知；`soonestExpireAt/soonestExpireAtUtc` 是保留明细中最近的已知未来到期时间，无此时间时为 null。

摘要不重算官方余额，到期分组之和可能不同于原始总余额。`queryStartedAt/queriedAt`、积分 updatedAt、limitsScannedAt 表示各项时间，整份 inspect 并非原子快照。查询期间删除的账号保留本次结果并标记 present=false。

## 模型限额

limits 扫描最近 2 天的本机 CodeBuddy CLI 日志，并读取已落盘的共享账号 hook 台账，不消费事件队列、不安装 hooks、不扫描其他客户端日志。台账可能包含其他客户端捕获的同账号限额；未落盘的 hook 信号可能暂不可见。

条目包含 `resetAt`（Unix 毫秒）、`resetAtUtc`（UTC ISO 时间）、`remainingSeconds`，以及首次观察时间 observedAt、实际来源 source/sources/sourceClients、reason/evidence。历史记录未保存的来源信息不会猜填。

`modelStates.status=limited` 表示有活动限额证据；`--model` 指定但没有活动证据时为 unknown。未列出的模型、已到恢复时间的模型也仍是未知，不能据此证明调用可用。model=null 的 scope 为 unknown，不推断账号级限制；`unattributedLimitCount` 在模型筛选后仍保留。工具不主动调用模型探测可用性。

## 切号与并发

实际切号先关闭正在运行的 CodeBuddy CLI，再切换账号，不自动重开；不复制、删除或隔离本地对话。Windows 会关闭进程树。

dry-run 的 impact 列出 PID、可获取的启动时间、匹配数量和可能受影响的后代。任务是否忙碌为 unknown；这是时间快照，实际切号会重新扫描。进程查询失败返回 PROCESS_SCAN_FAILED，不会当成无会话继续写入。

真实结果中 `autoReopened=false`；关闭计数与 closeAttemptedProcessCount 表示尝试数，`processTerminationVerified=false`，不是逐进程退出证明。当前未生成持久备份，`backupCreated=false`、backupPath/backupId=null，内存回滚不能当作磁盘备份。

配套新版 GUI 与 CLI 共用跨进程认证锁，并同步账号状态与积分缓存。旧发行版不支持这套协议，同时写入须完全退出旧 GUI。遇到竞争时 CLI 自动等待，超时返回 AUTH_STATE_BUSY；不要删除锁文件或杀掉刷新进程。积分查询可能刷新凭据、写缓存，不是严格只读。

带历史「需重新登录」标记的账号再次请求成功时，会自动复核续期；续期成功后清除旧标记并同步界面。失败不丢弃已成功的查询结果，同一账号的失败复核至少间隔 60 秒；正常账号不增加复核请求。

正常 Ctrl+C/Windows Ctrl+Break/Unix SIGTERM 会停止排队，等待已开始的认证更新或切号完成，再以 130 返回。已完成结果保留，operationOk 表示操作结果；重试中断的切号前先确认当前账号。超时清理也可能使耗时超过 --timeout。强制终止、SIGKILL 和断电不受正常中断协议保护。

## 编码

stdout/stderr 使用无 BOM 的 UTF-8。AI 子进程按 UTF-8 解码；旧 PowerShell 5.1 管道可用 `--ascii-json`，标准 Unicode 转义在 JSON 解析后恢复原文，底层文本诊断不受此参数控制。

PowerShell 5.1 的 `>` 会将文本重新编码为 UTF-16LE；可用 `Out-File -Encoding utf8` 保存（5.1 带 BOM），或使用 PowerShell 7.4+ 原生重定向保留原始字节。

## 构建与维护

```bash
npm ci
npm run build
cargo build -p wb-switch-server --release
```

产物为 target/release/wb-switch（Windows 为 wb-switch.exe），Rust 需 1.89 或更新版本。测试使用 `WB_SWITCH_HOME` 隔离账号库与 CLI 配置；三平台 CI 包含 CLI 子进程与 npm 入口，本地通过不能代替跨平台 CI。

CLI 模块位于 crates/wb-switch-server/src/cli/，客户端操作集中在 Target 分派，账号与余额服务保持共享。core 不依赖 Tauri。认证锁覆盖 token 轮换和本地提交，当前按账号库串行化；账号库损坏时拒绝覆盖，缓存与统计拒绝迟到结果。并发锁、OAuth 保存和字段兼容的细节以 core 实现及测试为准。
