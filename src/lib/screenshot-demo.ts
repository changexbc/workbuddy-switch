import type {
  AccountMeta, AppStatus, AutoRotateConfig, CheckinConfig, CheckinLog,
  CodeBuddyCliStatus, CodeBuddyCliSwitchResult, CodeBuddyCnIdeStatus, CreditExpiry, CreditOfficialUsageModel, CreditStatistics,
  GithubConfig, RateLimitHookStatus, RateLimitsPayload, RotateLog, RotateStatus, TokenStatistics, TokenStatsGroup, TokenStatsRequestRow, TokenStatsSource, TokenStatsTotals,
  TravelConfig, TravelStatus, VscodeExtStatus, VscodeSessionList,
} from "./types";
import { demoModeEnabled } from "./demo-mode";
import { accountVariant, normalizeVariant } from "./variant";

export const screenshotDemoEnabled = demoModeEnabled;

const MODEL_NAMES = ["deepseek-v4-flash", "kimi-k3-1", "deepseek-v4-pro", "glm-5.2", "hy3"] as const;

interface ModelSeed {
  model: (typeof MODEL_NAMES)[number];
  requestCount: number;
  credit: number;
}

interface AccountUsageSeed {
  requestCount: number;
  models: ModelSeed[];
}

// 演示数据只覆盖国内版；切换国际版时展示的是空状态（不构造国际版演示账号）。
const accounts: AccountMeta[] = [
  { id: "demo-account-a", uid: "demo-user-001", email: "test-a@example.com", nickname: "测试 A", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "cn" },
  { id: "demo-account-b", uid: "demo-user-002", email: "test-b@example.com", nickname: "测试 B", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "cn" },
  { id: "demo-account-c", uid: "demo-user-003", email: "test-c@example.com", nickname: "测试 C", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "cn" },
];

/** 演示模式中的临时 CLI 当前账号，仅存在于本次页面会话。 */
let demoActiveCliAccountId = accounts[0].id;

// Counts and relative model roles follow anonymous aggregates from the sanitized local cache.
// No upstream request row or identifier is copied into this fixture.
const usageSeeds: AccountUsageSeed[] = [
  {
    requestCount: 2243,
    models: [
      { model: "deepseek-v4-flash", requestCount: 2133, credit: 1794.39 },
      { model: "kimi-k3-1", requestCount: 24, credit: 2497.16 },
      { model: "deepseek-v4-pro", requestCount: 23, credit: 3.63 },
      { model: "glm-5.2", requestCount: 1, credit: 33.63 },
      { model: "hy3", requestCount: 62, credit: 0 },
    ],
  },
  {
    requestCount: 679,
    models: [
      { model: "deepseek-v4-flash", requestCount: 659, credit: 1270.62 },
      { model: "hy3", requestCount: 20, credit: 0 },
    ],
  },
  {
    requestCount: 318,
    models: [
      { model: "deepseek-v4-flash", requestCount: 309, credit: 595.08 },
      { model: "hy3", requestCount: 9, credit: 0 },
    ],
  },
];

/**
 * 演示数据刻意让三个账号的积分包数量不同（1 / 2 / 5），覆盖卡片内容区的三种高度：
 * 1 行、2 行、2 行 + 「查看全部积分包」。这样演示模式能真实暴露「同排卡片因内容长度不同
 * 而高低参差」的布局问题 —— 若三家都给同样多的包，这个问题在演示里永远看不见。
 * 请勿"顺手"改回统一数量。
 */
const creditPackages = [
  // 账号 A：1 条 → 1 行，无「查看全部积分包」
  [
    ["CodeBuddy 新用户体验包", 800, 386.4, 5],
  ],
  // 账号 B：2 条 → 2 行，无「查看全部积分包」（链接在 resources.length > 2 时才出现）
  [
    ["CodeBuddy 个人版积分包", 1800, 905.5, 42],
    ["CodeBuddy 签到赠送积分", 240, 174.35, 15],
  ],
  // 账号 C：5 条 → 2 行 + 「查看全部积分包」
  [
    ["CodeBuddy 个人版国内运营裂变包", 2400, 1680.4, 29],
    ["CodeBuddy 个人版积分包", 1200, 748.6, 55],
    ["CodeBuddy 新用户体验包", 360, 214.5, 14],
    ["CodeBuddy 签到赠送积分", 180, 96.75, 21],
    ["CodeBuddy 活动奖励积分", 300, 207.9, 38],
  ],
] as const;

function startOfToday(): Date {
  const date = new Date();
  date.setHours(0, 0, 0, 0);
  return date;
}

function localDate(daysAgo: number): string {
  const date = startOfToday();
  date.setDate(date.getDate() - daysAgo);
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

function atLocalTime(daysAgo: number, hour: number, minute: number): number {
  const date = startOfToday();
  date.setDate(date.getDate() - daysAgo);
  date.setHours(hour, minute, 0, 0);
  return date.getTime();
}

function futureAt(daysAhead: number, hour = 23, minute = 59): number {
  const date = startOfToday();
  date.setDate(date.getDate() + daysAhead);
  date.setHours(hour, minute, 0, 0);
  return date.getTime();
}

function hydratedAccounts(): AccountMeta[] {
  return accounts.map((account, index) => ({
    ...account,
    expiresAt: futureAt(12 + index * 5, 18, 30),
    refreshExpiresAt: futureAt(40 + index * 7),
    refreshedAt: atLocalTime(0, 9, 12 + index * 7),
    createdAt: atLocalTime(45 + index * 19, 10, 0),
  }));
}

function creditExpiry(accountId: string): CreditExpiry {
  const index = Math.max(0, accounts.findIndex((account) => account.id === accountId));
  const account = accounts[index] ?? accounts[0];
  const resources = creditPackages[index].map(([packageName, total, remaining, expireDays], packageIndex) => ({
    packageCode: `demo-package-${index + 1}-${packageIndex + 1}`,
    packageName,
    total,
    remaining,
    used: Number((total - remaining).toFixed(2)),
    status: 1,
    expireAt: futureAt(expireDays),
    expired: false,
    expiringSoon: expireDays <= 7,
  }));
  const totalCapacity = resources.reduce((sum, resource) => sum + resource.total, 0);
  const totalRemaining = resources.reduce((sum, resource) => sum + resource.remaining, 0);
  const expiringSoonRemaining = resources.filter((resource) => resource.expiringSoon).reduce((sum, resource) => sum + resource.remaining, 0);

  return {
    ok: true,
    accountId: account.id,
    accountName: account.nickname ?? account.email ?? account.id,
    updatedAt: Date.now() - (index + 1) * 4 * 60 * 1000,
    totalCapacity,
    totalRemaining: Number(totalRemaining.toFixed(2)),
    expiringSoonRemaining: Number(expiringSoonRemaining.toFixed(2)),
    expiredRemaining: 0,
    soonestExpireAt: Math.min(...resources.map((resource) => resource.expireAt)),
    expiringSoon: expiringSoonRemaining > 0,
    expired: false,
    resources,
  };
}

function dailyWeight(accountIndex: number, dayIndex: number): number {
  const weekdayWave = [0.72, 1.08, 0.93, 1.22, 0.84, 1.16, 1.01][dayIndex % 7];
  const quiet = (dayIndex + accountIndex * 4) % 13 === 0 ? 0.16 : 1;
  return weekdayWave * quiet * (1 + accountIndex * 0.035);
}

function distributeModels(seed: AccountUsageSeed, accountIndex: number) {
  const weights = Array.from({ length: 30 }, (_, dayIndex) => dailyWeight(accountIndex, dayIndex));
  const weightTotal = weights.reduce((sum, weight) => sum + weight, 0);
  const countSeries = seed.models.map((model) => {
    const raw = weights.map((weight) => (model.requestCount * weight) / weightTotal);
    const values = raw.map(Math.floor);
    let remaining = model.requestCount - values.reduce((sum, value) => sum + value, 0);
    const byFraction = raw.map((value, index) => ({ index, fraction: value - Math.floor(value) })).sort((left, right) => right.fraction - left.fraction);
    for (let index = 0; index < remaining; index += 1) values[byFraction[index].index] += 1;
    return values;
  });
  const creditSeries = seed.models.map((model) => {
    const values = weights.map((weight) => Number(((model.credit * weight) / weightTotal).toFixed(2)));
    const drift = Number((model.credit - values.reduce((sum, value) => sum + value, 0)).toFixed(2));
    values[values.length - 1] = Number((values[values.length - 1] + drift).toFixed(2));
    return values;
  });
  return Array.from({ length: 30 }, (_, dayIndex) => {
    const models = seed.models.map((model, modelIndex) => ({
      model: model.model,
      requestCount: countSeries[modelIndex][dayIndex],
      credit: creditSeries[modelIndex][dayIndex],
    }));
    return {
      date: localDate(29 - dayIndex),
      usage: Number(models.reduce((sum, model) => sum + model.credit, 0).toFixed(2)),
      models,
    };
  });
}

function sumModels(rows: { models: CreditOfficialUsageModel[] }[]): CreditOfficialUsageModel[] {
  const totals = new Map<string, CreditOfficialUsageModel>();
  for (const row of rows) {
    for (const model of row.models) {
      const current = totals.get(model.model) ?? { model: model.model, requestCount: 0, credit: 0 };
      current.requestCount += model.requestCount;
      current.credit = Number((current.credit + model.credit).toFixed(2));
      totals.set(model.model, current);
    }
  }
  return [...totals.values()].sort((left, right) => right.credit - left.credit);
}

function visibleRequests(accountIndex: number) {
  const account = accounts[accountIndex];
  const seed = usageSeeds[accountIndex];
  const hours = [16, 15, 17, 14, 1, 0, 3];
  const flashCredits = [0.13, 0.04, 0.2, 1, 0.08, 3.99, 0.45, 8.5, 24.56];
  const kimiCredits = [86.4, 103.2, 112.8, 128.4, 74.6];
  const proCredits = [0.04, 0.13, 0.2, 0.45];
  const weightedModels = seed.models.flatMap((model) =>
    Array.from({ length: Math.max(1, Math.round((model.requestCount / seed.requestCount) * 100)) }, () => model.model),
  );

  return Array.from({ length: 100 }, (_, rowIndex) => {
    const daysAgo = Math.floor(rowIndex / 8);
    const hour = hours[(rowIndex + accountIndex * 2) % hours.length];
    const minute = (rowIndex * 7 + accountIndex * 11) % 60;
    const ts = new Date(atLocalTime(daysAgo, hour, minute));
    const model = rowIndex < seed.models.length
      ? seed.models[rowIndex].model
      : weightedModels[rowIndex % weightedModels.length];
    const credit = model === "hy3"
      ? 0
      : model === "kimi-k3-1"
        ? kimiCredits[(rowIndex + accountIndex) % kimiCredits.length]
        : model === "glm-5.2"
          ? 33.63
          : model === "deepseek-v4-pro"
            ? proCredits[(rowIndex + accountIndex) % proCredits.length]
            : flashCredits[(rowIndex + accountIndex * 3) % flashCredits.length];
    return {
      accountId: account.id,
      accountName: account.nickname ?? account.email ?? account.id,
      requestId: `demo-request-${String(accountIndex + 1).padStart(2, "0")}-${String(rowIndex + 1).padStart(4, "0")}`,
      credit,
      model,
      client: rowIndex % 50 === 0 ? "CodeBuddyIDE" : "CLI",
      requestTime: `${localDate(daysAgo)} ${String(ts.getHours()).padStart(2, "0")}:${String(ts.getMinutes()).padStart(2, "0")}:00`,
    };
  });
}

function buildStatistics(): CreditStatistics {
  const demoAccounts = hydratedAccounts();
  const accountDaily = usageSeeds.map((seed, index) => distributeModels(seed, index));
  const daily = accountDaily[0].map((_, dayIndex) => {
    const models = new Map<string, CreditOfficialUsageModel>();
    for (const rows of accountDaily) {
      for (const model of rows[dayIndex].models) {
        const current = models.get(model.model) ?? { model: model.model, requestCount: 0, credit: 0 };
        current.requestCount += model.requestCount;
        current.credit = Number((current.credit + model.credit).toFixed(2));
        models.set(model.model, current);
      }
    }
    const modelRows = [...models.values()];
    return { date: accountDaily[0][dayIndex].date, usage: Number(modelRows.reduce((sum, model) => sum + model.credit, 0).toFixed(2)), models: modelRows };
  });
  const sumRecent = (rows: { usage: number }[], count: number) => Number(rows.slice(-count).reduce((sum, row) => sum + row.usage, 0).toFixed(2));
  const monthPrefix = localDate(0).slice(0, 7);
  const sumMonth = (rows: { date: string; usage: number }[]) => Number(rows.filter((row) => row.date.startsWith(monthPrefix)).reduce((sum, row) => sum + row.usage, 0).toFixed(2));
  const generatedAt = Date.now() - 3 * 60 * 1000;
  const creditRows = accounts.map((account) => creditExpiry(account.id));
  const officialAccounts = demoAccounts.map((account, index) => ({
    accountId: account.id,
    accountName: account.nickname ?? account.email ?? account.id,
    ok: true,
    requestCount: usageSeeds[index].requestCount,
    detailTruncated: true,
    usageToday: accountDaily[index][accountDaily[index].length - 1]?.usage ?? 0,
    usage7Days: sumRecent(accountDaily[index], 7),
    usageThisMonth: sumMonth(accountDaily[index]),
    reportedTotal: usageSeeds[index].requestCount,
    fetchedCount: usageSeeds[index].requestCount,
    models: sumModels(accountDaily[index]),
    daily: accountDaily[index],
  }));
  const usageToday = daily[daily.length - 1]?.usage ?? 0;
  const usage7Days = sumRecent(daily, 7);
  const usageThisMonth = sumMonth(daily);
  const totalRemaining = creditRows.reduce((sum, credit) => sum + (credit.totalRemaining ?? 0), 0);
  const totalCapacity = creditRows.reduce((sum, credit) => sum + (credit.totalCapacity ?? 0), 0);

  return {
    generatedAt,
    retentionDays: 90,
    coverageStartAt: atLocalTime(29, 0, 0),
    summary: { currentRemaining: Number(totalRemaining.toFixed(2)), currentCapacity: totalCapacity, usageToday, usage7Days, usageThisMonth, todayCheckedInAccounts: 3, todaySuccess: 2, todayAlready: 1, todayFailed: 0 },
    daily,
    accounts: demoAccounts.map((account, index) => ({
      accountId: account.id,
      accountName: account.nickname ?? account.email ?? account.id,
      isCurrent: index === 0,
      currentRemaining: creditRows[index].totalRemaining ?? null,
      totalCapacity: creditRows[index].totalCapacity ?? null,
      lastSnapshotAt: generatedAt - index * 120_000,
      usageToday: officialAccounts[index].usageToday ?? 0,
      usage7Days: officialAccounts[index].usage7Days ?? 0,
      usageThisMonth: officialAccounts[index].usageThisMonth ?? 0,
      checkedInToday: true,
      checkinStatusToday: index === 1 ? "already" : "success",
      lastCheckinAt: atLocalTime(0, 8, 6 + index * 9),
      lastCheckinResult: index === 1 ? "already" : "success",
      daily: accountDaily[index],
    })),
    events: demoAccounts.map((account, index) => ({ kind: "checkin" as const, ts: atLocalTime(0, 8, 6 + index * 9), date: localDate(0), accountId: account.id, accountName: account.nickname ?? account.email ?? account.id, result: index === 1 ? "already" : "success" })),
    officialUsage: {
      status: "complete",
      rangeStart: localDate(29),
      rangeEnd: localDate(0),
      collectedAt: generatedAt,
      summary: { usageToday, usage7Days, usageThisMonth },
      daily,
      accounts: officialAccounts,
      requests: accounts.flatMap((_, index) => visibleRequests(index)),
      models: sumModels(daily),
      detailLimitPerAccount: 100,
      errors: [],
    },
  };
}

function checkinConfig(): CheckinConfig {
  return {
    enabled: true,
    checkin_start: "",
    checkin_end: "",
    keepalive_days: 7,
    lazy_refresh_hours: 12,
  };
}

function travelConfig(): TravelConfig {
  return { enabled: true };
}

function travelStatus(accountId: string): TravelStatus {
  const index = Math.max(0, accounts.findIndex((account) => account.id === accountId));
  // 演示三种状态：旅行中 / 已结束 / 无 Buddy
  if (index % 3 === 0) return { label: "traveling", rewardCredit: 7, locationName: "咖啡馆", arriveAt: Math.floor(Date.now() / 1000) + 2 * 3600 + 40 * 60 };
  if (index % 3 === 1) return { label: "finished", rewardCredit: 20, locationName: "健身房" };
  return { label: "no-buddy", rewardCredit: null, locationName: null };
}

/**
 * 模型限额演示数据：A 单模型受限（图标无角标）、B 双模型受限（图标带数量角标，
 * 其中一条故意归因失败以展示「未知模型」）、C 无受限（图标不渲染）。
 *
 * 恢复时刻必须相对 `Date.now()` 生成：前端每秒按 `resetAt` 过滤，写死绝对时间会让
 * 演示页在某个时刻之后再也看不到图标。三条条目也刻意覆盖「小时级 / 分钟级」倒计时。
 */
function rateLimits(): RateLimitsPayload {
  const now = Date.now();
  return {
    scannedAt: now,
    windowDays: 2,
    accounts: [
      {
        accountId: accounts[0].id,
        limited: [
          { model: "deepseek-v4.1-flash", resetAt: now + 134 * 60_000, firstSeenAt: now - 26 * 60_000, hitCount: 7 },
        ],
      },
      {
        accountId: accounts[1].id,
        limited: [
          { model: "kimi-k3-1", resetAt: now + 47 * 60_000, firstSeenAt: now - 41 * 60_000, hitCount: 7 },
          { model: null, resetAt: now + 5 * 3_600_000, firstSeenAt: now - 12 * 60_000, hitCount: 2 },
        ],
      },
    ],
  };
}

function rotateConfig(): AutoRotateConfig {
  return { enabled: true, check_interval_minutes: 15, cooldown_minutes: 120, min_gap_hours: 24, min_urgency_hours: 72, active_guard_minutes: 30, min_remaining_credits: 50 };
}

/** 限额 hook 演示状态：三处配置都显示为已安装。 */
function rateLimitHookStatus(): RateLimitHookStatus {
  const base = "/demo/.wb-switch";
  return {
    scriptPath: `${base}/hook.sh`,
    scriptExists: true,
    eventsPath: `${base}/hook-events.jsonl`,
    installed: true,
    lastEventAt: Date.now() - 4 * 60_000,
    targets: ["codebuddy", "workbuddy", "workbuddy-ai"].map((label) => ({
      label,
      path: `/demo/.${label}/settings.json`,
      exists: true,
      installed: true,
    })),
  };
}

function checkinLogs(): CheckinLog[] {
  return hydratedAccounts().flatMap((account, accountIndex) => [0, 1, 2].map((daysAgo) => ({ ts: atLocalTime(daysAgo, 8, 6 + accountIndex * 9), accountId: account.id, email: account.nickname ?? account.email ?? account.id, result: accountIndex === 1 && daysAgo === 0 ? "already" : "success" })));
}

function rotateLogs(): RotateLog[] {
  return [
    { ts: atLocalTime(0, 9, 30), action: "skipped", reason: "当前账号仍是积分到期最紧迫的可用账号", from: { id: accounts[0].id, name: accounts[0].nickname }, to: null },
    { ts: atLocalTime(1, 16, 20), action: "switched", reason: "目标账号积分将在 5 天内到期", from: { id: accounts[1].id, name: accounts[1].nickname }, to: { id: accounts[0].id, name: accounts[0].nickname } },
  ];
}

function demoTokenTotals(input: number, output: number, cacheRead: number, cacheWrite: number, records: number): TokenStatsTotals {
  return { total: input + output + cacheWrite, input, output, cacheRead, cacheWrite, uncachedInput: Math.max(0, input - cacheRead), records, cacheHitRate: input > 0 ? cacheRead / input : null };
}

function demoTokenGroup(key: string, input: number, output: number, cacheRead: number, cacheWrite: number, records: number): TokenStatsGroup {
  return { key, ...demoTokenTotals(input, output, cacheRead, cacheWrite, records) };
}

function demoTokenSession(key: string, title: string, project: string, input: number, output: number, cacheRead: number, cacheWrite: number, records: number): TokenStatsGroup {
  const keyParts = key.split(" · ");
  return { ...demoTokenGroup(key, input, output, cacheRead, cacheWrite, records), title, project, sessionId: keyParts[keyParts.length - 1] };
}

/** 演示用请求明细：约 120 条，时间戳落在最近 14 天内且按时间倒序。 */
function demoTokenRequests(scale: number): TokenStatsRequestRow[] {
  const models = ["deepseek-v4-flash", "deepseek-v4-flash", "kimi-k3-1", "deepseek-v4-flash", "glm-5.2"];
  const sessions = [
    { project: "wb-switch-rust", sessionId: "token-stats-dashboard", title: "完善 Token 统计仪表盘与本地用量分析" },
    { project: "wb-switch-rust", sessionId: "account-card-redesign", title: "统一账号卡片视觉和交互" },
    { project: "my-code-teams", sessionId: "settings-agent-acp", title: "设计 Agent 与 ACP 管理设置" },
    { project: "LetterTotTown", sessionId: "character-audio", title: "补全角色成语双音频" },
  ];
  // 每天 9 条、小时降序，保证整体严格倒序（最新在前）；从昨天开始，
  // 避免演示时间戳落在当前时刻之后。
  const hours = [23, 21, 20, 17, 16, 15, 14, 11, 10];

  return Array.from({ length: 120 }, (_, index) => {
    const session = sessions[Math.floor(index / 3) % sessions.length];
    const factor = 0.55 + ((index * 37) % 23) / 20;
    const input = Math.round(310_000 * factor * scale);
    const cacheRead = Math.round(input * 0.87);
    const output = Math.round(20_000 * factor * scale);
    const cacheWrite = index % 4 === 0 ? 0 : Math.round(4_000 * factor * scale);
    // 思考过程是 output 的子集：比例随行变化，保证 thinking <= output。
    const thinking = Math.round(output * [0.34, 0.12, 0.05, 0.41, 0.22][index % 5]);
    return {
      timestamp: atLocalTime(Math.floor(index / 9) + 1, hours[index % 9], (index * 13) % 60),
      model: models[index % models.length],
      project: session.project,
      sessionId: session.sessionId,
      title: session.title,
      input,
      output,
      cacheRead,
      cacheWrite,
      // 与后端明细行同口径：input 已含 cacheRead，未命中部分为两者之差。
      uncachedInput: Math.max(0, input - cacheRead),
      thinking,
      total: input + output + cacheWrite,
    };
  });
}

function demoTokenSource(source: TokenStatsSource["source"], scale: number): TokenStatsSource {
  const daily = Array.from({ length: 14 }, (_, index) => {
    const wave = [0.62, 0.86, 1.1, 0.72, 1.3, 0.94, 0.38][index % 7] * scale;
    return demoTokenGroup(localDate(13 - index), Math.round(7_600_000 * wave), Math.round(480_000 * wave), Math.round(6_650_000 * wave), Math.round(95_000 * wave), Math.round(24 * wave));
  });
  const summary = daily.reduce((sum, row) => demoTokenTotals(sum.input + row.input, sum.output + row.output, sum.cacheRead + row.cacheRead, sum.cacheWrite + row.cacheWrite, sum.records + row.records), demoTokenTotals(0, 0, 0, 0, 0));
  const hours = Array.from({ length: 7 * 24 }, (_, index) => {
    const day = Math.floor(index / 24); const hour = index % 24;
    const active = Math.max(0.02, Math.exp(-Math.pow(hour - (day >= 5 ? 22 : 15), 2) / 22));
    return demoTokenGroup(`${day}-${hour}`, Math.round(720_000 * active * scale), Math.round(41_000 * active * scale), Math.round(610_000 * active * scale), 0, Math.max(1, Math.round(6 * active * scale)));
  });
  const projects = [
    demoTokenGroup("wb-switch-rust", 42_800_000 * scale, 2_400_000 * scale, 37_100_000 * scale, 420_000 * scale, Math.round(148 * scale)),
    demoTokenGroup("my-code-teams", 25_600_000 * scale, 1_650_000 * scale, 21_900_000 * scale, 260_000 * scale, Math.round(96 * scale)),
    demoTokenGroup("LetterTotTown", 11_900_000 * scale, 920_000 * scale, 9_700_000 * scale, 110_000 * scale, Math.round(51 * scale)),
  ];
  const models = [
    demoTokenGroup("deepseek-v4-flash", 56_400_000 * scale, 3_200_000 * scale, 49_100_000 * scale, 530_000 * scale, Math.round(210 * scale)),
    demoTokenGroup("kimi-k3-1", 17_300_000 * scale, 1_140_000 * scale, 14_200_000 * scale, 180_000 * scale, Math.round(61 * scale)),
    demoTokenGroup("glm-5.2", 6_600_000 * scale, 630_000 * scale, 5_400_000 * scale, 80_000 * scale, Math.round(24 * scale)),
  ];
  const sessions = [
    demoTokenSession("wb-switch-rust · token-stats-dashboard", "完善 Token 统计仪表盘与本地用量分析", "wb-switch-rust", 18_700_000 * scale, 1_050_000 * scale, 16_100_000 * scale, 160_000 * scale, Math.round(72 * scale)),
    demoTokenSession("my-code-teams · settings-agent-acp", "设计 Agent 与 ACP 管理设置", "my-code-teams", 13_200_000 * scale, 890_000 * scale, 11_300_000 * scale, 120_000 * scale, Math.round(55 * scale)),
    demoTokenSession("LetterTotTown · character-audio", "补全角色成语双音频", "LetterTotTown", 8_600_000 * scale, 640_000 * scale, 7_200_000 * scale, 80_000 * scale, Math.round(38 * scale)),
    demoTokenSession("wb-switch-rust · account-card-redesign", "统一账号卡片视觉和交互", "wb-switch-rust", 6_300_000 * scale, 410_000 * scale, 5_400_000 * scale, 50_000 * scale, Math.round(29 * scale)),
  ];
  const now = Date.now();
  return {
    source, summary, models, projects, sessions, daily, hours,
    filesScanned: source === "workbuddy" ? 63 : source === "codebuddy-ide" ? 17 : 41,
    parseErrors: 0,
    coverageStartAt: now - 13 * 86_400_000,
    coverageEndAt: now,
    // 只有 CodeBuddy CLI 来源返回请求明细，与真实后端行为一致。
    ...(source === "codebuddy-cli" ? { requests: demoTokenRequests(scale) } : {}),
  };
}

/** 国际版数据源：演示环境不构造数据，保持真实的「空集」形态（页面显示空状态）。 */
function emptyTokenSource(source: TokenStatsSource["source"]): TokenStatsSource {
  return {
    source,
    summary: demoTokenTotals(0, 0, 0, 0, 0),
    models: [],
    projects: [],
    sessions: [],
    daily: [],
    hours: [],
    filesScanned: 0,
    parseErrors: 0,
  };
}

function demoTokenStatistics(days?: number): TokenStatistics {
  return { generatedAt: Date.now(), rangeDays: days ?? null, sources: [demoTokenSource("workbuddy", 1), emptyTokenSource("workbuddy-ai"), demoTokenSource("codebuddy-cli", 0.58), demoTokenSource("codebuddy-ide", 0.36)] };
}

/** Read-only demo response provider. It never reads or mutates real user data. */
export function screenshotDemoResponse(command: string, args?: Record<string, unknown>): unknown {
  const demoAccounts = hydratedAccounts();
  const appStatus: AppStatus = { running: true, authFile: "/demo/workbuddy/auth.json", current: { uid: demoAccounts[0].uid, nickname: demoAccounts[0].nickname, email: demoAccounts[0].email }, appPath: "/demo/WorkBuddy.app", version: "0.1.24" };
  const activeIndex = Math.max(0, demoAccounts.findIndex((account) => account.id === demoActiveCliAccountId));
  const activeAccount = demoAccounts[activeIndex] ?? demoAccounts[0];
  const cliStatus: CodeBuddyCliStatus = { configured: true, settingsPresent: true, helperPresent: true, helperSupportsAccountIds: true, activeIndex, activeAccountId: activeAccount.id, activeAccountName: activeAccount.nickname, activeAccountVariant: accountVariant(activeAccount), accountCount: demoAccounts.length, statePath: "/demo/codebuddy-cli-state.json" };
  const config = rotateConfig();
  const rotateStatus: RotateStatus = { config, cliConfigured: true, activeAccountId: demoAccounts[0].id, activeAccountName: demoAccounts[0].nickname, lastCheckAt: atLocalTime(0, 9, 30), lastSwitchAt: atLocalTime(1, 16, 20) };
  const githubConfig: GithubConfig = { owner: "zhangjia", repo: "wb-switch", proxy: "" };
  switch (command) {
    // 档位随请求回显：演示数据本身只有国内版账号，国际版展示空状态。
    case "get_status": {
      const variant = normalizeVariant(args?.variant);
      return variant === "ai"
        ? {
            ...appStatus,
            running: false,
            current: null,
            authFile: "/demo/workbuddy-ai/auth.json",
            appPath: "/demo/WorkBuddy AI.app",
            variant,
          }
        : { ...appStatus, variant };
    }
    case "get_accounts": return { accounts: demoAccounts };
    case "discover_known_accounts":
      return {
        accounts: demoAccounts.map((account) => ({
          uid: account.uid,
          nickname: account.nickname,
          email: account.email,
          source: "auth-history",
          backupFiles: 3,
          backedUpAt: futureAt(-1, 21, 0),
          inAccountList: true,
          accessTokenExpiresAt: account.expiresAt,
          refreshTokenExpiresAt: account.refreshExpiresAt,
          restorable: true,
        })),
      };
    case "adopt_account": return { ok: true, account: demoAccounts[0] };
    case "get_codebuddy_cli_status": return cliStatus;
    // 让演示里存在一个「CodeBuddy IDE 当前账号」：否则 IDE 标记与选中态染色（淡紫）在演示里永远不可见。
    // 取第二个账号，使三张卡各自演示一种形态（A 占位行 / B IDE 选中 / C 查看全部）。
    case "get_codebuddy_cn_ide_status": return {
      installed: true,
      running: true,
      dataDir: "/demo/codebuddy-cn-ide",
      dbPath: "/demo/codebuddy-cn-ide/state.vscdb",
      dbExists: true,
      appPath: "/demo/CodeBuddy CN.app",
      activeAccountId: demoAccounts[1].id,
      activeAccountName: demoAccounts[1].nickname,
    } satisfies CodeBuddyCnIdeStatus;
    // 国际版同理：不 mock 会让演示切到「国际版」时落到 default → throw（被 AccountsPage 的
    // try/catch 吞掉），IDE 标记退化成「未接入」——而这正是本轮国际版 IDE 功能在演示页的展示面。
    case "get_codebuddy_ide_status": return {
      installed: true,
      running: true,
      dataDir: "/demo/codebuddy-ide",
      dbPath: "/demo/codebuddy-ide/state.vscdb",
      dbExists: true,
      appPath: "/demo/CodeBuddy IDE.app",
      activeAccountId: demoAccounts[1].id,
      activeAccountName: demoAccounts[1].nickname,
    } satisfies CodeBuddyCnIdeStatus;
    case "get_vscode_ext_status": return {
      installed: true,
      extensionInstalled: true,
      running: false,
      loggedIn: true,
      dataDir: "/demo/Code",
      dbPath: "/demo/Code/User/globalStorage/state.vscdb",
      dbExists: true,
      activeAccountId: demoAccounts[0].id,
      activeAccountName: demoAccounts[0].nickname,
      detectedFrom: "state",
      statePath: "/demo/vscode_ext.json",
    } satisfies VscodeExtStatus;
    case "list_vscode_sessions": return {
      sourceUid: demoAccounts[0].uid,
      skipped: 0,
      dataRoot: "/demo/CodeBuddyExtension/Data",
      sessions: [
        { id: "7f3a91c0d4e5b6a7c8d9e0f1a2b3c4d5", workspaceHash: "3c1f8a92b4d5e60718f9a0b1c2d3e4f5", title: "完善账号卡片交互", updatedAt: Date.now() - 1000 * 60 * 12, type: "craft", hasHistory: true },
        { id: "9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e", workspaceHash: "3c1f8a92b4d5e60718f9a0b1c2d3e4f5", title: "修复切换后历史为空", updatedAt: Date.now() - 1000 * 60 * 60 * 3, type: "craft", hasHistory: true },
        { id: "11223344556677889900112233445566", workspaceHash: "aabbccddeeff00112233445566778899", title: "设计会话复制方案", updatedAt: Date.now() - 1000 * 60 * 60 * 26, type: "craft", hasHistory: true },
        { id: "66554433221100998877665544332211", workspaceHash: "aabbccddeeff00112233445566778899", title: "(无标题)", updatedAt: Date.now() - 1000 * 60 * 60 * 50, type: "craft", hasHistory: false },
      ],
    } satisfies VscodeSessionList;
    case "switch_codebuddy_cli_account": {
      const target = demoAccounts.find((account) => account.id === args?.accountId);
      if (!target) throw new Error("账号不存在");
      demoActiveCliAccountId = target.id;
      return { ok: true, configured: true, synced: true, verified: true, activeIndex: demoAccounts.indexOf(target), activeAccountId: target.id, regionChanged: false, cliClosed: false, closedProcessCount: 0, message: "演示切换已完成" } satisfies CodeBuddyCliSwitchResult;
    }
    case "get_checkin_status": return { ok: true, todayCheckedIn: true };
    case "get_credit_expiry": return creditExpiry(String(args?.accountId ?? ""));
    case "get_credit_statistics": return buildStatistics();
    case "get_token_statistics": return demoTokenStatistics(typeof args?.days === "number" ? args.days : undefined);
    case "get_auto_checkin_config": return checkinConfig();
    case "get_checkin_logs": return { logs: checkinLogs() };
    case "get_travel_status": return travelStatus(String(args?.accountId ?? ""));
    case "get_rate_limits": return rateLimits();
    case "get_rate_limit_hook_status": return rateLimitHookStatus();
    case "get_rate_limit_config": return { enabled: true };
    case "get_auto_travel_config": return travelConfig();
    case "get_auto_rotate_config": return config;
    case "rotate_status": return rotateStatus;
    case "get_rotate_logs": return { logs: rotateLogs() };
    case "get_github_config": return githubConfig;
    case "check_update": return { ok: true, current: "0.1.24", latest: "0.1.25", latestTag: "v0.1.25", hasUpdate: true, releaseName: "更新提示演示", releaseUrl: "https://github.com/changexbc/workbuddy-switch/releases/tag/v0.1.25" };
    case "get_launch_at_login_enabled": return true;
    case "switch_progress": return { running: false, progress: null };
    default: throw new Error(`演示模式缺少只读数据: ${command}`);
  }
}
