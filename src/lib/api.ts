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

// 对应 Rust commands（阶段 1）

export function getStatus(): Promise<AppStatus> {
  return invoke("get_status");
}

export function getAccounts(): Promise<{ accounts: AccountMeta[] }> {
  return invoke("get_accounts");
}

export function deleteAccount(accountId: string): Promise<{ ok: boolean }> {
  return invoke("delete_account", { accountId });
}

export function oauthStart(): Promise<OAuthStartResult> {
  return invoke("oauth_start");
}

export function oauthStatus(loginId: string): Promise<OAuthPollResult> {
  return invoke("oauth_status", { loginId });
}

export function importLocal(): Promise<{ ok: boolean; account: AccountMeta }> {
  return invoke("import_local");
}

export function manualAdd(args: ManualAddArgs): Promise<{ ok: boolean; account: AccountMeta }> {
  return invoke("manual_add", args as unknown as Record<string, unknown>);
}

export function switchAccount(args: {
  accountId: string;
  restart?: boolean;
  shareSessions?: boolean;
  copySessionIds?: string[];
}): Promise<SwitchResult> {
  return invoke("switch_account", args as unknown as Record<string, unknown>);
}

export function listSessions(): Promise<{
  sessions: Session[];
  current: string | null;
}> {
  return invoke("list_sessions");
}

export function copySessions(
  targetAccountId: string,
  sessionIds: string[],
): Promise<{ sourceUid: string; targetUid: string; copied: CopyResult[] }> {
  return invoke("copy_sessions", { targetAccountId, sessionIds });
}

/** 打开系统设置授权面板（App 管理 + 完全磁盘访问）。 */
export function openPermissionSettings(): Promise<void> {
  return invoke("open_permission_settings");
}

/** 权限自检：确认认证目录是否可写（完全磁盘访问是否生效）。 */
export function checkAuthPermission(): Promise<{
  ok: boolean;
  message?: string;
  error?: string;
  dir?: string;
  hint?: string;
}> {
  return invoke("check_auth_permission");
}

/** 在 Finder 中显示当前 App（便于拖拽授权）。 */
export function revealAppInFinder(): Promise<void> {
  return invoke("reveal_app_in_finder");
}

// ---------------------------------------------------------------------------
// 阶段 3：签到 + token 刷新
// ---------------------------------------------------------------------------

export function getCheckinStatus(accountId: string): Promise<{
  ok: boolean;
  todayCheckedIn: boolean;
  error?: string;
  raw?: unknown;
}> {
  return invoke("get_checkin_status", { accountId });
}

export function checkin(accountId: string): Promise<CheckinResult> {
  return invoke("checkin", { accountId });
}

export function checkinAll(): Promise<{
  accounts: { accountId: string; email: string; result: string; error?: string }[];
}> {
  return invoke("checkin_all");
}

export function getAutoCheckinConfig(): Promise<CheckinConfig> {
  return invoke("get_auto_checkin_config");
}

export function saveAutoCheckinConfig(config: CheckinConfig): Promise<CheckinConfig> {
  return invoke("save_auto_checkin_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function getCheckinLogs(): Promise<{ logs: CheckinLog[] }> {
  return invoke("get_checkin_logs");
}

export function refreshAccountToken(accountId: string): Promise<AccountMeta> {
  return invoke("refresh_account_token", { accountId });
}

// ---------------------------------------------------------------------------
// 阶段 4：自动更新
// ---------------------------------------------------------------------------

export function getGithubConfig(): Promise<GithubConfig> {
  return invoke("get_github_config");
}

export function saveGithubConfig(config: GithubConfig): Promise<GithubConfig> {
  return invoke("save_github_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function checkUpdate(): Promise<UpdateInfo> {
  return invoke("check_update");
}

/** 把 Tauri command 抛出的字符串错误统一为 Error。 */
export function asError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e ?? "未知错误");
}
