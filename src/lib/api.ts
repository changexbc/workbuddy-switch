import { invoke } from "@tauri-apps/api/core";
import type {
  AccountMeta,
  AccountRecord,
  AppNotification,
  AppStatus,
  AutoRotateConfig,
  CodeBuddyCliInstallResult,
  CodeBuddyCliStatus,
  CodeBuddyCliSwitchResult,
  CodeBuddyCnIdeStatus,
  CodeBuddyCnIdeSwitchResult,
  CheckinConfig,
  CheckinLog,
  CheckinResult,
  CreditExpiry,
  CreditStatistics,
  TokenStatistics,
  ErrorLogKind,
  GithubConfig,
  ImportPreviewAccount,
  ImportResult,
  OAuthPollResult,
  OAuthStartResult,
  RateLimitConfig,
  RateLimitHookStatus,
  RateLimitsPayload,
  RotateLog,
  RotateStatus,
  Session,
  SessionCopyReport,
  SessionLinksPreview,
  SessionSyncSelection,
  SwitchResult,
  TravelConfig,
  TravelStatus,
  UpdateInfo,
  UpdateSnapshot,
  VscodeExtStatus,
  VscodeExtSwitchResult,
  JetbrainsStatus,
  JetbrainsSwitchResult,
  VscodeSessionList,
  VscodeSessionRef,
  WbVariant,
} from "./types";
import { DEMO_UNAVAILABLE_MESSAGE, demoModeEnabled } from "./demo-mode";
import { screenshotDemoResponse } from "./screenshot-demo";

/**
 * 双通道适配层：
 * - 桌面 App（Tauri）：`invoke` 调用 Rust commands
 * - webui（浏览器）：HTTP fetch 调用本地 workbuddy-switch 服务（127.0.0.1）
 */
const API_BASE = "http://127.0.0.1:57890";

const DEMO_READ_COMMANDS = new Set([
  "get_status", "get_accounts", "get_codebuddy_cli_status", "get_codebuddy_cn_ide_status", "get_codebuddy_ide_status", "get_vscode_ext_status", "get_jetbrains_status", "list_vscode_sessions", "list_codebuddy_ide_sessions", "vscode_session_links_preview", "codebuddy_ide_session_links_preview", "get_checkin_status",
  "get_credit_expiry", "get_credit_statistics", "get_auto_checkin_config",
  "get_token_statistics",
  "get_checkin_logs", "get_auto_rotate_config", "rotate_status", "get_rotate_logs",
  "get_github_config", "check_update", "get_launch_at_login_enabled", "switch_progress",
  "get_travel_status", "get_auto_travel_config", "get_rate_limits",
  "get_rate_limit_hook_status", "get_rate_limit_config",
]);

export function isDemoMode(): boolean {
  return demoModeEnabled;
}

export function isWebui(): boolean {
  return typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);
}

/** Tauri mobile 也注入内部 API；用现有平台 UA 约定把桌面宿主与移动宿主区分开。 */
function isMobilePlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const ua = navigator.userAgent;
  return (
    /Android|iPhone|iPad|iPod/i.test(ua) ||
    (ua.includes("Macintosh") && navigator.maxTouchPoints > 1)
  );
}

/** 是否为提供桌面专属能力的 Tauri 宿主。 */
export function isDesktop(): boolean {
  return !isWebui() && !isMobilePlatform();
}

/** Agent Companion 只由桌面宿主管理，不能经 WebUI 或演示模式访问。 */
function requireCompanionDesktop(): void {
  if (demoModeEnabled) throw new Error(DEMO_UNAVAILABLE_MESSAGE);
  if (!isDesktop()) throw new Error("Agent Companion 仅在桌面版中可用");
}

type Route = { method: "GET" | "POST"; path: string };

/** Tauri command → HTTP 路由映射（webui 模式）。 */
const ROUTES: Record<string, Route> = {
  get_status: { method: "GET", path: "/api/status" },
  get_accounts: { method: "GET", path: "/api/accounts" },
  get_codebuddy_cli_status: { method: "GET", path: "/api/codebuddy-cli/status" },
  install_codebuddy_cli_helper: { method: "POST", path: "/api/codebuddy-cli/install-helper" },
  switch_codebuddy_cli_account: { method: "POST", path: "/api/codebuddy-cli/switch" },
  get_codebuddy_cn_ide_status: { method: "GET", path: "/api/codebuddy-cn-ide/status" },
  switch_codebuddy_cn_ide_account: { method: "POST", path: "/api/codebuddy-cn-ide/switch" },
  detect_codebuddy_cn_ide_account: { method: "POST", path: "/api/codebuddy-cn-ide/detect" },
  list_codebuddy_ide_sessions: { method: "GET", path: "/api/codebuddy-cn-ide/sessions" },
  codebuddy_ide_session_links_preview: {
    method: "POST",
    path: "/api/codebuddy-cn-ide/session-links",
  },
  get_vscode_ext_status: { method: "GET", path: "/api/vscode-ext/status" },
  get_jetbrains_status: { method: "GET", path: "/api/jetbrains/status" },
  switch_jetbrains_account: { method: "POST", path: "/api/jetbrains/switch" },
  detect_jetbrains_account: { method: "POST", path: "/api/jetbrains/detect" },
  list_vscode_sessions: { method: "GET", path: "/api/vscode-ext/sessions" },
  switch_vscode_ext_account: { method: "POST", path: "/api/vscode-ext/switch" },
  vscode_session_links_preview: { method: "POST", path: "/api/vscode-ext/session-links" },
  detect_vscode_ext_account: { method: "POST", path: "/api/vscode-ext/detect" },
  get_codebuddy_ide_status: { method: "GET", path: "/api/codebuddy-ide/status" },
  switch_codebuddy_ide_account: { method: "POST", path: "/api/codebuddy-ide/switch" },
  detect_codebuddy_ide_account: { method: "POST", path: "/api/codebuddy-ide/detect" },
  delete_account: { method: "POST", path: "/api/delete" },
  oauth_start: { method: "POST", path: "/api/oauth/start" },
  oauth_status: { method: "POST", path: "/api/oauth/status" },
  import_local: { method: "POST", path: "/api/import-local" },
  export_accounts: { method: "POST", path: "/api/export-accounts" },
  export_accounts_to_path: { method: "POST", path: "/api/export-accounts-to-path" },
  preview_import_accounts: { method: "POST", path: "/api/import/preview" },
  import_accounts: { method: "POST", path: "/api/import" },
  switch_account: { method: "POST", path: "/api/switch" },
  list_sessions: { method: "GET", path: "/api/sessions" },
  copy_sessions: { method: "POST", path: "/api/sessions/copy" },
  session_links_preview: { method: "POST", path: "/api/session-links/preview" },
  get_checkin_status: { method: "GET", path: "/api/checkin/status" },
  get_credit_expiry: { method: "POST", path: "/api/credits" },
  get_credit_statistics: { method: "GET", path: "/api/credits/stats" },
  get_token_statistics: { method: "GET", path: "/api/token-stats" },
  get_rate_limits: { method: "GET", path: "/api/rate-limits" },
  get_rate_limit_hook_status: { method: "GET", path: "/api/rate-limits/hook-status" },
  install_rate_limit_hook: { method: "POST", path: "/api/rate-limits/install-hook" },
  uninstall_rate_limit_hook: { method: "POST", path: "/api/rate-limits/uninstall-hook" },
  get_rate_limit_config: { method: "GET", path: "/api/rate-limits/config" },
  save_rate_limit_config: { method: "POST", path: "/api/rate-limits/config" },
  checkin: { method: "POST", path: "/api/checkin" },
  checkin_all: { method: "POST", path: "/api/checkin/all" },
  get_auto_checkin_config: { method: "GET", path: "/api/checkin/config" },
  save_auto_checkin_config: { method: "POST", path: "/api/checkin/config" },
  get_checkin_logs: { method: "GET", path: "/api/checkin/logs" },
  list_notifications: { method: "GET", path: "/api/notifications" },
  record_notification: { method: "POST", path: "/api/notifications/record" },
  clear_notifications: { method: "POST", path: "/api/notifications/clear" },
  get_travel_status: { method: "GET", path: "/api/travel/status" },
  get_auto_travel_config: { method: "GET", path: "/api/travel/config" },
  save_auto_travel_config: { method: "POST", path: "/api/travel/config" },
  get_auto_rotate_config: { method: "GET", path: "/api/rotate/config" },
  save_auto_rotate_config: { method: "POST", path: "/api/rotate/config" },
  rotate_status: { method: "GET", path: "/api/rotate/status" },
  run_rotate: { method: "POST", path: "/api/rotate/run" },
  get_rotate_logs: { method: "GET", path: "/api/rotate/logs" },
  refresh_account_token: { method: "POST", path: "/api/refresh-token" },
  get_github_config: { method: "GET", path: "/api/update/config" },
  save_github_config: { method: "POST", path: "/api/update/config" },
  check_update: { method: "GET", path: "/api/update/check" },
  switch_progress: { method: "GET", path: "/api/switch/progress" },
};

/**
 * 档位参数只在国际版时下发：缺省（国内版）保持改造前的请求体逐字一致，
 * Tauri 走 `invoke(cmd, undefined)`，HTTP 走无 query 的路径。
 */
function variantArgs(variant?: WbVariant): Record<string, unknown> | undefined {
  return variant === "ai" ? { variant } : undefined;
}

function queryString(args?: Record<string, unknown>): string {
  if (!args) return "";
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(args)) {
    if (value === undefined || value === null) continue;
    params.set(key, String(value));
  }
  const text = params.toString();
  return text ? `?${text}` : "";
}

async function httpCall<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const route = ROUTES[cmd];
  if (!route) throw new Error(`webui 模式暂不支持该操作: ${cmd}`);
  let res: Response;
  try {
    const url =
      route.method === "GET"
        ? `${API_BASE}${route.path}${queryString(args)}`
        : `${API_BASE}${route.path}`;
    res = await fetch(url, {
      method: route.method,
      headers: { "Content-Type": "application/json" },
      body: route.method === "POST" ? JSON.stringify(args ?? {}) : undefined,
    });
  } catch {
    throw new Error(`无法连接 workbuddy-switch 服务（${API_BASE}），请先运行 \`workbuddy-switch\``);
  }
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new Error(data.message || data.error || `请求失败 (${res.status})`);
  }
  return data as T;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (demoModeEnabled) {
    if (cmd === "get_credit_statistics" && args?.refresh === true) {
      throw new Error(DEMO_UNAVAILABLE_MESSAGE);
    }
    if (!DEMO_READ_COMMANDS.has(cmd)) throw new Error(DEMO_UNAVAILABLE_MESSAGE);
    return screenshotDemoResponse(cmd, args) as T;
  }
  if (!isWebui()) return invoke<T>(cmd, args);
  return httpCall<T>(cmd, args);
}

// ---------------------------------------------------------------------------
// 状态 / 账号
// ---------------------------------------------------------------------------

/** 运行状态 / 当前账号 / 应用路径；`variant` 缺省为国内版。 */
export function getStatus(variant?: WbVariant): Promise<AppStatus> {
  return call("get_status", variantArgs(variant));
}

/** 返回全部档位的账号，由调用方按 `variant` 过滤。 */
export function getAccounts(): Promise<{ accounts: AccountMeta[] }> {
  return call("get_accounts");
}

export function getCodebuddyCliStatus(): Promise<CodeBuddyCliStatus> {
  return call("get_codebuddy_cli_status");
}

export function installCodebuddyCliHelper(): Promise<CodeBuddyCliInstallResult> {
  return call("install_codebuddy_cli_helper");
}

/**
 * 切换 CodeBuddy CLI 默认账号。
 *
 * @param closeRunningCli 已废弃：后端一律先关闭正在运行的 CLI 再写状态，该入参被忽略。
 *   仅为兼容既有调用方保留（HTTP 路径仍会原样发送）。
 */
export function switchCodebuddyCliAccount(
  accountId: string,
  closeRunningCli = false,
): Promise<CodeBuddyCliSwitchResult> {
  if (demoModeEnabled) {
    return new Promise((resolve, reject) => {
      window.setTimeout(() => {
        try {
          resolve(
            screenshotDemoResponse("switch_codebuddy_cli_account", {
              accountId,
              closeRunningCli,
            }) as CodeBuddyCliSwitchResult,
          );
        } catch (error) {
          reject(error);
        }
      }, 1200);
    });
  }
  return call("switch_codebuddy_cli_account", { accountId, closeRunningCli });
}

export function getCodebuddyCnIdeStatus(): Promise<CodeBuddyCnIdeStatus> {
  return call("get_codebuddy_cn_ide_status");
}

/**
 * 切换 CodeBuddy IDE 账号（可同时复制 / 同步会话）。
 *
 * `restart` 默认 true：IDE 运行时由后端先关闭、写入后再重新打开。
 * `copySessions` 非空时切换前把勾选会话复制到目标账号（默认沿用会话 id，冲突才重随机）；
 * `syncSelections` 与 VS Code 侧同形；两者都不传时行为与纯切换逐字一致。
 */
export function switchCodebuddyCnIdeAccount(
  accountId: string,
  restart = true,
  copySessions?: VscodeSessionRef[],
  syncSelections?: SessionSyncSelection[],
): Promise<CodeBuddyCnIdeSwitchResult> {
  return call("switch_codebuddy_cn_ide_account", { accountId, restart, copySessions, syncSelections });
}

/** 列出当前 CodeBuddy IDE 账号可复制的会话（未登录/未安装时返回空列表）。 */
export function listCodebuddyIdeSessions(): Promise<VscodeSessionList> {
  return call("list_codebuddy_ide_sessions");
}

/**
 * 预览「当前 CodeBuddy IDE 账号 → 目标账号」可同步的关联会话。
 *
 * 只读：`defaultChecked` 与 `availableModes` 是勾选权限的唯一来源，前端不得自行扩大。
 */
export function codebuddyIdeSessionLinksPreview(
  targetAccountId: string,
): Promise<SessionLinksPreview> {
  return call("codebuddy_ide_session_links_preview", { targetAccountId });
}

export function detectCodebuddyCnIdeAccount(): Promise<{
  ok: boolean;
  found: boolean;
  matched?: boolean;
  accountId?: string;
  message?: string;
}> {
  return call("detect_codebuddy_cn_ide_account");
}

export function getVscodeExtStatus(): Promise<VscodeExtStatus> {
  return call("get_vscode_ext_status");
}

/** 列出当前 VS Code 扩展账号可复制的会话（未安装/未登录时返回空列表）。 */
export function listVscodeSessions(): Promise<VscodeSessionList> {
  return call("list_vscode_sessions");
}

/**
 * 预览「当前 VS Code CodeBuddy 插件账号 → 目标账号」可同步的关联会话。
 *
 * 只读：`defaultChecked` 与 `availableModes` 是勾选权限的唯一来源，前端不得自行扩大。
 */
export function vscodeSessionLinksPreview(targetAccountId: string): Promise<SessionLinksPreview> {
  return call("vscode_session_links_preview", { targetAccountId });
}

/**
 * 切换 VS Code CodeBuddy 扩展账号。
 *
 * `restart` 默认 true：VS Code 运行时由后端先优雅退出、写入后再重新打开；
 * 传 false 退回「请先完全退出 VS Code」的手动模式（不在编辑器中自动操作）。
 * `syncSelections` 与 WorkBuddy 侧同形；只传它（不传 `copySessions`）也能执行同步。
 */
export function switchVscodeExtAccount(
  accountId: string,
  restart = true,
  copySessions?: VscodeSessionRef[],
  syncSelections?: SessionSyncSelection[],
): Promise<VscodeExtSwitchResult> {
  return call("switch_vscode_ext_account", { accountId, restart, copySessions, syncSelections });
}

export function detectVscodeExtAccount(): Promise<{
  ok: boolean;
  found: boolean;
  matched?: boolean;
  accountId?: string;
  message?: string;
}> {
  return call("detect_vscode_ext_account");
}

export function getJetbrainsStatus(): Promise<JetbrainsStatus> {
  return call("get_jetbrains_status");
}

/**
 * 切换 JetBrains IDE（IDEA / PyCharm）CodeBuddy 插件账号。
 *
 * `restart` 默认 true：IDE 运行时由后端先优雅退出、写入后再重新打开；
 * 传 false 退回「请先完全退出 IDE」的手动模式（不在 IDE 中自动操作）。
 * `configDirs` 可选：目标配置目录名列表（如 ["PyCharm2026.2"]），缺省 / 空
 * = 全部装了插件的 IDE；非空时只写所选目录、只关闭/重开这些目录的运行实例。
 */
export function switchJetbrainsAccount(
  accountId: string,
  restart = true,
  configDirs?: string[],
): Promise<JetbrainsSwitchResult> {
  return call("switch_jetbrains_account", { accountId, restart, configDirs });
}

export function detectJetbrainsAccount(): Promise<{
  ok: boolean;
  found: boolean;
  matched?: boolean;
  accountId?: string;
  message?: string;
}> {
  return call("detect_jetbrains_account");
}

export function getCodebuddyIdeStatus(): Promise<CodeBuddyCnIdeStatus> {
  return call("get_codebuddy_ide_status");
}

export function switchCodebuddyIdeAccount(
  accountId: string,
  restart = true,
): Promise<CodeBuddyCnIdeSwitchResult> {
  return call("switch_codebuddy_ide_account", { accountId, restart });
}

export function detectCodebuddyIdeAccount(): Promise<{
  ok: boolean;
  found: boolean;
  matched?: boolean;
  accountId?: string;
  message?: string;
}> {
  return call("detect_codebuddy_ide_account");
}


export function deleteAccount(accountId: string): Promise<{ ok: boolean }> {
  return call("delete_account", { accountId });
}

/** 发起登录：国内版为扫码授权，国际版为浏览器 Web 登录授权；`variant` 缺省为国内版（档位由后端记忆，轮询无需再传）。 */
export function oauthStart(variant?: WbVariant): Promise<OAuthStartResult> {
  return call("oauth_start", variantArgs(variant));
}

export function oauthStatus(loginId: string): Promise<OAuthPollResult> {
  return call("oauth_status", { loginId });
}

/** 导入本机当前登录态；`variant` 缺省为国内版（对应各自的登录态文件）。 */
export function importLocal(variant?: WbVariant): Promise<{ ok: boolean; account: AccountMeta }> {
  return call("import_local", variantArgs(variant));
}

export function exportAccounts(accountIds: string[]): Promise<{ ok: boolean; accounts: AccountRecord[] }> {
  return call("export_accounts", { accountIds });
}

/** 桌面端：把完整记录写入用户选择的路径（系统保存对话框产物）。 */
export function exportAccountsToPath(
  accountIds: string[],
  path: string,
): Promise<{ ok: boolean; path: string }> {
  return call("export_accounts_to_path", { accountIds, path });
}

export function previewImportAccounts(
  fileText: string,
): Promise<{ accounts: ImportPreviewAccount[]; total: number }> {
  return call("preview_import_accounts", { fileText });
}

export function importAccounts(fileText: string, indexes: number[]): Promise<ImportResult> {
  return call("import_accounts", { fileText, indexes });
}

export function switchAccount(args: {
  accountId: string;
  restart?: boolean;
  shareSessions?: boolean;
  copySessionIds?: string[];
  syncSelections?: SessionSyncSelection[];
}): Promise<SwitchResult> {
  return call("switch_account", args as unknown as Record<string, unknown>);
}

/** 切换进度（webui 轮询用；桌面端走事件，此函数无副作用）。 */
export function switchProgress(): Promise<{ running: boolean; progress: string | null }> {
  return call("switch_progress");
}

/** 当前登录态的会话列表；`variant` 缺省为国内版。 */
export function listSessions(variant?: WbVariant): Promise<{
  sessions: Session[];
  current: string | null;
}> {
  return call("list_sessions", variantArgs(variant));
}

/** 把勾选会话复制到指定账号；返回 core 同形的复制报告（copied / alreadyLinked / errors）。 */
export function copySessions(
  targetAccountId: string,
  sessionIds: string[],
): Promise<SessionCopyReport & { variant?: WbVariant }> {
  return call("copy_sessions", { targetAccountId, sessionIds });
}

/**
 * 预览「当前账号 → 目标账号」可同步的关联会话（只读）。
 *
 * 默认勾选与可选模式都来自后端：前端只按 `defaultChecked` / `availableModes` 渲染，
 * 不自行扩大权限。`variant` 缺省由后端取目标账号自身档位。
 */
export function sessionLinksPreview(
  targetAccountId: string,
  variant?: WbVariant,
): Promise<SessionLinksPreview> {
  const args: Record<string, unknown> = { targetAccountId };
  if (variant === "ai") args.variant = variant;
  return call("session_links_preview", args);
}

/** 打开系统设置授权面板（桌面端专用；webui 模式由服务进程权限决定，无操作）。 */
export function openPermissionSettings(
  target?: "app_management" | "all_files",
): Promise<void> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (isWebui()) return Promise.resolve();
  return call("open_permission_settings", { target: target ?? "app_management" });
}

/** 权限自检：桌面端写探针（按档位写在对应登录态文件旁）；webui 模式由服务进程权限决定。 */
export function checkAuthPermission(variant?: WbVariant): Promise<{
  ok: boolean;
  message?: string;
  error?: string;
  dir?: string;
  hint?: string;
}> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (isWebui()) {
    return Promise.resolve({
      ok: true,
      message: "webui 模式由服务进程（终端启动）的权限决定，无需额外授权",
      hint: "",
    });
  }
  return call("check_auth_permission", variantArgs(variant));
}

/** 在 Finder 中显示当前 App（桌面端专用；webui 无操作）。 */
export function revealAppInFinder(): Promise<void> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (isWebui()) return Promise.resolve();
  return call("reveal_app_in_finder");
}

/** 后端持久化的悬浮栏启用状态；默认值由后端决定。 */
export function getCompanionEnabled(): Promise<boolean> {
  try {
    requireCompanionDesktop();
    return call("get_companion_enabled");
  } catch (error) {
    return Promise.reject(error);
  }
}

/** 返回后端确认的最终状态，不在前端单独持久化。 */
export function setCompanionEnabled(enabled: boolean): Promise<boolean> {
  try {
    requireCompanionDesktop();
    return call("set_companion_enabled", { enabled });
  } catch (error) {
    return Promise.reject(error);
  }
}

export function openCompanionSettings(): Promise<void> {
  try {
    requireCompanionDesktop();
    return call("open_companion_settings");
  } catch (error) {
    return Promise.reject(error);
  }
}

// ---------------------------------------------------------------------------
// 阶段 3：签到 + token 刷新
// ---------------------------------------------------------------------------

export async function getCheckinStatus(accountId: string): Promise<{
  ok: boolean;
  todayCheckedIn?: boolean;
  result?: string;
  reason?: string;
  error?: string;
  raw?: unknown;
  /** 该行所属档位（档位取账号自身）；缺省按国内版处理。 */
  variant?: WbVariant;
}> {
  // 两个宿主都只查询目标账号；Web 端不再为每个账号重复请求整份列表。
  return call("get_checkin_status", { accountId });
}

export function getCreditExpiry(accountId: string): Promise<CreditExpiry> {
  return call("get_credit_expiry", { accountId });
}

export function getCreditStatistics(refresh = false): Promise<CreditStatistics> {
  return call("get_credit_statistics", refresh ? { refresh: true } : undefined);
}

export function getTokenStatistics(days?: number): Promise<TokenStatistics> { return call("get_token_statistics", days ? { days } : undefined); }

/**
 * 模型限额台账：一次返回**全部账号**当前受限的模型与官方恢复时刻。
 *
 * 不传档位：扫描本身就是全局的（两档位各扫一遍）。后端把 hook 信号与日志扫描
 * 合并后返回，日志扫描按 5 分钟节流（`scannedAt` 是最近一次真实扫描时刻）。
 */
export function getRateLimits(): Promise<RateLimitsPayload> {
  return call("get_rate_limits");
}

/** 限额 hook 安装状态（脚本 + 三处客户端配置）。 */
export function getRateLimitHookStatus(): Promise<RateLimitHookStatus> {
  return call("get_rate_limit_hook_status");
}

/** 安装限额 hook（幂等、写前备份；返回安装后的状态）。 */
export function installRateLimitHook(): Promise<RateLimitHookStatus> {
  return call("install_rate_limit_hook");
}

/** 卸载限额 hook（移除注册条目并尽量逐字节还原配置）。 */
export function uninstallRateLimitHook(): Promise<RateLimitHookStatus> {
  return call("uninstall_rate_limit_hook");
}

/** 限额监听开关（关闭后不扫日志、不渲染限额 chip）。 */
export function getRateLimitConfig(): Promise<RateLimitConfig> {
  return call("get_rate_limit_config");
}

export function saveRateLimitConfig(config: RateLimitConfig): Promise<RateLimitConfig> {
  return call("save_rate_limit_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function checkin(accountId: string): Promise<CheckinResult> {
  return call("checkin", { accountId });
}

/**
 * 批量签到：不传档位时覆盖全部档位；显式传入时只处理该档位。
 * 关闭自动签到的账号会被跳过，并逐账号返回 skipped 原因（设置页与托盘同样遵守）。
 *
 * 这里**不能**用 `variantArgs`：`checkin_all` 的缺省语义是「全部档位」，国内版若
 * 缺省不传参，账号页在国内版 Tab 触发的批量签到会打到国际版账号。显式下发 `cn`
 * 与改造前等价（改造前账号库里只有国内版账号）。
 */
export function checkinAll(variant?: WbVariant): Promise<{
  accounts: { accountId: string; email: string; result: string; error?: string; inactive?: boolean; reason?: string }[];
  status?: string;
  reason?: string;
}> {
  return call("checkin_all", variant ? { variant } : {});
}

export function getAutoCheckinConfig(): Promise<CheckinConfig> {
  return call("get_auto_checkin_config");
}

export function saveAutoCheckinConfig(config: CheckinConfig): Promise<CheckinConfig> {
  return call("save_auto_checkin_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function getCheckinLogs(): Promise<{ logs: CheckinLog[] }> {
  return call("get_checkin_logs");
}

export async function getTravelStatus(accountId: string): Promise<TravelStatus> {
  if (demoModeEnabled) {
    return screenshotDemoResponse("get_travel_status", { accountId }) as TravelStatus;
  }
  if (isWebui()) {
    // webui 端为批量接口，按 accountId 过滤
    const all = await httpCall<{
      accounts: { accountId: string; email: string; label: TravelStatus["label"]; rewardCredit: number | null; locationName?: string | null; arriveAt?: number | null }[];
    }>("get_travel_status");
    const one = all.accounts.find((a) => a.accountId === accountId);
    return one
      ? { label: one.label, rewardCredit: one.rewardCredit, locationName: one.locationName ?? null, arriveAt: one.arriveAt ?? null }
      : { label: "untraveled", rewardCredit: null, locationName: null, arriveAt: null };
  }
  return call("get_travel_status", { accountId });
}

export function getAutoTravelConfig(): Promise<TravelConfig> {
  return call("get_auto_travel_config");
}

export function saveAutoTravelConfig(config: TravelConfig): Promise<TravelConfig> {
  return call("save_auto_travel_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function getAutoRotateConfig(): Promise<AutoRotateConfig> {
  return call("get_auto_rotate_config");
}

export function saveAutoRotateConfig(config: AutoRotateConfig): Promise<AutoRotateConfig> {
  return call("save_auto_rotate_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function getRotateStatus(): Promise<RotateStatus> {
  return call("rotate_status");
}

/**
 * 手动触发一次轮换检查。
 *
 * `notify`（可选）：因存活门控被推迟、但除门控外本来会切换时由 core 组装好的提示内容。
 * 桌面端由宿主（Rust）直接投递系统通知，这里只保留字段以描述完整返回契约；
 * 无头 server 不投递，调用方按需自行处理。
 */
export function runRotate(): Promise<{
  status: string;
  reason?: string;
  error?: string;
  to?: string;
  notify?: { title: string; body: string };
}> {
  return call("run_rotate");
}

export function getRotateLogs(): Promise<{ logs: RotateLog[] }> {
  return call("get_rotate_logs");
}

export function refreshAccountToken(accountId: string): Promise<AccountMeta> {
  return call("refresh_account_token", { accountId });
}

// ---------------------------------------------------------------------------
// 阶段 4：自动更新
// ---------------------------------------------------------------------------

export function getGithubConfig(): Promise<GithubConfig> {
  return call("get_github_config");
}

export function saveGithubConfig(config: GithubConfig): Promise<GithubConfig> {
  return call("save_github_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function checkUpdate(proxy?: string, force?: boolean): Promise<UpdateInfo> {
  return call("check_update", { proxy: proxy?.trim() || null, force: force ?? false });
}

export function relaunchApp(): Promise<void> {
  return call("relaunch_app");
}

// ---------------------------------------------------------------------------
// 统一更新服务（桌面端；`update-state` 事件是阶段与进度的唯一来源）
// ---------------------------------------------------------------------------

/** 浏览器 / 演示模式没有更新服务：与弹窗既有文案逐字一致。 */
const UPDATE_UNSUPPORTED_MESSAGE = "浏览器 webui 模式不能直接安装桌面更新包";

/**
 * 更新状态快照（前端首屏初始化；之后由 `update-state` 事件推送）。
 *
 * webui 没有更新服务、演示模式禁止真实下载，两者都回落到静态快照：
 * 演示模式给「有新版」态，保证演示页 / 截图里的升级入口与外链完整。
 */
export function updateState(): Promise<UpdateSnapshot> {
  if (demoModeEnabled) {
    // 复用只读演示数据的版本号，避免版本号在两处硬编码。
    const demo = screenshotDemoResponse("check_update") as UpdateInfo;
    return Promise.resolve({
      phase: "available",
      latest: demo.latest ?? null,
      percent: null,
      message: null,
      checkedAt: null,
    });
  }
  if (isWebui()) {
    return Promise.resolve({
      phase: "idle",
      latest: null,
      percent: null,
      message: null,
      checkedAt: null,
    });
  }
  return call("update_state");
}

/** 启动更新包下载（异步，立即返回；进度走 `update-state` 事件与托盘）。 */
export function updateDownload(): Promise<void> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (isWebui()) return Promise.reject(new Error(UPDATE_UNSUPPORTED_MESSAGE));
  return call<unknown>("update_download").then(() => undefined);
}

/** 安装已下载的更新包并重启（用户点「重启以完成升级」时调用）。 */
export function updateRestart(): Promise<void> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (isWebui()) return Promise.reject(new Error(UPDATE_UNSUPPORTED_MESSAGE));
  return call<unknown>("update_restart").then(() => undefined);
}

// ---------------------------------------------------------------------------
// 开机自启（仅桌面端；webui 不提供同名接口，卡片也不在 webui 渲染）
// ---------------------------------------------------------------------------

/** 查询系统当前的开机自启注册状态（桌面端）。 */
export function getLaunchAtLoginEnabled(): Promise<boolean> {
  if (demoModeEnabled) return call("get_launch_at_login_enabled");
  if (!isDesktop()) return Promise.resolve(false);
  return call("get_launch_at_login_enabled");
}

/** 注册 / 移除系统开机自启，返回回读后的权威状态（桌面端）。 */
export function setLaunchAtLoginEnabled(enabled: boolean): Promise<boolean> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (!isDesktop()) return Promise.resolve(false);
  return call("set_launch_at_login_enabled", { enabled });
}

/** 把 Tauri command / HTTP 抛出的错误统一为 Error。 */
export function asError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e ?? "未知错误");
}

// ---------------------------------------------------------------------------
// 通知存档（toast 事后可查）
// ---------------------------------------------------------------------------

/** 记录一条应用内提示（由 `lib/notify.ts` 统一调用；失败不影响提示本身）。 */
export function recordNotification(
  level: AppNotification["level"],
  title: string,
  description?: string,
): Promise<{ recorded: boolean }> {
  if (demoModeEnabled) return Promise.resolve({ recorded: false });
  return call("record_notification", { level, title, description });
}

/** 读取最近的通知（新的在前，最多 100 条）。 */
export function listNotifications(): Promise<{ items: AppNotification[] }> {
  if (demoModeEnabled) return Promise.resolve({ items: [] });
  return call("list_notifications");
}

/** 清空通知存档。 */
export function clearNotifications(): Promise<{ cleared: boolean }> {
  if (demoModeEnabled) return Promise.resolve({ cleared: false });
  return call("clear_notifications");
}

// ---------------------------------------------------------------------------
// 错误日志（前端崩溃 / 未捕获错误落盘，见 lib/error-report.ts）
// ---------------------------------------------------------------------------

/**
 * 上报一条错误到本地错误日志（桌面端落盘 `~/.wb-switch/error.log`）。
 *
 * webui / 演示模式没有落盘通道：静默忽略（调用方的本地提示不受影响）。
 */
export function logError(kind: ErrorLogKind, message: string, detail?: string): Promise<void> {
  if (demoModeEnabled || isWebui()) return Promise.resolve();
  return call<unknown>("log_error", { kind, message, detail: detail ?? null }).then(
    () => undefined,
  );
}

/** 错误日志文件路径（设置页展示）。 */
export function getErrorLogPath(): Promise<string> {
  // 演示模式给一条与其它演示路径同风格的值，保证演示页 / 截图里界面完整。
  if (demoModeEnabled) return Promise.resolve("/demo/.wb-switch/error.log");
  // webui 没有落盘通道（不写服务端日志），设置页不展示路径。
  if (isWebui()) return Promise.resolve("");
  return call<string>("get_error_log_path");
}

/** 在文件管理器中定位错误日志（桌面端；日志尚未生成时由后端打开所在目录）。 */
export function revealErrorLog(): Promise<void> {
  if (demoModeEnabled || isWebui()) return Promise.resolve();
  return call<unknown>("reveal_error_log").then(() => undefined);
}
