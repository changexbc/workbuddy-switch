import type {
  AccountMeta, AppStatus, AutoRotateConfig, CheckinConfig, CheckinLog,
  CodeBuddyCliStatus, CodeBuddyCliSwitchResult, CreditExpiry, CreditOfficialUsageModel, CreditStatistics,
  GithubConfig, RotateLog, RotateStatus, TokenStatistics, TokenStatsGroup, TokenStatsSource, TokenStatsTotals,
  TravelConfig, TravelStatus,
} from "./types";
import { demoModeEnabled } from "./demo-mode";

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

const accounts: AccountMeta[] = [
  { id: "demo-account-a", uid: "demo-user-001", email: "test-a@example.com", nickname: "测试 A", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null },
  { id: "demo-account-b", uid: "demo-user-002", email: "test-b@example.com", nickname: "测试 B", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null },
  { id: "demo-account-c", uid: "demo-user-003", email: "test-c@example.com", nickname: "测试 C", enterpriseName: "Demo Workspace", expiresAt: 0, refreshExpiresAt: 0, refreshedAt: 0, createdAt: 0, needsRelogin: false, needsReloginReason: null },
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

const creditPackages = [
  [
    ["CodeBuddy 个人版国内运营裂变包", 5000, 3186.4, 36],
    ["CodeBuddy 个人版积分包", 2400, 1180.75, 18],
    ["CodeBuddy 新用户体验包", 800, 386.4, 5],
    ["CodeBuddy 签到赠送积分", 300, 196.25, 11],
    ["CodeBuddy 活动奖励积分", 600, 428.6, 27],
  ],
  [
    ["CodeBuddy 个人版国内运营裂变包", 3600, 2468.2, 24],
    ["CodeBuddy 个人版积分包", 1800, 905.5, 42],
    ["CodeBuddy 新用户体验包", 500, 128.2, 7],
    ["CodeBuddy 签到赠送积分", 240, 174.35, 15],
    ["CodeBuddy 活动奖励积分", 400, 286.8, 31],
  ],
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
  return { enabled: true, keepalive_days: 7, lazy_refresh_hours: 12 };
}

function travelConfig(): TravelConfig {
  return { enabled: true };
}

function travelStatus(accountId: string): TravelStatus {
  const index = Math.max(0, accounts.findIndex((account) => account.id === accountId));
  // 演示三种状态：旅行中 / 已结束 / 无 Buddy
  if (index % 3 === 0) return { label: "traveling", rewardCredit: null };
  if (index % 3 === 1) return { label: "finished", rewardCredit: 20 };
  return { label: "no-buddy", rewardCredit: null };
}

function rotateConfig(): AutoRotateConfig {
  return { enabled: true, check_interval_minutes: 15, cooldown_minutes: 120, min_gap_hours: 24, min_urgency_hours: 72, active_guard_minutes: 30, min_remaining_credits: 50 };
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
  return { source, summary, models, projects, sessions, daily, hours, filesScanned: source === "workbuddy" ? 63 : 41, parseErrors: 0, coverageStartAt: now - 13 * 86_400_000, coverageEndAt: now };
}

function demoTokenStatistics(days?: number): TokenStatistics {
  return { generatedAt: Date.now(), rangeDays: days ?? null, sources: [demoTokenSource("workbuddy", 1), demoTokenSource("codebuddy-cli", 0.58)] };
}

/** Read-only demo response provider. It never reads or mutates real user data. */
export function screenshotDemoResponse(command: string, args?: Record<string, unknown>): unknown {
  const demoAccounts = hydratedAccounts();
  const appStatus: AppStatus = { running: true, authFile: "/demo/workbuddy/auth.json", current: { uid: demoAccounts[0].uid, nickname: demoAccounts[0].nickname, email: demoAccounts[0].email }, appPath: "/demo/WorkBuddy.app", version: "0.1.24" };
  const activeIndex = Math.max(0, demoAccounts.findIndex((account) => account.id === demoActiveCliAccountId));
  const activeAccount = demoAccounts[activeIndex] ?? demoAccounts[0];
  const cliStatus: CodeBuddyCliStatus = { configured: true, settingsPresent: true, helperPresent: true, helperSupportsAccountIds: true, activeIndex, activeAccountId: activeAccount.id, activeAccountName: activeAccount.nickname, accountCount: demoAccounts.length, statePath: "/demo/codebuddy-cli-state.json" };
  const config = rotateConfig();
  const rotateStatus: RotateStatus = { config, cliConfigured: true, activeAccountId: demoAccounts[0].id, activeAccountName: demoAccounts[0].nickname, lastCheckAt: atLocalTime(0, 9, 30), lastSwitchAt: atLocalTime(1, 16, 20) };
  const githubConfig: GithubConfig = { owner: "zhangjia", repo: "wb-switch", proxy: "" };
  switch (command) {
    case "get_status": return appStatus;
    case "get_accounts": return { accounts: demoAccounts };
    case "get_codebuddy_cli_status": return cliStatus;
    case "switch_codebuddy_cli_account": {
      const target = demoAccounts.find((account) => account.id === args?.accountId);
      if (!target) throw new Error("账号不存在");
      demoActiveCliAccountId = target.id;
      return { ok: true, configured: true, synced: true, verified: true, activeIndex: demoAccounts.indexOf(target), activeAccountId: target.id, message: "演示切换已完成" } satisfies CodeBuddyCliSwitchResult;
    }
    case "get_checkin_status": return { ok: true, todayCheckedIn: true };
    case "get_credit_expiry": return creditExpiry(String(args?.accountId ?? ""));
    case "get_credit_statistics": return buildStatistics();
    case "get_token_statistics": return demoTokenStatistics(typeof args?.days === "number" ? args.days : undefined);
    case "get_auto_checkin_config": return checkinConfig();
    case "get_checkin_logs": return { logs: checkinLogs() };
    case "get_travel_status": return travelStatus(String(args?.accountId ?? ""));
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
