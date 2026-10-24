import type {
  AccountMeta, AppStatus, AutoRotateConfig, CheckinConfig, CheckinLog,
  CodeBuddyCliStatus, CodeBuddyCliSwitchResult, CodeBuddyCnIdeStatus, CreditExpiry, CreditOfficialUsageModel, CreditStatistics,
  DisplayField,
  GithubConfig, RateLimitHookStatus, RateLimitsPayload, RotateLog, RotateStatus, TokenStatistics, TokenStatsAccount, TokenStatsGroup, TokenStatsRequestRow, TokenStatsSource, TokenStatsTotals,
  Session, SessionLinkPreviewGroup, SessionLinksPreview, SessionSyncVerdict, SessionGroupClient, SessionGroupSummary, SessionGroupDetail, SessionGroupList, SessionGroupPairPreview, SessionMemberVersionStatus, TravelConfig, TravelStatus, VscodeExtStatus, VscodeSessionList,
} from "./types";
import { displayName } from "./account-display";
import { demoModeEnabled } from "./demo-mode";
import { accountVariant, normalizeVariant, variantSupportsCheckin } from "./variant";

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

// 演示数据按档位分组：前三行是国内版 fixture，后两行是国际版；最后一行是企业版
// （国内）演示账号，只覆盖账号页卡片形态（「企业版」标注 + 企业配额 / 不限量）。
// 国际版同时覆盖账号页、积分统计与 Token 统计；
// 限额台账、签到、旅行、轮换保持原有空态，见各档位能力函数。
const intlAccountA: AccountMeta = { id: "demo-account-ai-a", uid: "demo-intl-001", email: "intl-a@example.com", nickname: "国际版 A", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "ai" };
const intlAccountB: AccountMeta = { id: "demo-account-ai-b", uid: "demo-intl-002", email: "intl-b@example.com", nickname: "国际版 B", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "ai" };

const accounts: AccountMeta[] = [
  { id: "demo-account-a", uid: "demo-user-001", email: "test-a@example.com", nickname: "测试 A", phoneNumber: "138 0013 8001", note: "主力账号", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "cn" },
  { id: "demo-account-b", uid: "demo-user-002", email: "test-b@example.com", nickname: "测试 B", phoneNumber: "138 0013 8002", note: "备用号", displayField: "note", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "cn" },
  // 企业版有限配额：卡片应显示「企业版」徽标 + 单条「企业配额」。
  { id: "demo-account-c", uid: "demo-user-003", email: "test-c@example.com", nickname: "测试 C", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "cn", isEnterprise: true },
  intlAccountA,
  intlAccountB,
  // 企业版不限量（limitNum=-1）：卡片应显示「不限量」+「企业配额」，无积分包。
  { id: "demo-account-d", uid: "demo-user-004", email: "test-d@example.com", nickname: "测试 D", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null, variant: "cn", isEnterprise: true },
];

/** 演示模式中的临时 CLI 当前账号，仅存在于本次页面会话。 */
let demoActiveCliAccountId = accounts[0].id;

/**
 * 积分统计里的「当前账号」：后端按账号列表构造 `current_account_ids`（列表内即当前），
 * 前端档位视图只把 `isCurrent` 账号计入「当前剩余」。演示里各档位取一个代表账号，
 * 使国际版视图也有非零的当前剩余；国内版仍只取账号 A，保持既有数值不变。
 */
const currentAccountIds = new Set([accounts[0].id, intlAccountA.id]);

// Counts and relative model roles follow anonymous aggregates from the sanitized local cache.
// No upstream request row or identifier is copied into this fixture.
// 与 `accounts` 严格同索引：`buildStatistics()` 按账号索引取这里的 seed。
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
  // 国际版 A：用量集中在主力模型，国际版演示视图因此有可读的曲线与模型占比。
  {
    requestCount: 1420,
    models: [
      { model: "deepseek-v4-flash", requestCount: 1338, credit: 1120.5 },
      { model: "kimi-k3-1", requestCount: 68, credit: 642.7 },
      { model: "glm-5.2", requestCount: 14, credit: 33.63 },
    ],
  },
  // 国际版 B：轻量账号，用于演示同一档位内的用量差异。
  {
    requestCount: 268,
    models: [
      { model: "deepseek-v4-flash", requestCount: 240, credit: 482.9 },
      { model: "hy3", requestCount: 28, credit: 0 },
    ],
  },
  // 企业版 D：轻量用量，让统计页的企业版账号也有可读数据。
  {
    requestCount: 156,
    models: [
      { model: "deepseek-v4-flash", requestCount: 148, credit: 268.4 },
      { model: "hy3", requestCount: 8, credit: 0 },
    ],
  },
];

/** 演示积分包种子：[名称, 总量, 剩余, N 天后到期]；与 `accounts` 严格同索引。 */
type CreditPackageSeed = readonly [packageName: string, total: number, remaining: number, expireDays: number];

/**
 * 演示积分包按账号索引与 `accounts` 严格对应，覆盖卡片内容区的多种形态：
 * A（1 条）/ B（2 条）为个人版，覆盖 1 行、2 行两种高度；C 为企业版单条
 * 「企业配额」（企业接口只返回一条配额资源）；D 为企业版不限量，无积分包。
 * 说明：原先由 C 覆盖的「>2 条 → 查看全部积分包」样例随企业版改造移除，
 * 如需该样例，应由后续新增的个人版账号承载，而不是改回 C 的个人版数据。
 */
const creditPackages: readonly (readonly CreditPackageSeed[])[] = [
  // 账号 A：1 条 → 1 行，无「查看全部积分包」
  [
    ["CodeBuddy 新用户体验包", 800, 386.4, 5],
  ],
  // 账号 B：2 条 → 2 行，无「查看全部积分包」（链接在 resources.length > 2 时才出现）
  [
    ["CodeBuddy 个人版积分包", 1800, 905.5, 42],
    ["CodeBuddy 签到赠送积分", 240, 174.35, 15],
  ],
  // 账号 C（企业版有限配额）：单条「企业配额」，总量 2000 / 剩余 1500 / 30 天重置
  [
    ["企业配额", 2000, 1500, 30],
  ],
  // 国际版账号：合成码不会命中官方商品码映射，因此展示的就是这里自造的名字；
  // 沿用官方命名口径，覆盖「试用 / 赠送 / 购买」三类国际版语义。
  [
    ["试用版基础用量", 2000, 1340.8, 30],
    ["版本赠送用量", 600, 402.35, 75],
  ],
  [
    ["购买积分", 3000, 2180.6, 12],
    ["试用版基础用量", 500, 318.75, 60],
    ["平台奖励积分", 800, 512.4, 90],
  ],
  // 账号 D（企业版不限量）：无积分包，creditExpiry 返回 unlimited 形态
  [],
];

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
  const packages = creditPackages[index] ?? [];
  const resources = packages.map(([packageName, total, remaining, expireDays], packageIndex) => ({
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
  const updatedAt = Date.now() - (index + 1) * 4 * 60 * 1000;

  // 企业版（issue #167）：企业配额不在个人版接口里，卡片按企业形态渲染。
  // 无积分包 = 不限量（limitNum=-1）；有积分包 = 单条「企业配额」有限配额。
  if (account.isEnterprise && resources.length === 0) {
    return {
      ok: true,
      accountId: account.id,
      accountName: account.nickname ?? account.email ?? account.id,
      updatedAt,
      unlimited: true,
      totalCapacity: null,
      totalRemaining: null,
      expiringSoonRemaining: 0,
      expiredRemaining: 0,
      soonestExpireAt: null,
      expiringSoon: false,
      expired: false,
      resources: [],
    };
  }

  return {
    ok: true,
    accountId: account.id,
    accountName: account.nickname ?? account.email ?? account.id,
    updatedAt,
    // 企业版有限配额显式标记 unlimited=false；个人版不新增字段（保持原有 mock 形态）。
    ...(account.isEnterprise ? { unlimited: false } : {}),
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
    // 签到计数与 `checkinLogs()` / `events` 的构造规则保持一致：
    // 支持签到的账号共 4 个（国内版 A/B/C/D），其中 B 为 already、其余为 success。
    summary: { currentRemaining: Number(totalRemaining.toFixed(2)), currentCapacity: totalCapacity, usageToday, usage7Days, usageThisMonth, todayCheckedInAccounts: 4, todaySuccess: 3, todayAlready: 1, todayFailed: 0 },
    daily,
    accounts: demoAccounts.map((account, index) => {
      // 国际版没有签到接口：签到态一律留空，与 `variantSupportsCheckin` 一致。
      const checkinSupported = variantSupportsCheckin(accountVariant(account));
      const checkinResult = index === 1 ? "already" : "success";
      return {
        accountId: account.id,
        accountName: account.nickname ?? account.email ?? account.id,
        isCurrent: currentAccountIds.has(account.id),
        currentRemaining: creditRows[index].totalRemaining ?? null,
        totalCapacity: creditRows[index].totalCapacity ?? null,
        lastSnapshotAt: generatedAt - index * 120_000,
        usageToday: officialAccounts[index].usageToday ?? 0,
        usage7Days: officialAccounts[index].usage7Days ?? 0,
        usageThisMonth: officialAccounts[index].usageThisMonth ?? 0,
        checkedInToday: checkinSupported ? true : null,
        checkinStatusToday: checkinSupported ? checkinResult : null,
        lastCheckinAt: checkinSupported ? atLocalTime(0, 8, 6 + index * 9) : null,
        lastCheckinResult: checkinSupported ? checkinResult : null,
        daily: accountDaily[index],
      };
    }),
    // 签到事件按档位能力生成：国际版没有签到接口，不构造事件（国内版索引与取值不变）。
    events: demoAccounts.flatMap((account, index) => variantSupportsCheckin(accountVariant(account))
      ? [{
          kind: "checkin" as const,
          ts: atLocalTime(0, 8, 6 + index * 9),
          date: localDate(0),
          accountId: account.id,
          accountName: account.nickname ?? account.email ?? account.id,
          result: index === 1 ? "already" : "success",
        }]
      : []),
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
    excluded_account_ids: [],
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

/** 签到日志只覆盖支持签到的档位；A/B/C 的索引与取值不变，企业版 D 追加在国际版之后。 */
function checkinLogs(): CheckinLog[] {
  return hydratedAccounts().flatMap((account, accountIndex) => variantSupportsCheckin(accountVariant(account))
    ? [0, 1, 2].map((daysAgo) => ({ ts: atLocalTime(daysAgo, 8, 6 + accountIndex * 9), accountId: account.id, email: account.nickname ?? account.email ?? account.id, result: accountIndex === 1 && daysAgo === 0 ? "already" : "success" }))
    : []);
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

function sumDemoTokenGroups(rows: TokenStatsGroup[]): TokenStatsTotals {
  return rows.reduce((sum, row) => demoTokenTotals(sum.input + row.input, sum.output + row.output, sum.cacheRead + row.cacheRead, sum.cacheWrite + row.cacheWrite, sum.records + row.records), demoTokenTotals(0, 0, 0, 0, 0));
}

function splitDemoTokenGroup(row: TokenStatsGroup, shares: number[]): TokenStatsGroup[] {
  const fields = ["input", "output", "cacheRead", "cacheWrite", "records"] as const;
  const remaining = Object.fromEntries(fields.map((field) => [field, row[field]])) as Record<typeof fields[number], number>;
  return shares.map((share, index) => {
    const values = fields.map((field) => {
      const value = index === shares.length - 1 ? remaining[field] : Math.floor(row[field] * share);
      remaining[field] -= value;
      return value;
    });
    return demoTokenGroup(row.key, values[0], values[1], values[2], values[3], values[4]);
  });
}

function demoTokenAccounts(source: TokenStatsSource["source"], daily: TokenStatsGroup[], models: string[]): Pick<TokenStatsSource, "accounts" | "dailyByModel"> {
  const candidates = source === "workbuddy" ? accounts.filter((account) => accountVariant(account) === "cn")
    : source === "workbuddy-ai" ? accounts.filter((account) => accountVariant(account) === "ai") : accounts;
  const slices = daily.map((row) => splitDemoTokenGroup(row, [0.6, 0.3, 0.1]));
  const tokenAccounts: TokenStatsAccount[] = [candidates[0], candidates[1], null].map((account, index) => {
    const accountDaily = slices.map((rows) => rows[index]);
    const modelSlices = accountDaily.map((row) => splitDemoTokenGroup(row, [0.55, 0.3, 0.15]));
    return {
      accountId: account?.id ?? null, accountName: account ? displayName(account) : "未归属",
      summary: sumDemoTokenGroups(accountDaily), daily: accountDaily,
      dailyByModel: Object.fromEntries(models.map((model, modelIndex) => [model, modelSlices.map((rows) => rows[modelIndex])])),
    };
  });
  const dailyByModel = Object.fromEntries(models.map((model) => [model, daily.map((row) => ({
    key: row.key, ...sumDemoTokenGroups(tokenAccounts.flatMap((account) => account.dailyByModel[model].filter((point) => point.key === row.key))),
  }))]));
  return { accounts: tokenAccounts, dailyByModel };
}

function demoTokenSource(source: TokenStatsSource["source"], scale: number): TokenStatsSource {
  const daily = Array.from({ length: 14 }, (_, index) => {
    const wave = [0.62, 0.86, 1.1, 0.72, 1.3, 0.94, 0.38][index % 7] * scale;
    return demoTokenGroup(localDate(13 - index), Math.round(7_600_000 * wave), Math.round(480_000 * wave), Math.round(6_650_000 * wave), Math.round(95_000 * wave), Math.round(24 * wave));
  });
  const summary = sumDemoTokenGroups(daily);
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
    ...demoTokenAccounts(source, daily, models.map((model) => model.key)),
    filesScanned: source === "workbuddy" ? 63 : source === "codebuddy-ide" ? 17 : 41,
    parseErrors: 0,
    coverageStartAt: now - 13 * 86_400_000,
    coverageEndAt: now,
    // 只有 CodeBuddy CLI 来源返回请求明细，与真实后端行为一致。
    ...(source === "codebuddy-cli" ? { requests: demoTokenRequests(scale) } : {}),
  };
}

function demoTokenStatistics(days?: number): TokenStatistics {
  return { generatedAt: Date.now(), rangeDays: days ?? null, sources: [demoTokenSource("workbuddy", 1), demoTokenSource("workbuddy-ai", 0.24), demoTokenSource("codebuddy-cli", 0.58), demoTokenSource("codebuddy-ide", 0.36)] };
}

/** 演示用的 IDE 会话列表：国内版 / 国际版共用同一套存储，只有来源 uid 不同。 */
function demoIdeSessionList(sourceUid: string): VscodeSessionList {
  return {
    sourceUid,
    skipped: 0,
    dataRoot: "/demo/CodeBuddyExtension/Data",
    sessions: [
      { id: "4a598bb4e3144a799e602dd6c9091d03", workspaceHash: "6c4b8aec50b679a6fc4023b379160418", title: "两分钟后自动回复设置", updatedAt: Date.now() - 1000 * 60 * 8, type: "craft", hasHistory: true },
      { id: "1d2e3f405162738495a6b7c8d9e0f1a2", workspaceHash: "6c4b8aec50b679a6fc4023b379160418", title: "整理会话复制文案", updatedAt: Date.now() - 1000 * 60 * 90, type: "craft", hasHistory: true },
      { id: "a1b2c3d4e5f60718293a4b5c6d7e8f90", workspaceHash: "ffeeddccbbaa99887766554433221100", title: "工作区索引结构确认", updatedAt: Date.now() - 1000 * 60 * 60 * 30, type: "craft", hasHistory: true },
    ],
  };
}

/** 演示用的「会话管理页」会话列表：任务（playground）与空间（按 cwd 分组）两种形态。 */
function demoAccountSessions(intl: boolean): Session[] {
  const now = Date.now();
  const project = intl ? "/Users/demo/WorkBuddy AI/2026-09-18-01-33-29" : "/Users/demo/WorkBuddy/2026-09-18-01-33-29";
  const repo = intl ? "/Users/demo/projects/intl-app" : "/Users/demo/projects/wb-switch";
  return [
    { id: "demo-sess-task-1", title: "整理会话复制文案", cwd: project, updatedAt: now - 1000 * 60 * 8, hasHistory: true, isPlayground: true },
    { id: "demo-sess-task-2", title: "生成 changelog 草稿", cwd: project, updatedAt: now - 1000 * 60 * 95, hasHistory: true, isPlayground: true },
    { id: "demo-sess-space-1", title: "跨档复制内核改造", cwd: repo, updatedAt: now - 1000 * 60 * 40, hasHistory: true },
    { id: "demo-sess-space-2", title: "会话管理页骨架", cwd: repo, updatedAt: now - 1000 * 60 * 60 * 5, hasHistory: true },
    { id: "demo-sess-space-3", title: "发布前回归检查", cwd: "/Users/demo/Documents/notes", updatedAt: now - 1000 * 60 * 60 * 26, hasHistory: true },
  ];
}

/** 会话管理页的副本状态演示项（判定口径与后端 `decide_sync` 一致）。 */
function demoPreviewGroup(
  sessionId: string,
  title: string,
  verdict: SessionSyncVerdict,
  extraA: number,
  extraB: number,
): SessionLinkPreviewGroup {
  return {
    groupId: `demo-group-${sessionId}`,
    title,
    cwd: "/Users/demo",
    verdict,
    extraA,
    extraB,
    common: 10,
    defaultChecked: verdict === "fastForward",
    availableModes: verdict === "fastForward" ? ["fastForward"] : verdict === "diverge" ? ["overwrite"] : [],
    reason: "演示数据",
    // 演示凭据：让「同步 / 覆盖目标」按钮在演示页可见（点击由 DemoAction 拦截）。
    previewToken: `demo-token-${sessionId}`,
    recordCount: { source: 10 + extraA, target: 10 + extraB, baseline: 10 },
    source: { memberId: `m-src-${sessionId}`, uid: "demo-source", accountId: null, sessionId, state: "active" },
    target: {
      memberId: `m-tgt-${sessionId}`,
      uid: "demo-target",
      accountId: null,
      sessionId: `${sessionId}-copy`,
      state: "active",
    },
  };
}

function buildDemoSessionGroups(
  client: SessionGroupClient,
  scope: "cn" | "ai" | null,
  allAccounts: AccountMeta[],
): SessionGroupDetail[] {
  const candidates = allAccounts.filter((account) => {
    if (client === "codebuddyIde") return accountVariant(account) === scope;
    return true;
  });
  const memberAccounts = candidates.length > 0 ? candidates : allAccounts.slice(0, 2);
  const scenario = [
    { title: "跨档复制内核改造", status: "behind" as const, count: 2 },
    { title: "会话筛选和分页", status: "latest" as const, count: 3 },
    { title: "整理长时间未完成的任务", status: "diverge" as const, count: 2 },
    { title: "恢复后历史会话索引", status: "missing" as const, count: 2 },
  ];
  return scenario.map((item, groupIndex) => {
    const groupId = `demo-${client}-${scope ?? "all"}-${groupIndex + 1}`;
    const selectedAccounts = client === "workbuddy" && item.status === "diverge" && memberAccounts.length >= 5
      ? [memberAccounts[1], memberAccounts[2], memberAccounts[4], memberAccounts[0], memberAccounts[3]]
      : Array.from({ length: Math.min(item.count, memberAccounts.length) }, (_, index) => memberAccounts[index]);
    const sharedBaseScenario = client === "workbuddy" && item.status === "diverge" && selectedAccounts.length === 5;
    const members = selectedAccounts.map((account, memberIndex) => {
      let versionStatus: SessionMemberVersionStatus = "latest";
      let contentState: "ready" | "missing" | "unavailable" = "ready";
      let reason = "内容与操作来源一致";
      if (item.status === "behind" && memberIndex > 0) {
        versionStatus = "behind";
        reason = "目标账号没有独有改动，来源账号新增 3 条，可以安全同步";
      } else if (item.status === "diverge") {
        versionStatus = "diverge";
        reason = sharedBaseScenario
          ? memberIndex < 3 ? "与另外两个账号处于相同旧版；两条独立分支都包含此内容" : "从共同旧版新增了内容；与另一条分支的更新不同"
          : "双方都有更新；覆盖会替换目标账号的完整内容";
      } else if (item.status === "missing" && memberIndex > 0) {
        versionStatus = "missing";
        contentState = "missing";
        reason = "目标会话正文不存在，无法确认或同步";
      }
      return {
        memberId: `${groupId}-member-${memberIndex + 1}`,
        accountId: account.id,
        uid: account.uid ?? `demo-uid-${memberIndex}`,
        sessionId: `${groupId}-session-${memberIndex + 1}`,
        accountName: account.nickname || account.email || account.uid || "演示账号",
        variant: accountVariant(account),
        linkState: "active" as const,
        versionStatus,
        title: item.title,
        projectLabel: client === "workbuddy" ? "项目 wb-switch" : "工作区 3c1f8a92",
        updatedAt: Date.now() - (sharedBaseScenario ? memberIndex < 3 ? 8 + memberIndex : memberIndex - 3 : memberIndex + groupIndex * 2) * 1000 * 60 * 37,
        recordCount: contentState === "ready" ? (sharedBaseScenario ? memberIndex < 3 ? 14 : memberIndex === 3 ? 17 : 19 : item.status === "latest" ? 14 : Math.max(4, 14 - memberIndex * 3)) : null,
        contentPreview: contentState === "ready" ? [
          { speaker: "用户", text: item.status === "diverge" ? "整理长期未完成的任务，并确认下一步安排。" : "继续处理这个会话。" },
          { speaker: "助手", text: sharedBaseScenario ? memberIndex < 3 ? "已整理共同旧版的待跟进事项。" : memberIndex === 3 ? "已补充国内版分支的处理记录。" : "已补充国际版分支的处理记录。" : item.status === "diverge" ? (memberIndex === 0 ? "已整理待跟进事项，建议先处理账号同步与会话列表。" : "已补充测试和发布前检查，建议先核对剩余问题。") : "已记录当前进度。" },
        ] : [],
        contentState,
        reason,
        canBeSource: contentState === "ready",
      };
    });
    const summary: SessionGroupSummary = {
      key: `${client}:${scope ?? "all"}:${groupId}`,
      client,
      variantScope: scope,
      groupId,
      groupVariant: scope ?? members[0].variant,
      title: item.title,
      projectLabel: members[0].projectLabel,
      latestActivityAt: Math.max(...members.map((member) => member.updatedAt)),
      memberCount: members.length,
      activeMemberCount: members.length,
      accountNames: members.map((member) => member.accountName),
      summaryStatus: item.status,
      summaryText: item.status === "behind" ? "有副本落后，可安全同步" : item.status === "latest" ? "关联副本内容一致" : sharedBaseScenario ? "3 个账号在共同旧版，另有 2 条独立更新" : item.status === "diverge" ? "多个副本有不同更新，需要选择来源" : "有副本内容缺失",
      safeSourceMemberId: item.status === "behind" || item.status === "latest" ? members[0].memberId : null,
      hasSafeSource: item.status === "behind" || item.status === "latest",
    };
    const linkedAccountIds = new Set(members.map((member) => member.accountId));
    return {
      ...summary,
      members,
      addTargets: candidates.filter((account) => !linkedAccountIds.has(account.id)),
      ...(sharedBaseScenario ? { divergence: { commonMemberIds: members.slice(0, 3).map((member) => member.memberId), branches: members.slice(3).map((member) => [member.memberId]) } } : {}),
    };
  });
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
  const groupClient = args?.client as SessionGroupClient | undefined;
  const groupScope = args?.variantScope === "ai" ? "ai" : args?.variantScope === "cn" ? "cn" : null;
  const demoGroups = groupClient ? buildDemoSessionGroups(groupClient, groupScope, demoAccounts) : [];
  switch (command) {
    // 档位随请求回显：两个档位各有一套演示账号，国际版同样回显「运行中 + 当前账号」。
    case "get_status": {
      const variant = normalizeVariant(args?.variant);
      if (variant === "ai") {
        return {
          ...appStatus,
          running: true,
          current: { uid: intlAccountA.uid, nickname: intlAccountA.nickname, email: intlAccountA.email },
          authFile: "/demo/workbuddy-ai/auth.json",
          appPath: "/demo/WorkBuddy AI.app",
          variant,
        };
      }
      return { ...appStatus, variant };
    }
    case "get_accounts": return { accounts: demoAccounts };
    // 会话管理页：按账号返回演示会话；国际版账号给一套不同 cwd，混排时可见档位差异。
    case "list_account_sessions": {
      const account = demoAccounts.find((item) => item.id === args?.accountId) ?? demoAccounts[0];
      return {
        sessions: demoAccountSessions(account.id === intlAccountA.id),
        current: account.uid ?? "demo-uid",
        variant: accountVariant(account),
      };
    }
    case "list_session_groups": {
      if (!groupClient || (groupClient === "codebuddyIde" && !groupScope) || (groupClient !== "codebuddyIde" && groupScope)) {
        throw new Error("会话客户端或档位参数无效");
      }
      const groups = demoGroups.map(({ members: _members, addTargets: _targets, divergence: _divergence, ...summary }) => summary);
      return { client: groupClient, variantScope: groupScope, storeStatus: "ready", groups } satisfies SessionGroupList;
    }
    case "get_session_group": {
      if (!groupClient || (groupClient === "codebuddyIde" && !groupScope) || (groupClient !== "codebuddyIde" && groupScope)) {
        throw new Error("会话客户端或档位参数无效");
      }
      const groupId = String(args?.groupId ?? "");
      const selected = demoGroups.find((group) => group.groupId === groupId);
      if (!selected) throw new Error("演示会话组不存在");
      return selected satisfies SessionGroupDetail;
    }
    case "preview_session_group_pair": {
      if (!groupClient || (groupClient === "codebuddyIde" && !groupScope) || (groupClient !== "codebuddyIde" && groupScope)) {
        throw new Error("会话客户端或档位参数无效");
      }
      const group = demoGroups.find((item) => item.groupId === args?.groupId);
      const sourceMemberId = String(args?.sourceMemberId ?? "");
      const targetMemberId = String(args?.targetMemberId ?? "");
      const source = group?.members.find((member) => member.memberId === sourceMemberId);
      const target = group?.members.find((member) => member.memberId === targetMemberId);
      if (!group || !source || !target || sourceMemberId === targetMemberId) throw new Error("来源或目标成员无效");
      const verdict = target.versionStatus === "behind" ? "fastForward" : target.versionStatus === "diverge" ? "diverge" : target.versionStatus === "missing" || target.versionStatus === "unknown" ? "unknown" : "identical";
      return {
        client: groupClient,
        variantScope: groupScope,
        groupId: group.groupId,
        sourceMemberId,
        targetMemberId,
        verdict,
        availableModes: verdict === "fastForward" ? ["fastForward"] : verdict === "diverge" ? ["overwrite"] : [],
        previewToken: verdict === "fastForward" || verdict === "diverge" ? `demo-preview-${group.groupId}-${targetMemberId}` : null,
        reason: target.reason,
        recordCount: { source: source.recordCount ?? 0, target: target.recordCount ?? 0, baseline: null },
        extraTargetCount: verdict === "diverge" ? 2 : 0,
      } satisfies SessionGroupPairPreview;
    }
    case "get_codebuddy_cli_status": return cliStatus;
    // 让演示里存在一个「CodeBuddy IDE 当前账号」：否则 IDE 标记与选中态染色（淡紫）在演示里永远不可见。
    // 取第二个账号，使三张卡各自演示一种形态（A 占位行 / B IDE 选中 / C 查看全部）。
    case "get_codebuddy_cn_ide_status": return {
      installed: true,
      running: true,
      loggedIn: true,
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
      loggedIn: true,
      dataDir: "/demo/codebuddy-ide",
      dbPath: "/demo/codebuddy-ide/state.vscdb",
      dbExists: true,
      appPath: "/demo/CodeBuddy IDE.app",
      activeAccountId: intlAccountA.id,
      activeAccountName: intlAccountA.nickname,
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
    // 国内版 / 国际版 IDE 共用同一套会话存储，演示数据也只有来源 uid 不同。
    case "list_codebuddy_ide_sessions": return demoIdeSessionList(demoAccounts[0].uid ?? "demo-source");
    case "list_codebuddy_intl_ide_sessions": return demoIdeSessionList(intlAccountA.uid ?? "demo-intl-source");
    // 关联预览：演示库没有复制记录，返回 missing（弹窗默认 tab 会拉一次；不得落到「演示模式不可操作」）。
    // 会话管理页的副本状态（演示）：三条会话分别展示「目标落后 / 已一致 / 两边都有改动」。
    case "session_links_preview_cross": {
      const source = demoAccounts.find((account) => account.id === args?.sourceAccountId) ?? demoAccounts[0];
      const target = demoAccounts.find((account) => account.id === args?.targetAccountId) ?? demoAccounts[1] ?? demoAccounts[0];
      return {
        supported: true,
        storeStatus: "ready",
        sourceUid: source.uid ?? "demo-source",
        targetUid: target.uid ?? "demo-target",
        sourceVariant: accountVariant(source),
        targetVariant: accountVariant(target),
        groups: [
          demoPreviewGroup("demo-sess-space-1", "跨档复制内核改造", "fastForward", 3, 0),
          demoPreviewGroup("demo-sess-space-2", "会话管理页骨架", "identical", 0, 0),
          demoPreviewGroup("demo-sess-task-1", "整理会话复制文案", "diverge", 4, 2),
        ],
      } satisfies SessionLinksPreview;
    }
    case "codebuddy_ide_session_links_preview":
    case "codebuddy_intl_ide_session_links_preview":
    case "vscode_session_links_preview": {
      const target = demoAccounts.find((account) => account.id === args?.targetAccountId) ?? demoAccounts[1] ?? demoAccounts[0];
      return {
        supported: true,
        storeStatus: "missing",
        sourceUid: demoAccounts[0].uid ?? "demo-source",
        targetUid: target.uid ?? "demo-target",
        groups: [],
      } satisfies SessionLinksPreview;
    }
    case "switch_codebuddy_cli_account": {
      const target = demoAccounts.find((account) => account.id === args?.accountId);
      if (!target) throw new Error("账号不存在");
      demoActiveCliAccountId = target.id;
      return { ok: true, configured: true, synced: true, verified: true, activeIndex: demoAccounts.indexOf(target), activeAccountId: target.id, regionChanged: false, cliClosed: false, closedProcessCount: 0, message: "演示切换已完成" } satisfies CodeBuddyCliSwitchResult;
    }
    // 写操作：演示模式下直接改内存里的演示账号，保存后卡片/弹框立即可见。
    case "update_account_display": {
      const target = accounts.find((account) => account.id === args?.accountId);
      if (!target) throw new Error("账号不存在");
      const patch = (args?.patch ?? {}) as { note?: string | null; displayField?: DisplayField };
      if ("note" in patch) {
        const trimmed = typeof patch.note === "string" ? patch.note.trim() : "";
        target.note = trimmed || null;
      }
      if (patch.displayField) target.displayField = patch.displayField;
      return { ok: true, account: { ...target } };
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
