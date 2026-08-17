import { invoke } from "@tauri-apps/api/core";
import type {
  AccountMeta,
  AppStatus,
  CheckinConfig,
  CheckinLog,
  CheckinResult,
  CopyResult,
  GithubConfig,
  ManualAddArgs,
  OAuthPollResult,
  OAuthStartResult,
  Session,
  SwitchResult,
  UpdateInfo,
} from "./types";

/**
 * 双通道适配层：
 * - 桌面 App（Tauri）：`invoke` 调用 Rust commands
 * - webui（浏览器）：HTTP fetch 调用本地 workbuddy-switch 服务（127.0.0.1）
 */
const API_BASE = "http://127.0.0.1:57890";

export function isWebui(): boolean {
  return typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);
}

type Route = { method: "GET" | "POST"; path: string };

/** Tauri command → HTTP 路由映射（webui 模式）。 */
const ROUTES: Record<string, Route> = {
  get_status: { method: "GET", path: "/api/status" },
  get_accounts: { method: "GET", path: "/api/accounts" },
  delete_account: { method: "POST", path: "/api/delete" },
  oauth_start: { method: "POST", path: "/api/oauth/start" },
  oauth_status: { method: "POST", path: "/api/oauth/status" },
  import_local: { method: "POST", path: "/api/import-local" },
  manual_add: { method: "POST", path: "/api/manual-add" },
  switch_account: { method: "POST", path: "/api/switch" },
  list_sessions: { method: "GET", path: "/api/sessions" },
  copy_sessions: { method: "POST", path: "/api/sessions/copy" },
  get_checkin_status: { method: "GET", path: "/api/checkin/status" },
  checkin: { method: "POST", path: "/api/checkin" },
  checkin_all: { method: "POST", path: "/api/checkin/all" },
  get_auto_checkin_config: { method: "GET", path: "/api/checkin/config" },
  save_auto_checkin_config: { method: "POST", path: "/api/checkin/config" },
  get_checkin_logs: { method: "GET", path: "/api/checkin/logs" },
  refresh_account_token: { method: "POST", path: "/api/refresh-token" },
  get_github_config: { method: "GET", path: "/api/update/config" },
  save_github_config: { method: "POST", path: "/api/update/config" },
  check_update: { method: "GET", path: "/api/update/check" },
  switch_progress: { method: "GET", path: "/api/switch/progress" },
};

async function httpCall<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const route = ROUTES[cmd];
  if (!route) throw new Error(`webui 模式暂不支持该操作: ${cmd}`);
  let res: Response;
  try {
    res = await fetch(`${API_BASE}${route.path}`, {
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
  if (!isWebui()) return invoke<T>(cmd, args);
  return httpCall<T>(cmd, args);
}

// ---------------------------------------------------------------------------
// 状态 / 账号
// ---------------------------------------------------------------------------

export function getStatus(): Promise<AppStatus> {
  return call("get_status");
}

export function getAccounts(): Promise<{ accounts: AccountMeta[] }> {
  return call("get_accounts");
}

export function deleteAccount(accountId: string): Promise<{ ok: boolean }> {
  return call("delete_account", { accountId });
}

export function oauthStart(): Promise<OAuthStartResult> {
  return call("oauth_start");
}

export function oauthStatus(loginId: string): Promise<OAuthPollResult> {
  return call("oauth_status", { loginId });
}

export function importLocal(): Promise<{ ok: boolean; account: AccountMeta }> {
  return call("import_local");
}

export function manualAdd(args: ManualAddArgs): Promise<{ ok: boolean; account: AccountMeta }> {
  return call("manual_add", args as unknown as Record<string, unknown>);
}

export function switchAccount(args: {
  accountId: string;
  restart?: boolean;
  shareSessions?: boolean;
  copySessionIds?: string[];
}): Promise<SwitchResult> {
  return call("switch_account", args as unknown as Record<string, unknown>);
}

/** 切换进度（webui 轮询用；桌面端走事件，此函数无副作用）。 */
export function switchProgress(): Promise<{ running: boolean; progress: string | null }> {
  return call("switch_progress");
}

export function listSessions(): Promise<{
  sessions: Session[];
  current: string | null;
}> {
  return call("list_sessions");
}

export function copySessions(
  targetAccountId: string,
  sessionIds: string[],
): Promise<{ sourceUid: string; targetUid: string; copied: CopyResult[] }> {
  return call("copy_sessions", { targetAccountId, sessionIds });
}

/** 打开系统设置授权面板（桌面端专用；webui 模式由服务进程权限决定，无操作）。 */
export function openPermissionSettings(
  target?: "app_management" | "all_files",
): Promise<void> {
  if (isWebui()) return Promise.resolve();
  return call("open_permission_settings", { target: target ?? "app_management" });
}

/** 权限自检：桌面端写探针；webui 模式由服务进程权限决定。 */
export function checkAuthPermission(): Promise<{
  ok: boolean;
  message?: string;
  error?: string;
  dir?: string;
  hint?: string;
}> {
  if (isWebui()) {
    return Promise.resolve({
      ok: true,
      message: "webui 模式由服务进程（终端启动）的权限决定，无需额外授权",
      hint: "",
    });
  }
  return call("check_auth_permission");
}

/** 在 Finder 中显示当前 App（桌面端专用；webui 无操作）。 */
export function revealAppInFinder(): Promise<void> {
  if (isWebui()) return Promise.resolve();
  return call("reveal_app_in_finder");
}

// ---------------------------------------------------------------------------
// 阶段 3：签到 + token 刷新
// ---------------------------------------------------------------------------

export async function getCheckinStatus(accountId: string): Promise<{
  ok: boolean;
  todayCheckedIn: boolean;
  error?: string;
  raw?: unknown;
}> {
  if (isWebui()) {
    // webui 端为批量接口，按 accountId 过滤
    const all = await httpCall<{
      accounts: {
        accountId: string;
        email: string;
        todayCheckedIn: boolean;
        error?: string;
      }[];
    }>("get_checkin_status");
    const one = all.accounts.find((a) => a.accountId === accountId);
    return one
      ? { ok: true, todayCheckedIn: one.todayCheckedIn, error: one.error }
      : { ok: false, todayCheckedIn: false, error: "未找到账号" };
  }
  return call("get_checkin_status", { accountId });
}

export function checkin(accountId: string): Promise<CheckinResult> {
  return call("checkin", { accountId });
}

export function checkinAll(): Promise<{
  accounts: { accountId: string; email: string; result: string; error?: string }[];
}> {
  return call("checkin_all");
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

export function checkUpdate(proxy?: string): Promise<UpdateInfo> {
  return call("check_update", { proxy: proxy?.trim() || null });
}

export function relaunchApp(): Promise<void> {
  return call("relaunch_app");
}

/** 把 Tauri command / HTTP 抛出的错误统一为 Error。 */
export function asError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e ?? "未知错误");
}
