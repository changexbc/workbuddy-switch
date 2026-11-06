# 国际版每日活跃会话（网页通道）

> 状态：已实现并通过真机验证（2026-11-04）
> 代码：`crates/wb-switch-core/src/modules/web_activity.rs`

## 为什么需要它

国际版**没有签到**。`/billing/meter/daily-checkin` 对国际版账号恒返回
`10001 签到活动未开启`，所以 `variantSupportsCheckin()` 对 `ai` 档位返回 false，
账号页也不展示签到入口。

但国际版有一条独立的奖励链路：**在网页端建一条 agent 会话并把它真正跑完**，
官方按「每日活跃」发放 30/50 积分。这条链路与签到完全无关：

| | 签到 | 每日活跃 |
|---|---|---|
| 接口 | `POST /billing/meter/daily-checkin` | `POST /console/as/conversations/` + ACP |
| 准入门槛 | 服务端 `active` 开关 | 无开关，会话 `completed` 即计分 |
| 适用档位 | 国内版 | 国际版 |
| 鉴权 | 桌面端完整指纹 | 仅 `Authorization: Bearer` + `X-User-Id` |

## 执行链路

一次打卡 = 一个会话的五步：

1. **建会话**
   `POST https://www.workbuddy.ai/console/as/conversations/`

   ```json
   {
     "prompt": "Hi",
     "model": "hy3",
     "conversationOrigin": "workbuddy-app",
     "plugins": [{ "name": "weixinpay", "marketplace": "codebuddy-builtin" }]
   }
   ```

   建会话只是**排队**：不接沙箱、不请求这一轮，agent 永远不会跑。

2. **取沙箱**
   `GET .../conversations/{id}/session` → `link`（E2B 沙箱地址）、`token`、
   `sessionId`、`cwd`。

3. **驱动 ACP**（JSON-RPC over streamable HTTP）
   - `GET <link>`，`Accept: text/event-stream`，从响应头取 `Acp-Connection-Id`
   - `POST <link>` 依次发 `initialize` → `session/load` → `session/set_model`
     → `session/prompt`，每条都带 `Acp-Connection-Id`
   - 后台持续读 SSE 流，避免连接被判空闲

4. **轮询到完成**
   `GET .../conversations/{id}`，直到 `status == "completed"`（单轮上限 120 秒）。

5. **收尾**
   成功 `POST .../stop`（会话留在历史里，只改状态）；失败 `POST .../delete`
   避免网页端堆满「Hi」。

## 模型与成本

按 `MODEL_CANDIDATES` 顺序尝试，成功即停：

| 模型 | 倍率 | 上下文 | 实测单次成本 |
|---|---|---|---|
| `hy3` | x0.00 | 192k | **0.00** |
| `deepseek-v4.1-flash` | x0.11 | 300k | 9.36 |

打卡只需要一句「Hi」加一段短回复，默认用免费的 `hy3`；hy3 失败（限流/下线）
才顺延到 deepseek。

**注意**：网页通道无法单独下调思考档位。ACP 只提供 `session/set_model`，没有
reasoning effort 方法，所以 `deepseek-v4.1-flash` 的 `onlyReasoning: true` /
`defaultEffort: high` 改不动——省钱只能换模型，不能调参。

多账号连续执行时 `hy3` 可能触发限流（实测 3 个账号中第 3 个回退到了 deepseek）。

## 配置与接口

开关文件：`~/.wb-switch/daily_activity_config.json`，默认 `{"enabled": true}`。
**时间段不单独配置**，跟随签到配置的 `checkin_start` / `checkin_end`：两端都合法
且 `start < end` 才生效，未设置 = 不限制。

去重状态：`~/.wb-switch/daily_activity_state.json`，记录 `uid → 最后成功日期`。
当天已完成的账号直接 `skipped`，不发任何请求。

| 接口 | 说明 |
|---|---|
| `GET /api/activity/config` | 读开关 |
| `POST /api/activity/config` | 写开关 |
| `GET /api/activity/status` | 各账号今日状态（账号页 chip 用） |
| `POST /api/activity/daily` | 单账号手动跑（不受开关与时间段限制） |
| `POST /api/activity/daily/all` | 所有国际版账号手动跑一轮 |

调度：`lib.rs`（Tauri）与 `main.rs`（server）各起一个后台循环，启动即跑一轮，
之后每 30 分钟轮询一次。只记录「真跑了」和「失败」，`skipped` 不刷日志。

## 不切换账号

整个模块**没有任何 `switch` 调用**。它只读账号库里该账号自己的 `access_token`
和 `uid`，直接打网页通道。当前 WorkBuddy 登录态不受影响，账号库里有多少个国际版
账号就能跑多少个，与当前选中哪个账号无关。

## 已知限制

**沙箱域名的 DNS**：`*.sandbox.cloudstudio.club` 会被 Clash 的 fake-ip 模式解析成
`198.18.x.x`，TLS 握手直接被掐断（`SSLEOFError: UNEXPECTED_EOF_WHILE_READING`）。
模块内置 DoH 兜底（`dns.alidns.com` 查 A 记录 + `ClientBuilder::resolve()` 固定
真实 IP），不依赖用户配置。真实地址在腾讯云 EdgeOne。

**活跃会话上限**：上游对单账号同时存在的 `working` 会话有硬上限 10 条。建了必须
收尾，否则攒满后建会话直接 429 / 10105。名额满时会先 `prune_oldest` 停一条最旧的
再重试一次。

**协议是逆向的**：这套链路依赖上游私有协议，上游收紧非官方客户端识别时可能失效。
失效后需要对照网页端抓包更新 `web_activity.rs`。
