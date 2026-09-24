# 自定义 Agent Hooks 接入

内置来源只有 Codex、WorkBuddy、CodeBuddy、Codeg 四家。其他 Agent 通过一份声明式 JSON 模板接入，再在目标工具里配置一条本机命令，把它的原始事件送到 Agent Companion。

- 模板是数据：应用不执行模板里的任何内容，也不会自动安装、卸载或修改第三方工具的配置。
- 首版只支持本机命令提交（`transport: "hook"`），不新增公开网络接收端点。
- **尚未与 Claude Code、Antigravity 等具体工具做过真实联调。** 本文「模拟验证」一节全部是模拟载荷；字段语义取决于目标工具实际发送的内容，套用前必须核对。应用只接收你配置的 Hook 输出，不承诺任意 Agent 全能力接入。

> **当前状态**：设置页的「自定义 Agent」入口暂未开放（先用接口与命令操作；后续"内置适配器（开关式接入）"版本会开放）。下文所有「设置页」相关操作都以等价接口给出。

## 先决条件

- Agent Companion 桌面应用正在运行（它提供本机 IPC 接收服务）。
- 目标工具支持在事件发生时执行一条本机命令，并能把事件 JSON 写到命令的标准输入。
- 你能编辑目标工具自己的 Hook / 命令配置。

## 快速开始

（设置页入口开放前以接口操作；开放后为等价的设置页步骤。接口基址：开发链路为 `http://127.0.0.1:8849`。）

1. 导入模板：`POST /api/custom-integrations`，body 为 `{"action":"import","template":{…}}`（模板可直接用 `docs/examples/custom-agent-hooks/generic-agent.json` 的内容）。
2. 可选先预览：`POST /api/custom-integrations/preview`，body 为 `{"template":{…},"payload":{…}}`，确认样例载荷的映射结果。
   - 预览使用与正式接收完全相同的纯映射逻辑，不会创建会话。
   - 导入成功只代表模板可用，**不代表已经收到过任何事件**：`lastReceivedAt` / `lastMappedAt` 仍是 `null`。
3. 从 `GET /api/custom-integrations` 的 `command` 字段取出「事件提交命令」。
4. 把这条命令配置到目标工具的 Hook 里（见「手动安装」）。应用不会代你写入。
5. 触发一次真实事件，查询 `GET /api/custom-integrations` 的 `lastReceivedAt` / `lastMappedAt` 与诊断数组，在悬浮窗查看会话状态。

## 模板字段（v1）

顶层与 `mapping`、每个事件的动作对象都不允许出现未知字段——拼写错误会直接报错，而不是被静默忽略。

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `schemaVersion` | 是 | 固定为 `1`。 |
| `id` | 是 | `[a-z][a-z0-9-]{0,63}`；内部来源为 `custom:<id>`。不能使用内置 ID（`codex`、`workbuddy`、`codebuddy-ide`、`codeg`）。 |
| `name` | 是 | 界面显示名称，1–120 字符。 |
| `transport` | 是 | 固定为 `"hook"`。 |
| `mapping.event` | 是 | 指向原始事件名的 RFC 6901 Pointer，如 `/event_name`。 |
| `mapping.sessionId` | 是 | 指向会话 ID 的 Pointer。 |
| `mapping.roundId` | 否 | 指向轮次 ID；提供后可严格隔离旧轮次迟到事件。 |
| `mapping.eventId` | 否 | 指向事件 ID；提供后按「来源 + 会话」去重。 |
| `mapping.timestamp` | 否 | 指向整数 Unix 毫秒。字段缺失或为 `null` 时使用接收时间；若指向的值不是正整数，则拒绝该事件（`type_mismatch`），不会改用接收时间。 |
| `mapping.cwd` | 否 | 工作目录，用于显示项目名。 |
| `mapping.title` | 否 | 会话标题。 |
| `mapping.requestId` | 否 | 等待项 ID；提供后 `wait`/`resume` 按具体等待项配对。 |
| `ignoreIfPresent` | 否 | 最多 16 个 Pointer；任一指向非空值时忽略整个事件。 |
| `events` | 是 | 1–64 组「原始事件名 → 动作」。 |

Pointer 只支持 RFC 6901：以 `/` 开头、token 不能为空、`~` 必须写成 `~0`/`~1`。不支持 JSONPath、通配符或表达式。指向不存在的字段等同于「未提供」。

## 事件与动作

| 动作 | 附加参数 | 含义 |
| --- | --- | --- |
| `start` | — | 新一轮执行开始（不是「应用打开」）。 |
| `wait` | `reason`: `permission` \| `input` | 等待权限确认或等待用户输入。 |
| `resume` | — | 解除一个等待项。 |
| `finish` | `status`: `done` \| `error` | 本轮成功或失败结束。 |
| `close` | — | 会话关闭；仍在运行的轮次以 `aborted` 结束并记录关闭原因。 |

其他动作不接受多余参数（给 `start` 配 `reason` 会被拒绝）。`events` 里没有列出的原始事件会被忽略并计入诊断，**不会**创建会话，也不会回退成任何内置来源。

### 生命周期、轮次与去重

- 配置了 `roundId` 时，`start` 与后续事件必须同轮次；旧轮次的结束/恢复事件不会影响新轮次。
- 未配置 `roundId` 时由 `start` 分配轮次（`custom:<接收时间>`），后续事件关联当前轮次；这种情况下只支持同一会话按序投递，**无法**识别迟到的旧轮次事件。需要强保证就必须映射 `roundId`（可以让目标工具或你自己的转换脚本提供）。
- 没有活跃轮次时的 `wait`/`resume`/`finish` 不会创建会话，只记诊断。
- 有 `eventId` 时按来源+会话有界去重（每会话最近 64 条）；同轮次的重复 `start` 不会重置已结束的轮次。没有 `eventId`/`roundId` 的重复 `start` 无法严格区分——这是文档明确的保证边界。
- 有 `requestId` 时 `wait`/`resume` 按等待项配对；没有时同一会话只支持一个等待项。不要因为收到任意工具事件就断言「已恢复」，语义要由模板作者确认。
- 应用重启后模板仍在，但未结束的会话状态不会恢复；目标工具再次发送事件即可继续（有 `roundId` 时保持同一轮次）。

### 子任务过滤

把子任务字段（例如 `/parent_session_id`）放进 `ignoreIfPresent`，命中的事件在创建会话之前就被忽略，不会污染主会话列表。`null` 与空字符串视为「不存在」。字段名和语义取决于目标工具，不要盲目照抄。

## 手动安装

在设置页复制到的命令形如：

```
'/Users/you/.agent-studio/bin/agent-studio-runtime-v1' custom-hook --home '/Users/you' --integration example-agent
```

- 来源只由 `--integration` 参数决定；载荷里写 `source`、`agent_source` 之类的字段不会改变来源，也不能伪装成内置来源。
- 标准输入读一条 JSON（原始载荷），**标准输出永远为空**：某些宿主会解析 Hook 的输出，我们不返回任何可能被当成指令的内容。
- 失败一律写到 stderr 并返回非 0：参数缺失或 ID 非法、载荷超过 1 MiB、载荷不是合法 JSON、接收服务未启动、事件被拒绝（例如缺少会话 ID、来源已停用）。
- 如果目标工具把非 0 退出码当成故障，请自行包装成始终成功的形式，例如 `... custom-hook --home "$HOME" --integration example-agent || true`。
- 有些宿主需要一个 JSON 响应体才能继续，请在目标工具侧自行包装（例如把 stdout 替换成 `{}` 的包装脚本）；本命令不会返回通用 JSON。

把这条命令加到目标工具的 Hook 配置里（各工具格式不同，以该工具官方文档为准），并把该工具的事件 JSON 作为命令的标准输入。若目标工具只发送自己的事件名和字段，请用模板 `mapping` 对应它们的名字，而不是照抄示例。

开发链路（`npm start` 的 Node 采集器，默认 `http://127.0.0.1:8849`）用同一个映射引擎，可用 HTTP 直接验证：

```
curl -s -X POST http://127.0.0.1:8849/api/custom-hook \
  -H 'Content-Type: application/json' \
  -d '{"integration":"example-agent","payload":{"event_name":"prompt_submitted","session_id":"demo","round_id":"t1"}}'
```

## 模拟验证

用 `docs/examples/custom-agent-hooks/` 里的模板与载荷逐个验证。示例载荷**故意不带时间戳**，接收时间由应用补上，因此任何时候重放结果一致。先导入 `generic-agent.json`（`minimal-agent.json` 用于最后一组），然后按顺序执行：

```
BIN="$HOME/.agent-studio/bin/agent-studio-runtime-v1"
"$BIN" custom-hook --home "$HOME" --integration example-agent < docs/examples/custom-agent-hooks/scenarios/basic-lifecycle/01-start.json
```

| 场景/文件 | 预期结果 |
| --- | --- |
| `basic-lifecycle/01-start.json` | 接受 `start`；会话 `sim-basic` 运行中，标题「把登录页按钮对齐」，项目 `demo` |
| `basic-lifecycle/02-wait-permission.json` | 接受 `wait`；状态变为等待，待确认 1 项 |
| `basic-lifecycle/03-resume.json` | 接受 `resume`；回到运行中 |
| `basic-lifecycle/04-finish-done.json` | 接受 `finish`；本轮完成 |
| `basic-lifecycle/05-finish-again.json` | 忽略（`round_ended`）；已完成轮次不会被改成失败 |
| 重发 `basic-lifecycle/01-start.json` | 忽略（`duplicate_event`）；不会重启已完成的轮次 |
| `aborted-and-late/01-start-round-1.json` | 接受 `start`；会话 `sim-close` 运行中 |
| `aborted-and-late/02-close-round-1.json` | 接受 `close`；以 `aborted` 结束，关闭原因 `session_closed` |
| `aborted-and-late/03-start-round-2.json` | 接受 `start`；进入新轮次 `turn-2` |
| `aborted-and-late/04-late-finish-round-1.json` | 忽略（`late_round`）；旧轮次事件不影响新轮次 |
| `ignored-and-rejected/01-start-parent.json` | 接受 `start`；会话 `sim-parent` 运行中 |
| `ignored-and-rejected/02-subtask-start.json` | 忽略（`ignored_field_present`）；不会出现子任务会话 |
| `ignored-and-rejected/03-unknown-event.json` | 忽略（`unknown_event`）；只记诊断 |
| `ignored-and-rejected/04-missing-session.json` | 拒绝（`missing_session_id`，字段 `/mapping/sessionId`） |
| `minimal-agent/01-begin.json`、`02-end.json` | 接受 `start`、`finish`；轮次由接收时间分配 |

每个结果都会出现在设置页的「事件诊断」里（接受/忽略/拒绝、原因、事件名和时间）。

## 限制

| 项目 | 上限 |
| --- | --- |
| 模板大小 | 1 MiB |
| 单条载荷大小 | 1 MiB |
| `name` / 事件名 | 120 / 200 字符 |
| Pointer 长度 | 512 字符（按字符计，不是字节） |
| `events` / `ignoreIfPresent` | 64 / 16 |
| `sessionId`、`roundId`、`requestId` | 256 字符 |
| `eventId` / `cwd` / `title` | 200 / 2048 / 4096 字符 |
| 去重缓存 | 每会话 64 条 |
| 内存中会话数 | 256 |
| 诊断记录 | 最近 50 条（不落盘完整载荷或凭据） |

## 排障

| 现象 | 原因与处理 |
| --- | --- |
| 导入报「模板字段 /xxx 无效」 | 字段拼错、缺少必填项或值不合法；错误里的路径就是出错位置。 |
| 导入报「已存在 ID 为 … 请先删除后再导入」 | 同 ID 不会静默覆盖；先删除旧来源再导入。 |
| 导入报「自定义接入配置不可用…已保留原文件」 | `~/.agent-studio/custom-integrations.json` 损坏；按提示修复或删除该文件后重启应用。内置来源不受影响。 |
| 命令返回「监听服务未启动」 | 桌面应用没在运行，或 `--home` 不是应用使用的用户目录。 |
| 诊断里全是 `unknown_event` | `mapping.event` 指错了字段，或事件名和目标工具实际发送的不一致。 |
| 诊断里是 `missing_session_id` / `type_mismatch` | `mapping.sessionId`（或其它字段）指向了不存在、类型不对的字段；对照目标工具真实载荷修正。 |
| 诊断里是 `integration_disabled` | 该来源已停用；在设置页启用，或确认命令里的 `--integration` 是想要的来源。 |
| 诊断里是 `no_active_round` | 先收到 `wait`/`finish` 之类的后续事件，却没有先收到 `start`；检查事件顺序或补齐 `roundId`。 |
| 有「最近收到事件」但没有「最近成功映射」 | 事件到了但没被接受或忽略前即被拒绝；看诊断里的原因。 |
| 只导入模板，界面一直显示「尚无」 | 这是正确行为：没有事件就没有会话。 |

## 停用、删除与手动移除

- **停用**：该来源的事件会被拒绝（诊断 `integration_disabled`），运行态显示被清除；模板保留，随时可再启用。
- **删除**：模板从 `~/.agent-studio/custom-integrations.json` 移除，运行态显示清除。删除只影响这一个来源。
- 应用**不会**改动目标工具里的 Hook 配置，也不会帮你移除。停用或删除后，请自行到目标工具的配置里删掉那条 `custom-hook` 命令。

## 契约与验证工具

- JSON Schema：`docs/schemas/custom-agent-hooks-v1.json`（draft-07，与运行时校验逐字段对应）。
- 可导入示例：`docs/examples/custom-agent-hooks/generic-agent.json`、`minimal-agent.json`；模拟载荷见 `scenarios/`。
- 共享用例：`tests/fixtures/custom-hooks.json`（Rust 与 Node 实现共用）。
- 验证命令：
  - `node scripts/qa-custom-schema.mjs`：示例与 Schema、运行时校验、共享用例三者一致。
  - `node scripts/qa-custom-parity.mjs`：Rust 与 Node 对拍（模板校验、映射结果、生命周期序列）。
  - `node scripts/qa-custom-hooks-e2e.mjs`：临时 HOME + 本机 IPC 的端到端演练（导入、命令提交、重启、停用、删除）。
  - 测试：`cargo test -p agent-studio-core --test custom`、`node --import tsx --test tests/custom-hooks.test.js`。

## 真实验证状态

- 已验证：上述模拟载荷在临时 HOME 与本机 IPC 上完整走通；Rust 与 Node 使用同一份共享用例；导入/预览/启停/删除/诊断的接口流程在真实采集器上走通（设置页入口当前未开放）。
- **未验证**：与 Claude Code、Antigravity 或任何具体第三方工具的真实 Hook 对接。它们的真实事件名、字段与 Hook 配置格式需要在对应版本上核对后才能写进模板；在此之前不要把本文示例当成它们的真实适配。
