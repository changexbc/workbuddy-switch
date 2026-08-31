# 修复 OAuth 授权链接的 WebUI 打开方式

## Goal

修复 npm/WebUI 模式下 OAuth 扫码登录打开验证链接时报
`Cannot read properties of undefined (reading 'invoke')` 的问题，同时保持
Tauri 桌面 App 的外部链接打开行为不变。

## Background

- `oauthStart()` 通过 WebUI HTTP 通道成功返回了验证链接和 `loginId`。
- [oauth-login-dialog.tsx](../../../src/components/oauth-login-dialog.tsx) 的
  `start()` 与链接点击处理都无条件动态加载 `@tauri-apps/plugin-opener`。
- Tauri opener 依赖宿主注入的 `invoke`；普通浏览器页面没有该对象，因此
  自动打开或再次点击链接时会报错。OAuth 轮询逻辑本身通过 `api.oauthStatus()`
  运行，不是本次错误来源。

## Requirements

- 在普通浏览器 WebUI 中发起 OAuth 后，不得调用依赖 Tauri 注入对象的 opener API。
- WebUI 应使用浏览器自身能力打开验证链接；若浏览器阻止自动弹窗，已生成的链接仍须可点击打开。
- Tauri 桌面 App 继续使用 `@tauri-apps/plugin-opener` 打开系统浏览器。
- OAuth 轮询、扫码完成后的账号入库与现有错误提示行为保持不变。
- 不修改当前工作区中与 Token 统计及 `.playwright-mcp/` 相关的无关改动。

## Out of Scope

- 不修改 OAuth 服务端接口、验证链接格式、轮询间隔或账号入库逻辑。
- 不调整固定 API 端口、桌面端权限、系统浏览器默认设置或其他外部链接入口。

## Key Decisions

- 复用现有 `api.isWebui()` 双通道判定：WebUI 使用浏览器打开链接，Tauri
  继续使用 opener 插件。
- WebUI 自动弹窗被浏览器拦截时不应把它报告为 OAuth 失败；保留已生成的
  链接作为可点击兜底。

## Acceptance Criteria

- [x] WebUI 模式点击 OAuth 扫码后不再出现 `invoke` undefined 错误。
- [x] WebUI 能打开或展示可点击的验证链接，完成扫码后仍能轮询并显示已采集账号。
- [x] Tauri 模式仍通过 opener 插件打开系统浏览器。
- [x] `npm run build` 通过，且 `git diff --check` 通过。

## Risks / Deferred

- 浏览器可能按弹窗策略阻止异步 `window.open`；验收以“不抛 Tauri invoke
  错误且链接可点击打开”为准，自动弹窗失败不阻断轮询。
- 现有前端测试目录未发现 OAuth 专项测试；本次至少通过构建与静态差异检查，
  并对分流函数做代码级验证。

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
