// 与 Rust 后端命令返回结构对齐的类型定义（对照 server.py 各 API 响应）

/**
 * WorkBuddy 客户端档位：国内版（cn）/ 国际版（ai）。
 * 后端以字符串返回，历史数据与旧响应可能缺省该字段，读取时统一按国内版处理。
 */
export type WbVariant = "cn" | "ai";

export interface AccountMeta {
  id: string;
  uid: string | null;
  email: string | null;
  nickname: string | null;
  enterpriseName: string | null;
  expiresAt: number | null;
  refreshExpiresAt: number | null;
  refreshedAt: number | null;
  createdAt: number | null;
  needsRelogin: boolean;
  needsReloginReason: string | null;
  /** 账号所属档位；缺省（旧后端/历史账号）按国内版处理。 */
  variant?: WbVariant;
}

export interface AppStatus {
  running: boolean;
  authFile: string;
  current: {
    uid: string | null;
    nickname: string | null;
    email: string | null;
  } | null;
  appPath: string;
  version: string;
  /** 上述字段所属档位；缺省按国内版处理。 */
  variant?: WbVariant;
}

export interface OAuthStartResult {
  loginId: string;
  verificationUri: string;
  expiresIn: number;
}

export interface OAuthPollResult {
  done: boolean;
  result?: AccountMeta;
  error?: string;
}

/** 导出文件中的完整账号记录（含 token，仅导出命令返回；字段与账号库原始记录一致）。 */
export interface AccountRecord {
  id?: string;
  uid?: string | null;
  nickname?: string | null;
  email?: string | null;
  access_token?: string | null;
  refresh_token?: string | null;
  token_type?: string | null;
  domain?: string | null;
  expiresAt?: number | null;
  refreshExpiresAt?: number | null;
  auth_raw?: unknown;
  profile_raw?: unknown;
  createdAt?: number | null;
  [key: string]: unknown;
}

/** 导入文件账号的脱敏预览（不含 token）。 */
export interface ImportPreviewAccount {
  index: number;
  uid: string | null;
  nickname: string | null;
  email: string | null;
  hasToken: boolean;
}

/** 导入结果计数。 */
export interface ImportResult {
  ok: boolean;
  imported: number;
  skipped: number;
  overwritten: number;
}

export interface Session {
  id: string;
  title: string;
  cwd: string;
  updatedAt: number;
  hasHistory: boolean;
  /** WorkBuddy playground（侧栏「任务」）；缺省视为空间会话。 */
  isPlayground?: boolean;
}

export interface CopyResult {
  id: string;
  newId: string;
  jsonlCopied: boolean;
  mappingWritten: boolean;
  backup: string;
}

/** 切换时的会话复制报告；复制失败时后端只回 `error`（切换本身仍继续）。 */
export interface SessionCopyReport {
  sourceUid: string;
  targetUid: string;
  /** 错误分支不返回该字段：后端只给 `{ error }`。 */
  copied?: CopyResult[];
  errors?: { id: string; error: string }[];
  error?: string;
}

export interface SwitchResult {
  ok: boolean;
  account: string;
  /** 目标账号自身档位；缺省按国内版处理。 */
  variant?: WbVariant;
  backup: string | null;
  sessionCopy?: SessionCopyReport;
  /** 「增量硬链接共享」报告：源账号会话零拷贝共享给目标账号（inode 判重）。 */
  autoLink?: {
    sourceUid: string;
    targetUid: string;
    dryRun?: boolean;
    copied: { id: string; newId: string; planned?: boolean }[];
    alreadyCopied: number;
    /** 名单外（源侧不动，只计数）。 */
    beyondKeep: number;
    /** 统一保留名单（目标侧 sid）。 */
    keepTargetSids?: string[];
    clawSkipped: number;
    noBody: number;
    errors?: { id: string; error: string }[];
    backupDb?: string | null;
  };
}

export interface CheckinConfig {
  enabled: boolean;
  /** Legacy persisted fields; accepted by the backend but ignored by scheduling. */
  start_hour?: number;
  end_hour?: number;
  keepalive_days: number;
  lazy_refresh_hours: number;
}

export interface CheckinLog {
  ts: number;
  accountId: string | null;
  email: string;
  result: string;
  error?: string;
  /** 该行所属档位；历史日志缺省按国内版处理。 */
  variant?: WbVariant;
}

export interface CheckinResult {
  result: string;
  error?: string;
  /** 国际版签到活动未开放时的业务判定；不写成功日志、不计入失败重试。 */
  inactive?: boolean;
}

export interface TravelConfig {
  enabled: boolean;
}

export type TravelStatusLabel = "untraveled" | "no-buddy" | "traveling" | "finished";

export interface TravelStatus {
  label: TravelStatusLabel;
  rewardCredit: number | null;
  locationName?: string | null;
  arriveAt?: number | null;
}

/** 单个受限模型；`model` 为 null 表示日志里归因不到模型（显示「未知模型」，不猜测）。 */
export interface RateLimitEntry {
  model: string | null;
  /** 官方日志原文给出的恢复时刻（毫秒）。 */
  resetAt: number;
  /** 该事件首次出现的时刻（毫秒）。 */
  firstSeenAt: number;
  /** 去重前的原始命中行数（调试/排查用）。 */
  hitCount: number;
}

/** 一个账号当前受限的全部模型（按 `resetAt` 升序）。 */
export interface AccountRateLimits {
  accountId: string;
  limited: RateLimitEntry[];
}

/** 模型限额台账：一次返回全部账号的当前受限状态（数据来自本机日志）。 */
export interface RateLimitsPayload {
  scannedAt: number;
  /** 固定 2 天，回显便于调试。 */
  windowDays: number;
  /** 只包含至少有一个受限模型的账号。 */
  accounts: AccountRateLimits[];
}

/** 一处客户端 hook 配置的安装状态。 */
export interface RateLimitHookTarget {
  /** 备份标签（codebuddy / workbuddy / workbuddy-ai）。 */
  label: string;
  /** `settings.json` 路径。 */
  path: string;
  /** 该客户端数据根目录是否存在（唯一的存在性判据；不存在则不参与安装）。 */
  exists: boolean;
  /** 该配置里是否已注册本工具的 Stop / FinalStop。 */
  installed: boolean;
}

/**
 * 限额 hook 安装状态：脚本 + 三处客户端配置逐项结果。
 *
 * `installed` = 脚本存在且至少一处配置注册成功；`lastEventAt` 是最近一次由后端
 * 入账的 hook 限额事件时刻（null = 从未收到）。
 */
export interface RateLimitHookStatus {
  scriptPath: string;
  scriptExists: boolean;
  eventsPath: string;
  installed: boolean;
  lastEventAt?: number | null;
  targets: RateLimitHookTarget[];
}

/** 限额监听开关（`~/.wb-switch/rate_limit_config.json`）。 */
export interface RateLimitConfig {
  enabled: boolean;
  /** 用户点过「卸载 hook」→ 启动时不再自动接入；重新点「接入 hook」清除。 */
  hookOptOut: boolean;
  /**
   * 是否扫描两个 CodeBuddy IDE 的日志（默认 true）。
   * IDE 的 429 不触发任何 hook 事件，日志是它唯一的数据源；关闭只影响 IDE 两源，
   * CLI / WorkBuddy 的 hook 实时上报与未接 hook 时的日志兜底不变。
   */
  scanIdeLogs: boolean;
}

export interface AutoRotateConfig {
  enabled: boolean;
  check_interval_minutes: number;
  cooldown_minutes: number;
  min_gap_hours: number;
  min_urgency_hours: number;
  /** 配置键兼容保留：轮换已改用「会话存活门控」，该值不再参与决策，设置页也不再展示。 */
  active_guard_minutes: number;
  min_remaining_credits: number;
}

export interface RotateLog {
  ts: number;
  action: string;
  reason?: string | null;
  from?: { id: string; name?: string | null } | null;
  to?: { id: string; name?: string | null } | null;
}

export interface RotateStatus {
  config: AutoRotateConfig;
  cliConfigured: boolean;
  activeAccountId: string | null;
  activeAccountName: string | null;
  lastCheckAt: number | null;
  lastSwitchAt: number | null;
}

export interface CreditResource {
  packageCode: string | null;
  packageName: string | null;
  total: number;
  remaining: number;
  used: number;
  status: number | null;
  expireAt: number | null;
  expired: boolean;
  expiringSoon: boolean;
}

export interface CreditExpiry {
  ok: boolean;
  accountId?: string | null;
  accountName?: string;
  updatedAt?: number;
  totalCapacity?: number;
  totalRemaining?: number;
  expiringSoonRemaining?: number;
  expiredRemaining?: number;
  soonestExpireAt?: number | null;
  expiringSoon?: boolean;
  expired?: boolean;
  resources?: CreditResource[];
  error?: string;
}

export interface CreditStatsSummary {
  currentRemaining: number;
  currentCapacity: number;
  usageToday: number;
  usage7Days: number;
  usageThisMonth: number;
  todayCheckedInAccounts: number;
  todaySuccess: number;
  todayAlready: number;
  todayFailed: number;
}

export interface CreditStatsDailyPoint {
  date: string;
  usage: number;
  /** 官方用量按模型聚合（全量，不受请求明细条数限制）；本地观察口径下为空 */
  models?: { model: string; requestCount: number; credit: number }[];
}

export interface CreditStatsAccount {
  accountId: string;
  accountName: string;
  isCurrent: boolean;
  currentRemaining: number | null;
  totalCapacity: number | null;
  lastSnapshotAt: number | null;
  usageToday: number;
  usage7Days: number;
  usageThisMonth: number;
  checkedInToday: boolean | null;
  checkinStatusToday: string | null;
  lastCheckinAt: number | null;
  lastCheckinResult: string | null;
  /** 按账号的逐日观察消耗（缺省兼容旧后端）；官方可用时趋势图优先使用官方 daily */
  daily?: CreditStatsDailyPoint[];
  /** 档位标记。后端当前不下发，前端容忍性读取；缺省时回退到按 accountId 的映射表 */
  variant?: WbVariant;
}

export interface CreditStatsUsageEvent {
  kind: "usage";
  ts: number;
  date: string;
  accountId: string;
  accountName: string;
  amount: number;
  /** 档位标记。后端当前不下发，前端容忍性读取；缺省时回退到按 accountId 的映射表 */
  variant?: WbVariant;
}

export interface CreditStatsCheckinEvent {
  kind: "checkin";
  ts: number;
  date: string;
  accountId: string | null;
  accountName: string;
  result: string;
  error?: string | null;
  /** 档位标记。后端当前不下发，前端容忍性读取；缺省时回退到按 accountId 的映射表 */
  variant?: WbVariant;
}

export type CreditStatsEvent = CreditStatsUsageEvent | CreditStatsCheckinEvent;

export type CreditOfficialUsageStatus = "complete" | "partial" | "unavailable";

export interface CreditOfficialUsageSummary {
  usageToday: number;
  usage7Days: number;
  usageThisMonth: number;
}

export interface CreditOfficialUsageModel {
  model: string;
  requestCount: number;
  credit: number;
}

export interface CreditOfficialUsageAccount {
  accountId: string;
  accountName: string;
  ok: boolean;
  requestCount: number;
  detailTruncated: boolean;
  usageToday: number | null;
  usage7Days: number | null;
  usageThisMonth: number | null;
  error?: string | null;
  reportedTotal?: number | null;
  fetchedCount?: number;
  /** 缺省兼容旧后端响应。 */
  models?: CreditOfficialUsageModel[];
  /** 按账号的逐日官方消耗（全量聚合，不受 requests 明细上限影响；缺省兼容旧后端） */
  daily?: CreditStatsDailyPoint[];
}

export interface CreditOfficialUsageRequest {
  accountId: string;
  accountName: string;
  requestId: string;
  credit: number;
  model: string;
  client: string;
  requestTime: string;
}

export interface CreditOfficialUsageError {
  accountId: string;
  accountName: string;
  error: string;
}

export interface CreditOfficialUsage {
  status: CreditOfficialUsageStatus;
  rangeStart: string;
  rangeEnd: string;
  /** 官方用量最近一次采集时间；缓存命中时保持采集当时的时间。 */
  collectedAt?: number;
  summary: CreditOfficialUsageSummary;
  daily: CreditStatsDailyPoint[];
  accounts: CreditOfficialUsageAccount[];
  requests: CreditOfficialUsageRequest[];
  /** 官方全部有效请求按模型汇总；不受 requests 明细上限影响。 */
  models?: CreditOfficialUsageModel[];
  detailLimitPerAccount: number;
  errors: CreditOfficialUsageError[];
}

export interface CreditStatistics {
  generatedAt: number;
  retentionDays: number;
  coverageStartAt: number | null;
  summary: CreditStatsSummary;
  daily: CreditStatsDailyPoint[];
  accounts: CreditStatsAccount[];
  events: CreditStatsEvent[];
  /** 官方接口不可用时仍使用上述本地观察字段；缺省兼容旧后端。 */
  officialUsage?: CreditOfficialUsage;
}

export interface TokenStatsTotals { total: number; input: number; output: number; cacheRead: number; cacheWrite: number; uncachedInput: number; records: number; cacheHitRate: number | null; }
export interface TokenStatsGroup extends TokenStatsTotals { key: string; title?: string | null; project?: string; sessionId?: string; }
/** 一次模型调用的明细行；`total = input + output + cacheWrite`，`uncachedInput = max(0, input - cacheRead)`，`thinking` 是 `output` 中思考过程的 token 数（回复内容 = max(0, output - thinking)），均与聚合口径一致。 */
export interface TokenStatsRequestRow { timestamp: number; model: string; project: string; sessionId: string; title?: string | null; input: number; output: number; cacheRead: number; cacheWrite: number; uncachedInput: number; thinking: number; total: number; }
/** `workbuddy-ai` 为国际版本地数据源，与国内版分开统计，数据源缺失时为空集。 */
export interface TokenStatsSource { source: "workbuddy" | "workbuddy-ai" | "codebuddy-cli" | "codebuddy-ide"; summary: TokenStatsTotals; models: TokenStatsGroup[]; projects: TokenStatsGroup[]; sessions: TokenStatsGroup[]; daily: TokenStatsGroup[]; /** Optional model-specific daily series for trend filtering. */ dailyByModel?: Record<string, TokenStatsGroup[]>; /** 仅 CodeBuddy CLI 来源返回的最近请求明细；旧后端或缺失时按空数组处理。 */ requests?: TokenStatsRequestRow[]; hours: TokenStatsGroup[]; filesScanned: number; parseErrors: number; coverageStartAt?: number | null; coverageEndAt?: number | null; }
export interface TokenStatistics { generatedAt: number; rangeDays?: number | null; sources: TokenStatsSource[]; }

export interface CodeBuddyCliStatus {
  configured: boolean;
  authMode?: "settings-env" | "api-key-helper";
  environmentOverride?: boolean;
  settingsPresent: boolean;
  helperPresent: boolean;
  helperSupportsAccountIds: boolean;
  helperCurrent?: boolean;
  migrationRequired?: boolean;
  syncPending?: boolean;
  activeIndex: number | null;
  activeAccountId: string | null;
  activeAccountName: string | null;
  /** 当前 CLI 账号所属档位；尚未接入时缺省。 */
  activeAccountVariant?: WbVariant | null;
  accountCount: number;
  statePath: string;
}

export interface CodeBuddyCliSwitchResult {
  ok: boolean;
  configured: boolean;
  synced: boolean;
  verified?: boolean;
  authMode?: "settings-env" | "api-key-helper";
  activeIndex?: number;
  activeAccountId?: string;
  source?: string;
  skipped?: boolean;
  regionChanged?: boolean;
  cliClosed?: boolean;
  closedProcessCount?: number;
  message?: string;
  error?: string;
}

export interface CodeBuddyCliInstallResult {
  ok: boolean;
  configured: boolean;
  helperPresent: boolean;
  helperSupportsAccountIds: boolean;
  verified?: boolean;
  authMode?: "settings-env" | "api-key-helper";
  message?: string;
  error?: string;
}

export interface GithubConfig {
  owner?: string;
  repo?: string;
  proxy?: string;
}

export interface UpdateInfo {
  ok: boolean;
  current?: string;
  latest?: string;
  latestTag?: string;
  hasUpdate?: boolean;
  releaseName?: string;
  releaseUrl?: string;
  publishedAt?: string;
  error?: string;
  message?: string;
}

/** CodeBuddy CN IDE（桌面客户端）状态；与 CodeBuddy CLI 独立。 */
export interface CodeBuddyCnIdeStatus {
  installed: boolean;
  running: boolean;
  dataDir: string | null;
  dbPath: string | null;
  dbExists: boolean;
  appPath: string | null;
  activeAccountId: string | null;
  activeAccountName: string | null;
  detectedFrom?: string;
  statePath?: string;
}

export interface CodeBuddyCnIdeSwitchResult {
  ok: boolean;
  account: string;
  accountId: string;
  dbPath?: string;
  restarted?: boolean;
  message?: string;
}

