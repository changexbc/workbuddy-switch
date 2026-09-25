import { useCallback, useEffect, useMemo, useState, type ComponentProps } from "react";
import { Bar, BarChart, CartesianGrid, Rectangle, XAxis, YAxis } from "recharts";
import {
  ArrowUpDown,
  CalendarDays,
  CalendarRange,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  Loader2,
  Sparkles,
  RefreshCw,
  TrendingDown,
  Users,
  // XCircle, // 最近事件卡片隐藏后未使用
  type LucideIcon,
} from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DemoAction } from "@/components/demo-action";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
} from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import * as api from "@/lib/api";
import { creditResourceName } from "@/lib/credit-package-names";
import { getStackedSegmentVisualLayout } from "@/lib/stacked-bar-visuals";
import type {
  CreditExpiry,
  CreditOfficialUsage,
  CreditOfficialUsageAccount,
  CreditOfficialUsageModel,
  CreditOfficialUsageRequest,
  CreditStatsAccount,
  CreditStatsDailyPoint,
  CreditStatsEvent,
  CreditStatistics,
  WbVariant,
} from "@/lib/types";
import { accountVariant, DEFAULT_VARIANT, normalizeVariant, variantLabel } from "@/lib/variant";
import { useAccountsStore } from "@/stores/accounts";

type RangeKey = "30d" | "today" | "7d" | "month";
type StatsVariantView = "all" | "cn" | "ai";

function statsVariantViewLabel(view: StatsVariantView): string {
  return view === "all" ? "全部" : variantLabel(view);
}

const VARIANT_VIEW_OPTIONS: { key: StatsVariantView; label: string }[] = [
  { key: "all", label: statsVariantViewLabel("all") },
  { key: "cn", label: statsVariantViewLabel("cn") },
  { key: "ai", label: statsVariantViewLabel("ai") },
];

const RANGE_OPTIONS: { key: RangeKey; label: string }[] = [
  { key: "30d", label: "近 30 天" },
  { key: "today", label: "今天" },
  { key: "7d", label: "近 7 天" },
  { key: "month", label: "本月" },
];

const USAGE_SHARE_RANGE_OPTIONS: { key: RangeKey; label: string }[] = [
  { key: "today", label: "今天" },
  { key: "7d", label: "近 7 天" },
  { key: "30d", label: "近 30 天" },
  { key: "month", label: "本月" },
];

/** 账号消耗构成的分组顺序，与顶部档位切换一致；固定顺序避免组序随消耗高低跳动。 */
const VARIANT_GROUP_ORDER: WbVariant[] = ["cn", "ai"];

/**
 * 读取指定范围的消耗值；官方账号不可用（字段为 null）时返回 null，调用方不计入占比分母。
 * 「近 30 天」没有对应的汇总字段，由 rangeUsage 从逐日数据求和。
 */
function usageShareValue(
  account: { usageToday?: number | null; usage7Days?: number | null; usageThisMonth?: number | null },
  daily: CreditStatsDailyPoint[] | undefined,
  range: RangeKey,
): number | null {
  const { usageToday, usage7Days, usageThisMonth } = account;
  if (usageToday == null || usage7Days == null || usageThisMonth == null) return null;
  return rangeUsage({ usageToday, usage7Days, usageThisMonth }, daily ?? [], range);
}

function dateKey(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

function dateDaysAgo(days: number): string {
  const date = new Date();
  date.setHours(12, 0, 0, 0);
  date.setDate(date.getDate() - days);
  return dateKey(date);
}

function formatCredits(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return "—";
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(value);
}

function formatDateTime(ts: number | null | undefined): string {
  if (ts === null || ts === undefined) return "—";
  return new Date(ts).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatDate(ts: number | null | undefined): string {
  if (ts === null || ts === undefined) return "—";
  return new Date(ts).toLocaleDateString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  });
}

function formatExpiryDateTime(ts: number | null | undefined): string {
  if (ts === null || ts === undefined) return "—";
  return new Date(ts).toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatChartDate(date: string): string {
  return date.slice(5).replace("-", "/");
}

function accountLabel(account: { accountName?: string | null; accountId: string }): string {
  return account.accountName || account.accountId;
}

/** 读取可选的后端档位标记；缺省（当前后端不下发）时返回 undefined，由调用方回退到映射表。 */
function taggedVariant(value: { variant?: unknown }): WbVariant | undefined {
  return value.variant === undefined ? undefined : normalizeVariant(value.variant);
}

function resolveAccountVariant(
  account: CreditStatsAccount,
  variantByAccountId: Map<string, WbVariant>,
): WbVariant {
  return taggedVariant(account) ?? variantByAccountId.get(account.accountId) ?? DEFAULT_VARIANT;
}

function resolveEventVariant(
  event: CreditStatsEvent,
  variantByAccountId: Map<string, WbVariant>,
): WbVariant {
  const tagged = taggedVariant(event);
  if (tagged) return tagged;
  if (event.accountId) return variantByAccountId.get(event.accountId) ?? DEFAULT_VARIANT;
  return DEFAULT_VARIANT;
}

function mergeOfficialModels(models: CreditOfficialUsageModel[]): CreditOfficialUsageModel[] {
  const usage = new Map<string, { requestCount: number; credit: number }>();
  for (const item of models) {
    const raw = item.model?.trim() ?? "";
    const name = !raw || raw === "—" ? "未知模型" : raw;
    const entry = usage.get(name) ?? { requestCount: 0, credit: 0 };
    entry.requestCount += item.requestCount;
    entry.credit += item.credit;
    usage.set(name, entry);
  }
  return [...usage.entries()]
    .map(([model, value]) => ({ model, requestCount: value.requestCount, credit: value.credit }))
    .sort(
      (left, right) =>
        right.credit - left.credit ||
        right.requestCount - left.requestCount ||
        left.model.localeCompare(right.model),
    );
}

function officialStatusFromAccounts(
  accounts: CreditOfficialUsageAccount[],
): CreditOfficialUsage["status"] {
  if (accounts.length === 0) return "unavailable";
  const okCount = accounts.filter((account) => account.ok).length;
  if (okCount === accounts.length) return "complete";
  if (okCount > 0) return "partial";
  return "unavailable";
}

/** 仅 `cn` / `ai` 视图调用；`all` 必须直接使用后端聚合值（D1）。 */
function recomputeLocalStats(
  stats: CreditStatistics,
  accountIds: Set<string>,
  viewVariant: WbVariant,
  variantByAccountId: Map<string, WbVariant>,
): CreditStatistics {
  const accounts = stats.accounts.filter((account) => accountIds.has(account.accountId));
  const events = stats.events.filter(
    (event) => resolveEventVariant(event, variantByAccountId) === viewVariant,
  );

  let currentRemaining = 0;
  let currentCapacity = 0;
  let usageToday = 0;
  let usage7Days = 0;
  let usageThisMonth = 0;
  const usageByDate = new Map<string, number>();
  for (const account of accounts) {
    usageToday += account.usageToday;
    usage7Days += account.usage7Days;
    usageThisMonth += account.usageThisMonth;
    // 与后端口径一致：currentRemaining / currentCapacity 只统计 isCurrent 账号
    if (account.isCurrent) {
      currentRemaining += account.currentRemaining ?? 0;
      currentCapacity += account.totalCapacity ?? 0;
    }
    for (const point of account.daily ?? []) {
      usageByDate.set(point.date, (usageByDate.get(point.date) ?? 0) + point.usage);
    }
  }

  const today = dateKey(new Date());
  let todaySuccess = 0;
  let todayAlready = 0;
  let todayFailed = 0;
  const checkedIn = new Set<string>();
  for (const event of events) {
    if (event.kind !== "checkin" || event.date !== today) continue;
    if (event.result === "success") todaySuccess += 1;
    else if (event.result === "already") todayAlready += 1;
    else todayFailed += 1;
    if ((event.result === "success" || event.result === "already") && event.accountId) {
      checkedIn.add(event.accountId);
    }
  }

  return {
    ...stats,
    summary: {
      currentRemaining,
      currentCapacity,
      usageToday,
      usage7Days,
      usageThisMonth,
      todayCheckedInAccounts: checkedIn.size,
      todaySuccess,
      todayAlready,
      todayFailed,
    },
    // 日期轴以后端 stats.daily 为基准，避免过滤后曲线变短
    daily: stats.daily.map((point) => ({
      date: point.date,
      usage: usageByDate.get(point.date) ?? 0,
    })),
    accounts,
    events,
  };
}

/** 仅 `cn` / `ai` 视图调用；官方账号无 variant，用 accountId 关联 stats 档位集合（D2）。 */
function recomputeOfficialUsage(
  official: CreditOfficialUsage,
  accountIds: Set<string>,
): CreditOfficialUsage {
  const accounts = official.accounts.filter((account) => accountIds.has(account.accountId));
  const usageByDate = new Map<string, number>();
  const modelsByDate = new Map<string, CreditOfficialUsageModel[]>();
  const mergedModels: CreditOfficialUsageModel[] = [];
  let usageToday = 0;
  let usage7Days = 0;
  let usageThisMonth = 0;
  for (const account of accounts) {
    usageToday += account.usageToday ?? 0;
    usage7Days += account.usage7Days ?? 0;
    usageThisMonth += account.usageThisMonth ?? 0;
    if (account.models) mergedModels.push(...account.models);
    for (const point of account.daily ?? []) {
      usageByDate.set(point.date, (usageByDate.get(point.date) ?? 0) + point.usage);
      if (point.models && point.models.length > 0) {
        const list = modelsByDate.get(point.date) ?? [];
        list.push(...point.models);
        modelsByDate.set(point.date, list);
      }
    }
  }
  const visibleIds = new Set(accounts.map((account) => account.accountId));

  return {
    ...official,
    status: officialStatusFromAccounts(accounts),
    summary: { usageToday, usage7Days, usageThisMonth },
    // 日期轴以官方 daily 既有序列为基准，保留范围内空日期
    daily: official.daily.map((point) => ({
      date: point.date,
      usage: usageByDate.get(point.date) ?? 0,
      models: mergeOfficialModels(modelsByDate.get(point.date) ?? []),
    })),
    models: mergeOfficialModels(mergedModels),
    accounts,
    requests: official.requests.filter((request) => visibleIds.has(request.accountId)),
    errors: official.errors.filter((error) => visibleIds.has(error.accountId)),
  };
}

function AccountFilterMenu({
  accounts,
  accountFilter,
  onAccountFilterChange,
  ariaLabel,
  allowAll = true,
}: {
  accounts: { accountId: string; accountName?: string | null }[];
  accountFilter: string | null;
  onAccountFilterChange: (accountId: string | null) => void;
  ariaLabel: string;
  /** false 时隐藏「所有账号」选项，仅允许选择具体账号 */
  allowAll?: boolean;
}) {
  const activeFilterAccount =
    accountFilter && accounts.some((account) => account.accountId === accountFilter)
      ? accounts.find((account) => account.accountId === accountFilter)
      : undefined;
  const effectiveFilter = activeFilterAccount?.accountId ?? null;

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="h-8 max-w-[190px] gap-1.5 px-2.5 text-xs text-muted-foreground hover:text-foreground"
          aria-label={ariaLabel}
        >
          <Users className="size-3.5 shrink-0" />
          <span className="truncate">
            {activeFilterAccount
              ? accountLabel(activeFilterAccount)
              : allowAll
                ? "所有账号"
                : accounts[0]
                  ? accountLabel(accounts[0])
                  : "无账号"}
          </span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="max-h-80 w-56 overflow-y-auto">
        {allowAll && (
          <>
            <DropdownMenuItem onSelect={() => onAccountFilterChange(null)}>
              <Users className="size-3.5 shrink-0" />
              所有账号
              {!effectiveFilter && <Check className="ml-auto size-3.5 shrink-0" />}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
          </>
        )}
        {accounts.map((account) => (
          <DropdownMenuItem key={account.accountId} onSelect={() => onAccountFilterChange(account.accountId)}>
            <span className="min-w-0 flex-1 truncate">{accountLabel(account)}</span>
            {effectiveFilter === account.accountId && <Check className="ml-auto size-3.5 shrink-0" />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function isOfficialUsageAvailable(officialUsage?: CreditOfficialUsage): boolean {
  return officialUsage?.status === "complete" || officialUsage?.status === "partial";
}

function officialAccountFor(
  officialUsage: CreditOfficialUsage | undefined,
  accountId: string,
): CreditOfficialUsageAccount | undefined {
  return officialUsage?.accounts.find((account) => account.accountId === accountId);
}

function chartPoints(daily: CreditStatsDailyPoint[], range: RangeKey) {
  const today = dateKey(new Date());
  const firstDate = range === "today" ? today : range === "7d" ? dateDaysAgo(6) : dateDaysAgo(29);
  return daily.filter((point) => {
    if (range === "month") {
      return point.date.startsWith(`${today.slice(0, 7)}-`);
    }
    return point.date >= firstDate && point.date <= today;
  });
}

function rangeUsage(
  summary: CreditStatistics["summary"] | CreditOfficialUsage["summary"],
  daily: CreditStatsDailyPoint[],
  range: RangeKey,
): number {
  switch (range) {
    case "today":
      return summary.usageToday;
    case "7d":
      return summary.usage7Days;
    case "month":
      return summary.usageThisMonth;
    case "30d":
      return daily
        .filter((point) => point.date >= dateDaysAgo(29) && point.date <= dateKey(new Date()))
        .reduce((sum, point) => sum + point.usage, 0);
  }
}

/* 最近事件卡片隐藏后 checkinLabel 一并停用。恢复时取消本注释。
function checkinLabel(result: string | null | undefined): string {
  switch (result) {
    case "success":
      return "签到成功";
    case "already":
      return "已签到";
    case "error":
      return "签到失败";
    default:
      return "暂无记录";
  }
}
*/

/* 仅账号积分明细表使用，表隐藏期间一并注释。
function checkinBadgeVariant(
  result: string | null | undefined,
): "success" | "warning" | "destructive" | "outline" {
  switch (result) {
    case "success":
    case "already":
      return "success";
    case "error":
      return "destructive";
    default:
      return "outline";
  }
}
*/

function StatMetric({
  icon: Icon,
  label,
  value,
  divided = false,
}: {
  icon: LucideIcon;
  label: string;
  value: string;
  divided?: boolean;
}) {
  return (
    <div
      className={`flex min-w-0 flex-col items-center justify-center px-4 py-5 text-center sm:py-3 ${
        divided ? "sm:border-l sm:border-border/60" : ""
      }`}
    >
      <div className="flex max-w-full items-center justify-center gap-2 text-[13px] font-medium leading-5 text-muted-foreground">
        <Icon className="size-4 shrink-0 stroke-[1.75]" aria-hidden="true" />
        <span className="truncate">{label}</span>
      </div>
      <div className="mt-3 max-w-full truncate text-[26px] font-semibold leading-8 tracking-[-0.025em] text-foreground tabular-nums" style={{ fontFamily: '"Bricolage Grotesque Variable", "SF Pro Display", ui-sans-serif, sans-serif' }}>
        {value}
      </div>
    </div>
  );
}

/** 模型趋势共享数据色板；颜色由浅色/深色主题 token 提供。 */
const MODEL_COLORS = [
  "var(--data-series-emerald)",
  "var(--data-series-teal)",
  "var(--data-series-violet)",
  "var(--data-series-amber)",
  "var(--data-series-rose)",
  "var(--data-series-indigo)",
  "var(--data-series-sky)",
  "var(--data-series-lime)",
  "var(--data-series-orange)",
  "var(--data-series-pink)",
  "var(--data-series-cyan)",
  "var(--data-series-slate)",
];

interface ModelChartPoint {
  date: string;
  total: number;
  [model: string]: number | string;
}

type CreditBarShapeProps = ComponentProps<typeof Rectangle> & {
  segmentKey: string;
  seriesKeys: string[];
  payload?: ModelChartPoint;
  value?: number | [number, number];
};

function CreditBarShape({
  segmentKey,
  seriesKeys,
  payload,
  x = 0,
  y = 0,
  width = 0,
  height = 0,
  value,
  fill,
  stroke,
  strokeWidth,
  ...rest
}: CreditBarShapeProps) {
  if (width <= 0 || height <= 0) return null;
  const segmentIndex = seriesKeys.indexOf(segmentKey);
  const stackStart = Array.isArray(value) ? Number(value[0]) : 0;
  const layout = payload
    ? getStackedSegmentVisualLayout({
        values: seriesKeys.map((key) => Number(payload[key] ?? 0)),
        segmentIndex,
        segmentHeight: height,
        segmentY: y,
        stackStart,
      })
    : null;
  return (
    <Rectangle
      {...rest}
      x={x}
      y={layout?.y ?? y}
      width={width}
      height={layout?.height ?? height}
      fill={fill}
      radius={layout?.isTop ? [6, 6, 0, 0] : 0}
      stroke={stroke ?? "var(--background)"}
      strokeWidth={strokeWidth ?? 2}
    />
  );
}

/** 从官方 daily（全量按模型聚合）构建层叠数据；模型按总消耗降序全部保留。 */
function buildStackedChart(
  daily: CreditStatsDailyPoint[],
): { models: string[]; points: ModelChartPoint[] } {
  const modelTotals = new Map<string, number>();
  for (const point of daily) {
    for (const model of point.models ?? []) {
      modelTotals.set(model.model, (modelTotals.get(model.model) ?? 0) + model.credit);
    }
  }
  const models = [...modelTotals.entries()]
    .sort((left, right) => right[1] - left[1])
    .map(([model]) => model);

  const points: ModelChartPoint[] = daily.map((point) => {
    const entry: ModelChartPoint = { date: point.date, total: point.usage };
    for (const model of point.models ?? []) {
      const key = model.model;
      entry[key] = (typeof entry[key] === "number" ? entry[key] : 0) + model.credit;
    }
    return entry;
  });
  return { models, points };
}

function TrendChart({
  stats,
  officialUsage,
  accountFilter,
  onAccountFilterChange,
}: {
  stats: CreditStatistics;
  officialUsage?: CreditOfficialUsage;
  /** 与同页其他模块共享，null = 所有账号汇总 */
  accountFilter: string | null;
  onAccountFilterChange: (accountId: string | null) => void;
}) {
  /** 本卡片独立的时间范围，不影响其他卡片 */
  const [range, setRange] = useState<RangeKey>("30d");
  const official = isOfficialUsageAvailable(officialUsage) ? officialUsage : undefined;
  const officialAvailable = Boolean(official);
  const filterAccounts = official ? official.accounts : stats.accounts;
  const activeFilterAccount =
    accountFilter && filterAccounts.some((account) => account.accountId === accountFilter)
      ? filterAccounts.find((account) => account.accountId === accountFilter)
      : undefined;
  const effectiveFilter = activeFilterAccount?.accountId ?? null;
  const officialAccount = effectiveFilter ? officialAccountFor(official, effectiveFilter) : undefined;
  const localAccount = effectiveFilter
    ? stats.accounts.find((account) => account.accountId === effectiveFilter)
    : undefined;
  // 选中账号时切到该账号的逐日数据与汇总；否则用全部账号的聚合
  const daily = official
    ? (officialAccount ? officialAccount.daily ?? [] : official.daily)
    : (localAccount ? localAccount.daily ?? [] : stats.daily);
  const summary = official
    ? (officialAccount
        ? {
            usageToday: officialAccount.usageToday ?? 0,
            usage7Days: officialAccount.usage7Days ?? 0,
            usageThisMonth: officialAccount.usageThisMonth ?? 0,
          }
        : official.summary)
    : (localAccount
        ? {
            usageToday: localAccount.usageToday,
            usage7Days: localAccount.usage7Days,
            usageThisMonth: localAccount.usageThisMonth,
          }
        : stats.summary);
  const basePoints = chartPoints(daily, range);
  const hasDataSource = officialAvailable || Boolean(stats.coverageStartAt);

  // 官方 daily 带全量模型聚合 → 层叠柱（按模型）；否则单层「本地观察」柱
  const hasModelDetail = (official?.daily ?? []).some((point) => (point.models?.length ?? 0) > 0);
  const stacked = official && hasModelDetail ? buildStackedChart(basePoints) : null;
  const chartData: ModelChartPoint[] = stacked
    ? stacked.points
    : basePoints.map((point) => ({ date: point.date, total: point.usage }));

  // 单层本地数据实际使用 `total` 字段；官方数据才按模型名称分层。
  const series = stacked ? stacked.models : ["total"];
  const chartConfig: ChartConfig = {};
  for (const model of series) {
    chartConfig[model] = {
      label: model === "total" ? "总消耗" : model,
      ...(stacked
        ? { color: MODEL_COLORS[series.indexOf(model) % MODEL_COLORS.length] }
        : { color: "var(--data-series-emerald)" }),
    };
  }

  const hasObservedUsage = chartData.some((point) => point.total > 0);
  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="trend-chart-title">
      <div className="px-1">
        <h2 id="trend-chart-title" className="text-[13px] font-medium leading-5">
          {officialAvailable ? "官方积分消耗" : "本地观察积分消耗"}
        </h2>
      </div>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
          <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <CardDescription className="min-w-0 text-xs">
              {officialAvailable
                ? `来自 WorkBuddy 官方请求用量 · ${official?.rangeStart} 至 ${official?.rangeEnd}`
                : "只统计连续快照中余额下降的正差值；官方用量暂不可用时保留此口径。"}
            </CardDescription>
            <div className="flex max-w-full flex-wrap items-center gap-1.5">
              <AccountFilterMenu
                accounts={filterAccounts}
                accountFilter={effectiveFilter}
                onAccountFilterChange={onAccountFilterChange}
                ariaLabel="按账号筛选趋势"
              />
              <div className="flex max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1" aria-label="趋势范围">
                {RANGE_OPTIONS.map((option) => (
                  <button
                    key={option.key}
                    type="button"
                    className={`cursor-pointer rounded-md px-2.5 py-1.5 text-xs transition-colors ${
                      range === option.key
                        ? "bg-background font-medium text-foreground shadow-sm"
                        : "text-muted-foreground hover:text-foreground"
                    }`}
                    onClick={() => setRange(option.key)}
                    aria-pressed={range === option.key}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>
          </div>
        </CardHeader>
        <CardContent className="min-w-0 px-4 pt-3 pb-4 sm:px-5">
        {!hasDataSource ? (
          <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
            尚无积分快照。首次成功采集后，统计会从该时刻开始累计。
          </div>
        ) : chartData.length === 0 ? (
          <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
            当前口径暂无可展示的观察数据。
          </div>
        ) : (
          <>
            <ChartContainer config={chartConfig} className="h-56 w-full">
              <BarChart data={chartData} margin={{ top: 8, right: 8, left: 0, bottom: 0 }}>
                <CartesianGrid vertical={false} strokeDasharray="3 3" />
                <XAxis
                  dataKey="date"
                  tickLine={false}
                  axisLine={false}
                  tickMargin={8}
                  tickFormatter={(value) => formatChartDate(String(value))}
                />
                <YAxis tickLine={false} axisLine={false} width={42} tickFormatter={(value) => formatCredits(value)} />
                <ChartTooltip
                  cursor={{ fill: "var(--muted)", opacity: 0.4 }}
                  content={
                    <ChartTooltipContent
                      labelFormatter={(_, payload) => {
                        const item = Array.isArray(payload) ? payload[0] : payload;
                        return `${formatChartDate(String(item?.payload?.date ?? ""))} 消耗`;
                      }}
                    />
                  }
                />
                {series.map((model, index) => (
                  <Bar
                    key={model}
                    dataKey={model}
                    stackId="usage"
                    fill={stacked ? MODEL_COLORS[index % MODEL_COLORS.length] : "var(--color-total)"}
                    stroke="var(--background)"
                    strokeWidth={2}
                    maxBarSize={28}
                    shape={<CreditBarShape segmentKey={model} seriesKeys={series} />}
                    isAnimationActive={false}
                  />
                ))}
              </BarChart>
            </ChartContainer>
            {stacked && (
              <div className="mt-3 flex flex-wrap items-center justify-center gap-x-4 gap-y-1.5 text-xs text-muted-foreground">
                {stacked.models.map((model, index) => (
                  <span key={model} className="inline-flex items-center gap-1.5">
                    <span className="h-2 w-2 shrink-0 rounded-[2px]" style={{ backgroundColor: MODEL_COLORS[index % MODEL_COLORS.length] }} aria-hidden="true" />
                    {model}
                  </span>
                ))}
              </div>
            )}
            <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
              <span>
                {hasObservedUsage
                  ? `当前口径合计 ${formatCredits(rangeUsage(summary, daily, range))} 积分`
                  : officialAvailable
                    ? "官方已返回明细，当前范围暂无积分消耗"
                    : "已采集快照，暂未观察到余额下降"}
              </span>
              <span>{officialAvailable ? `数据更新于 ${formatDateTime(official?.collectedAt ?? stats.generatedAt)}` : `数据覆盖至 ${formatDate(stats.generatedAt)}`}</span>
            </div>
            <p className="sr-only">
              {chartData.map((point) => `${point.date} 消耗 ${formatCredits(point.total)} 积分`).join("；")}
            </p>
          </>
        )}
        </CardContent>
      </Card>
    </section>
  );
}

/* 与下方「积分明细」重复，先隐藏。恢复时取消本注释，并恢复页面中的 <AccountTable />。
function AccountTable({
  stats,
  officialUsage,
  selectedId,
  onSelect,
}: {
  stats: CreditStatistics;
  officialUsage?: CreditOfficialUsage;
  selectedId: string | null;
  onSelect: (id: string) => void;
}) {
  const official = isOfficialUsageAvailable(officialUsage) ? officialUsage : undefined;

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="account-table-title">
      <div className="px-1">
        <h2 id="account-table-title" className="text-[13px] font-medium leading-5">账号积分明细</h2>
      </div>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="border-b px-4 py-3 sm:px-5">
          <CardDescription className="text-xs">
            账号 ID 是统计关联键，名称只用于展示；官方用量优先，点击一行查看积分明细和事件。
          </CardDescription>
        </CardHeader>
        {stats.accounts.length === 0 ? (
        <div className="px-4 py-10 text-center text-sm text-muted-foreground">暂无账号统计。</div>
      ) : (
        <div className="min-w-0 overflow-x-auto">
          <table className="w-full min-w-[760px] text-left text-xs">
            <thead className="bg-muted/45 text-muted-foreground">
              <tr>
                <th className="px-4 py-3 font-medium sm:px-5">账号</th>
                <th className="px-3 py-3 text-right font-medium">当前剩余</th>
                <th className="px-3 py-3 text-right font-medium">今日消耗</th>
                <th className="px-3 py-3 text-right font-medium">近 7 天</th>
                <th className="px-3 py-3 text-right font-medium">本月</th>
                <th className="px-4 py-3 text-right font-medium sm:px-5">今日签到</th>
              </tr>
            </thead>
            <tbody>
              {stats.accounts.map((account) => {
                const selected = account.accountId === selectedId;
                const officialAccount = officialAccountFor(official, account.accountId);
                const usageToday = official ? officialAccount?.usageToday : account.usageToday;
                const usage7Days = official ? officialAccount?.usage7Days : account.usage7Days;
                const usageThisMonth = official ? officialAccount?.usageThisMonth : account.usageThisMonth;
                return (
                  <tr
                    key={account.accountId}
                    className={`border-t border-border/60 transition-colors ${selected ? "bg-primary/[0.06]" : "hover:bg-muted/35"}`}
                  >
                    <td className="max-w-[240px] px-4 py-3 sm:px-5">
                      <button
                        type="button"
                        className="min-w-0 max-w-full cursor-pointer text-left outline-none focus-visible:rounded-md focus-visible:ring-2 focus-visible:ring-ring"
                        onClick={() => onSelect(account.accountId)}
                      >
                        <span className="flex min-w-0 items-center gap-2">
                          <span className="min-w-0 truncate font-medium">{accountLabel(account)}</span>
                          {!account.isCurrent && (
                            <Badge variant="outline" className="shrink-0 px-1.5 py-0 text-[10px]">
                              历史
                            </Badge>
                          )}
                          {official && account.isCurrent && (
                            <Badge
                              variant={officialAccount?.ok ? "success" : "warning"}
                              className="shrink-0 px-1.5 py-0 text-[10px]"
                            >
                              {officialAccount?.ok ? "官方" : "不可用"}
                            </Badge>
                          )}
                        </span>
                        <span className="mt-0.5 block truncate text-[11px] text-muted-foreground">
                          {account.accountId}
                        </span>
                      </button>
                    </td>
                    <td className="px-3 py-3 text-right font-medium">
                      {formatCredits(account.currentRemaining)}
                    </td>
                    <td className="px-3 py-3 text-right">{formatCredits(usageToday)}</td>
                    <td className="px-3 py-3 text-right">{formatCredits(usage7Days)}</td>
                    <td className="px-3 py-3 text-right">{formatCredits(usageThisMonth)}</td>
                    <td className="px-4 py-3 text-right sm:px-5">
                      <Badge variant={checkinBadgeVariant(account.checkinStatusToday)}>
                        {checkinLabel(account.checkinStatusToday)}
                      </Badge>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      </Card>
    </section>
  );
}
*/

function ResourceBreakdown({ credit, loading }: { credit?: CreditExpiry; loading?: boolean }) {
  if (loading) {
    return (
      <div className="flex items-center gap-2 px-4 py-8 text-sm text-muted-foreground sm:px-5">
        <Loader2 className="size-4 animate-spin" />
        正在加载资源包…
      </div>
    );
  }
  if (!credit) {
    return <div className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">尚未采集当前资源包。</div>;
  }
  if (!credit.ok) {
    return (
      <div className="flex items-start gap-2 px-4 py-8 text-sm text-destructive sm:px-5">
        <CircleAlert className="mt-0.5 size-4 shrink-0" />
        <span>{credit.error || "积分资源查询失败"}</span>
      </div>
    );
  }
  const resources = credit.resources ?? [];
  if (resources.length === 0) {
    return <div className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">当前没有可展示的资源包。</div>;
  }

  return (
    <div className="divide-y divide-border/60">
      {resources.map((resource, index) => {
        const ratio = resource.total > 0 ? Math.min(100, Math.max(0, (resource.remaining / resource.total) * 100)) : 0;
        return (
          <div key={`${resource.packageCode || resource.packageName || "resource"}-${index}`} className="min-w-0 px-4 py-1.5 sm:px-5">
            <div className="flex min-w-0 items-center justify-between gap-2">
              <div className="min-w-0 truncate text-[13px] font-medium">{creditResourceName(resource, "未命名资源包")}</div>
              <div className="flex shrink-0 items-center gap-2.5">
                <span className="text-[11px] text-muted-foreground">
                  {resource.expired ? "已到期" : `到期 ${formatExpiryDateTime(resource.expireAt)}`}
                  {resource.used > 0 ? ` · 已用 ${formatCredits(resource.used)}` : ""}
                </span>
                <span className="text-xs font-medium">{formatCredits(resource.remaining)} / {formatCredits(resource.total)}</span>
              </div>
            </div>
            <div className="mt-1 h-1 overflow-hidden rounded-full bg-muted" aria-hidden="true">
              <div className="h-full rounded-full bg-primary/75" style={{ width: `${ratio}%` }} />
            </div>
          </div>
        );
      })}
    </div>
  );
}

function ModelBreakdownRows({ models }: { models: CreditOfficialUsageModel[] }) {
  const totalCredit = models.reduce((sum, model) => sum + model.credit, 0);
  const totalRequests = models.reduce((sum, model) => sum + model.requestCount, 0);

  return (
    <div className="space-y-3">
      {models.map((model) => {
        const ratio = totalCredit > 0 ? model.credit / totalCredit : totalRequests > 0 ? model.requestCount / totalRequests : 0;
        const percent = ratio * 100;
        const label = model.model === "—" ? "未知模型" : model.model;
        return (
          <div key={model.model} className="min-w-0">
            <div className="flex min-w-0 items-center justify-between gap-3 text-xs">
              <span className="min-w-0 truncate font-medium" title={label}>
                {label}
              </span>
              <span className="shrink-0 text-muted-foreground">
                {formatCredits(model.credit)} 积分 · {formatCredits(model.requestCount)} 次
                <span className="ml-1.5 font-medium text-foreground">
                  {percent < 0.05 ? "<0.1%" : `${percent.toFixed(1)}%`}
                </span>
              </span>
            </div>
            <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
              <div className="h-full rounded-full bg-primary/75" style={{ width: `${Math.min(100, Math.max(0, ratio * 100))}%` }} />
            </div>
          </div>
        );
      })}
    </div>
  );
}

function ModelBreakdown({
  officialUsage,
  accountFilter,
  onAccountFilterChange,
}: {
  officialUsage: CreditOfficialUsage;
  /** 与同页其他模块共享，null = 所有账号汇总 */
  accountFilter: string | null;
  onAccountFilterChange: (accountId: string | null) => void;
}) {
  /** 本卡片独立的时间范围，不影响其他卡片 */
  const [range, setRange] = useState<RangeKey>("30d");
  const filterAccounts = officialUsage.accounts;
  const activeFilterAccount =
    accountFilter && filterAccounts.some((account) => account.accountId === accountFilter)
      ? filterAccounts.find((account) => account.accountId === accountFilter)
      : undefined;
  const effectiveFilter = activeFilterAccount?.accountId ?? null;
  // 按选中账号 + 时间范围，从逐日模型聚合求和（全量，不受明细条数上限影响）
  const basePoints = chartPoints(
    effectiveFilter ? activeFilterAccount?.daily ?? [] : officialUsage.daily,
    range,
  );
  const rangeModelMap = new Map<string, { requestCount: number; credit: number }>();
  for (const point of basePoints) {
    for (const item of point.models ?? []) {
      const entry = rangeModelMap.get(item.model) ?? { requestCount: 0, credit: 0 };
      entry.requestCount += item.requestCount;
      entry.credit += item.credit;
      rangeModelMap.set(item.model, entry);
    }
  }
  const models = [...rangeModelMap.entries()]
    .map(([model, value]) => ({ model, requestCount: value.requestCount, credit: value.credit }))
    .sort(
      (a, b) =>
        b.credit - a.credit ||
        b.requestCount - a.requestCount ||
        a.model.localeCompare(b.model),
    );
  const totalCredit = models.reduce((sum, model) => sum + model.credit, 0);
  const totalRequests = models.reduce((sum, model) => sum + model.requestCount, 0);

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="model-breakdown-title">
      <div className="px-1">
        <h2 id="model-breakdown-title" className="text-[13px] font-medium leading-5">按模型分类</h2>
      </div>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
          <div className="flex min-w-0 flex-wrap items-center justify-between gap-2">
            <Badge variant="outline" className="shrink-0">
              {models.length} 个模型
            </Badge>
            <div className="flex flex-wrap items-center justify-end gap-1.5">
              <AccountFilterMenu
                accounts={filterAccounts}
                accountFilter={effectiveFilter}
                onAccountFilterChange={onAccountFilterChange}
                ariaLabel="按账号筛选模型分类"
              />
              <div className="flex max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1" aria-label="模型分类时间范围">
                {RANGE_OPTIONS.map((option) => (
                  <button
                    key={option.key}
                    type="button"
                    className={`cursor-pointer rounded-md px-2.5 py-1.5 text-xs transition-colors ${
                      range === option.key
                        ? "bg-background font-medium text-foreground shadow-sm"
                        : "text-muted-foreground hover:text-foreground"
                    }`}
                    onClick={() => setRange(option.key)}
                    aria-pressed={range === option.key}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>
          </div>
        </CardHeader>
        {models.length === 0 ? (
        <CardContent className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">
          {activeFilterAccount && !activeFilterAccount.ok ? "该账号官方用量暂不可用。" : "官方暂无可用的模型消耗明细。"}
        </CardContent>
      ) : (
        <CardContent className="px-4 pt-3 pb-4 sm:px-5">
          <div className="mb-4 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
            <span>共 {formatCredits(totalRequests)} 次请求</span>
            <span className="font-medium text-foreground">合计 {formatCredits(totalCredit)} 积分</span>
          </div>
          <ModelBreakdownRows models={models} />
        </CardContent>
      )}
      </Card>
    </section>
  );
}

function AccountUsageShare({
  stats,
  officialUsage,
  variantByAccountId,
}: {
  stats: CreditStatistics;
  officialUsage?: CreditOfficialUsage;
  variantByAccountId: Map<string, WbVariant>;
}) {
  /** 本卡片独立的时间范围，不影响其他卡片 */
  const [range, setRange] = useState<RangeKey>("today");
  const official = isOfficialUsageAvailable(officialUsage) ? officialUsage : undefined;

  // 官方用量优先，不可用时回退本地观察口径（与总览、趋势图一致）
  const rows = useMemo(() => {
    const source = official
      ? official.accounts.map((account) => ({
          accountId: account.accountId,
          accountName: account.accountName,
          usage: account.ok ? usageShareValue(account, account.daily, range) : null,
        }))
      : stats.accounts.map((account) => ({
          accountId: account.accountId,
          accountName: account.accountName,
          usage: usageShareValue(account, account.daily, range),
        }));
    return source
      .map((row) => ({ ...row, variant: variantByAccountId.get(row.accountId) ?? DEFAULT_VARIANT }))
      .sort(
        (a, b) =>
          (b.usage ?? -1) - (a.usage ?? -1) || accountLabel(a).localeCompare(accountLabel(b)),
      );
  }, [official, stats.accounts, range, variantByAccountId]);

  // 国内版与国际版积分体系不同，占比按档位分组、组内各算 100%；单档位视图下自然只有一组
  const groups = useMemo(() => {
    const grouped = new Map<WbVariant, typeof rows>();
    for (const row of rows) {
      const list = grouped.get(row.variant);
      if (list) list.push(row);
      else grouped.set(row.variant, [row]);
    }
    return [...grouped.entries()]
      .sort((a, b) => VARIANT_GROUP_ORDER.indexOf(a[0]) - VARIANT_GROUP_ORDER.indexOf(b[0]))
      .map(([variant, items]) => ({
        variant,
        items,
        total: items.reduce((sum, item) => sum + (item.usage ?? 0), 0),
      }));
  }, [rows]);

  // 官方账号失败时为 null，不能当 0 计入分母，否则占比会失真
  const measurable = rows.filter((row) => row.usage !== null);
  const grandTotal = groups.reduce((sum, group) => sum + group.total, 0);
  const showGroupLabel = groups.length > 1;

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="account-usage-share-title">
      <div className="px-1">
        <h2 id="account-usage-share-title" className="text-[13px] font-medium leading-5">
          账号消耗构成
        </h2>
      </div>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
          <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <CardDescription className="min-w-0 text-xs">
              {official
                ? `来自 WorkBuddy 官方请求用量 · ${official.rangeStart} 至 ${official.rangeEnd}`
                : "按本地观察口径；官方用量恢复后刷新即可切换。"}
            </CardDescription>
            <div
              className="flex max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1"
              aria-label="账号消耗构成范围"
            >
              {USAGE_SHARE_RANGE_OPTIONS.map((option) => (
                <button
                  key={option.key}
                  type="button"
                  className={`cursor-pointer rounded-md px-2.5 py-1.5 text-xs transition-colors ${
                    range === option.key
                      ? "bg-background font-medium text-foreground shadow-sm"
                      : "text-muted-foreground hover:text-foreground"
                  }`}
                  onClick={() => setRange(option.key)}
                  aria-pressed={range === option.key}
                >
                  {option.label}
                </button>
              ))}
            </div>
          </div>
        </CardHeader>
        {measurable.length === 0 ? (
          <CardContent className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">
            {official ? "当前账号的官方用量暂不可用，刷新后重试。" : "暂无账号消耗数据。"}
          </CardContent>
        ) : grandTotal <= 0 ? (
          <CardContent className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">
            当前范围暂未观察到积分消耗。
          </CardContent>
        ) : (
          <CardContent className="px-4 pt-3 pb-4 sm:px-5">
            <div className="mb-4 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
              <span>共 {rows.length} 个账号</span>
              <span className="font-medium text-foreground">合计 {formatCredits(grandTotal)} 积分</span>
            </div>
            <div className="space-y-4">
              {groups.map((group, index) => (
                <div
                  key={group.variant}
                  className={`min-w-0 space-y-3 ${index > 0 ? "border-t border-border/60 pt-4" : ""}`}
                >
                  {showGroupLabel && (
                    <div className="flex items-center justify-between gap-2">
                      <Badge variant="outline">{variantLabel(group.variant)}</Badge>
                      <span className="text-xs text-muted-foreground">
                        合计 {formatCredits(group.total)} 积分
                      </span>
                    </div>
                  )}
                  {group.items.map((row) => {
                    const ratio = row.usage !== null && group.total > 0 ? row.usage / group.total : 0;
                    const percent = ratio * 100;
                    return (
                      <div key={row.accountId} className="min-w-0">
                        <div className="flex min-w-0 items-center justify-between gap-3 text-xs">
                          <span className="min-w-0 truncate font-medium" title={accountLabel(row)}>
                            {accountLabel(row)}
                          </span>
                          <span className="shrink-0 text-muted-foreground">
                            {row.usage === null ? "官方用量不可用" : `${formatCredits(row.usage)} 积分`}
                            {row.usage !== null && (
                              <span className="ml-1.5 font-medium text-foreground">
                                {group.total > 0
                                  ? percent < 0.05
                                    ? "<0.1%"
                                    : `${percent.toFixed(1)}%`
                                  : "—"}
                              </span>
                            )}
                          </span>
                        </div>
                        <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
                          <div
                            className="h-full rounded-full bg-primary/75"
                            style={{ width: `${Math.min(100, Math.max(0, ratio * 100))}%` }}
                          />
                        </div>
                      </div>
                    );
                  })}
                </div>
              ))}
            </div>
          </CardContent>
        )}
      </Card>
    </section>
  );
}

function OfficialRequestRow({
  request,
  showAccount,
}: {
  request: CreditOfficialUsageRequest;
  showAccount: boolean;
}) {
  return (
    <tr className="border-t border-border/60 align-top">
      <td className="whitespace-nowrap px-3 py-3 text-muted-foreground">{request.requestTime}</td>
      {showAccount && (
        <td className="max-w-[140px] truncate px-3 py-3" title={request.accountName}>
          {request.accountName}
        </td>
      )}
      <td className="whitespace-nowrap px-3 py-3 text-right font-medium text-primary">
        {formatCredits(request.credit)}
      </td>
      <td className="max-w-[180px] truncate px-3 py-3" title={request.model}>
        {request.model}
      </td>
      <td className="max-w-[120px] truncate px-3 py-3 text-muted-foreground" title={request.client}>
        {request.client}
      </td>
      <td className="max-w-[170px] truncate px-3 py-3 font-mono text-[10px] text-muted-foreground" title={request.requestId}>
        {request.requestId}
      </td>
    </tr>
  );
}

/** 请求用量明细每页条数；明细由后端一次性返回，分页只控制单页渲染量 */
const REQUEST_PAGE_SIZE = 100;

/** 请求用量明细的排序维度：请求时间（默认倒序）与消耗 */
type RequestSortKey = "time" | "credit";
type SortDirection = "desc" | "asc";

/** 明细行的时间戳：requestTime 是「YYYY-MM-DD HH:mm:ss」文本，按字典序即时间序 */
function requestSortValue(request: CreditOfficialUsageRequest, key: RequestSortKey): number | string {
  if (key === "credit") return request.credit ?? 0;
  return request.requestTime ?? "";
}

function compareRequests(
  left: CreditOfficialUsageRequest,
  right: CreditOfficialUsageRequest,
  key: RequestSortKey,
  direction: SortDirection,
): number {
  const leftValue = requestSortValue(left, key);
  const rightValue = requestSortValue(right, key);
  let order: number;
  if (typeof leftValue === "number" && typeof rightValue === "number") {
    order = leftValue - rightValue;
  } else {
    order = String(leftValue).localeCompare(String(rightValue));
  }
  if (order === 0) {
    // 同值时用请求 ID 兜底，保证排序稳定（数组排序在 V8 中稳定，但仍显式保证）
    order = (left.requestId ?? "").localeCompare(right.requestId ?? "");
  }
  return direction === "desc" ? -order : order;
}

function SortableHeader({
  label,
  sortKey,
  activeKey,
  direction,
  onSort,
  align = "left",
}: {
  label: string;
  sortKey: RequestSortKey;
  activeKey: RequestSortKey;
  direction: SortDirection;
  onSort: (key: RequestSortKey) => void;
  align?: "left" | "right";
}) {
  const active = activeKey === sortKey;
  return (
    <th
      className={`px-3 py-2.5 font-medium ${align === "right" ? "text-right" : ""}`}
      aria-sort={active ? (direction === "desc" ? "descending" : "ascending") : "none"}
    >
      <button
        type="button"
        className={`inline-flex cursor-pointer items-center gap-1 transition-colors hover:text-foreground ${
          active ? "text-foreground" : ""
        } ${align === "right" ? "flex-row-reverse" : ""}`}
        onClick={() => onSort(sortKey)}
        title={active ? (direction === "desc" ? "当前降序，点击切换升序" : "当前升序，点击切换降序") : "点击排序"}
      >
        {label}
        <ArrowUpDown className={`size-3 shrink-0 ${active ? "opacity-100" : "opacity-40"}`} />
      </button>
    </th>
  );
}

function OfficialUsageBreakdown({
  officialUsage,
  accountId,
}: {
  officialUsage?: CreditOfficialUsage;
  accountId: string | null;
}) {
  const [sortKey, setSortKey] = useState<RequestSortKey>("time");
  const [sortDirection, setSortDirection] = useState<SortDirection>("desc");
  const [page, setPage] = useState(0);
  const officialAvailable = isOfficialUsageAvailable(officialUsage);

  // 换账号或换排序都回到第一页；重新采集后数据整体更换，同样重置
  useEffect(() => {
    setPage(0);
  }, [accountId, sortKey, sortDirection, officialUsage?.collectedAt]);

  const toggleSort = (key: RequestSortKey) => {
    if (key === sortKey) {
      setSortDirection((current) => (current === "desc" ? "asc" : "desc"));
      return;
    }
    setSortKey(key);
    // 新列默认从「最大」开始看：消耗列先看最高消耗，时间列看最近请求
    setSortDirection("desc");
  };
  const account = accountId ? officialAccountFor(officialUsage, accountId) : undefined;

  if (!officialAvailable || !officialUsage) {
    return (
      <div className="flex items-start gap-2 px-4 py-8 text-sm text-muted-foreground sm:px-5">
        <CircleAlert className="mt-0.5 size-4 shrink-0" />
        <span>官方请求用量暂不可用；总览已回退到本地观察数据，请稍后刷新重试。</span>
      </div>
    );
  }

  if (accountId && !account) {
    return <div className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">该账号暂无官方用量记录。</div>;
  }

  if (account && !account.ok) {
    return (
      <div className="flex items-start gap-2 px-4 py-8 text-sm text-destructive sm:px-5">
        <CircleAlert className="mt-0.5 size-4 shrink-0" />
        <span>{account.error || "该账号的官方请求用量查询失败"}</span>
      </div>
    );
  }

  const requests = account
    ? officialUsage.requests.filter((request) => request.accountId === account.accountId)
    : officialUsage.requests;
  const totalRequests = account
    ? (account.reportedTotal ?? account.requestCount)
    : officialUsage.accounts.reduce((sum, item) => sum + (item.reportedTotal ?? item.requestCount), 0);
  const detailTruncated = account
    ? account.detailTruncated
    : officialUsage.accounts.some((item) => item.detailTruncated);
  const showAccount = !account;

  const sortedRequests = [...requests].sort((left, right) =>
    compareRequests(left, right, sortKey, sortDirection),
  );
  const pageCount = Math.max(1, Math.ceil(sortedRequests.length / REQUEST_PAGE_SIZE));
  const safePage = Math.min(Math.max(page, 0), pageCount - 1);
  const pageStart = safePage * REQUEST_PAGE_SIZE;
  const pageRows = sortedRequests.slice(pageStart, pageStart + REQUEST_PAGE_SIZE);

  return (
    <div className="min-w-0">
      {detailTruncated && (
        <div className="flex items-start gap-2 border-b bg-amber-500/[0.06] px-4 py-2.5 text-xs text-amber-800 sm:px-5">
          <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
          <span>
            最多展示每账号最近 {officialUsage.detailLimitPerAccount} 条请求明细；合计使用官方返回的全部 {formatCredits(totalRequests)} 条请求。
          </span>
        </div>
      )}
      {requests.length === 0 ? (
        <div className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">
          {totalRequests > 0 ? "官方返回了请求总数，但明细未通过格式校验。" : "官方暂无请求用量。"}
        </div>
      ) : (
        <div className="min-w-0 overflow-x-auto">
          <table className="w-full min-w-[700px] text-left text-[11px]">
            <thead className="sticky top-0 bg-muted/95 text-muted-foreground">
              <tr>
                <SortableHeader
                  label="请求时间"
                  sortKey="time"
                  activeKey={sortKey}
                  direction={sortDirection}
                  onSort={toggleSort}
                />
                {showAccount && <th className="px-3 py-2.5 font-medium">账号</th>}
                <SortableHeader
                  label="消耗"
                  sortKey="credit"
                  activeKey={sortKey}
                  direction={sortDirection}
                  onSort={toggleSort}
                  align="right"
                />
                <th className="px-3 py-2.5 font-medium">模型</th>
                <th className="px-3 py-2.5 font-medium">客户端</th>
                <th className="px-3 py-2.5 font-medium">请求 ID</th>
              </tr>
            </thead>
            <tbody>
              {pageRows.map((request) => (
                <OfficialRequestRow
                  key={`${request.requestId}-${request.requestTime}`}
                  request={request}
                  showAccount={showAccount}
                />
              ))}
            </tbody>
          </table>
          {sortedRequests.length > REQUEST_PAGE_SIZE && (
            <div className="flex flex-wrap items-center justify-between gap-2 border-t px-3 py-2.5 text-xs text-muted-foreground sm:px-4">
              <span>
                第 {pageStart + 1}–{Math.min(pageStart + REQUEST_PAGE_SIZE, sortedRequests.length)} 条，共{" "}
                {sortedRequests.length} 条
              </span>
              <div className="flex items-center gap-1.5">
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 px-2 text-xs"
                  disabled={safePage === 0}
                  onClick={() => setPage(safePage - 1)}
                >
                  <ChevronLeft className="size-3.5" />
                  上一页
                </Button>
                <span className="tabular-nums">
                  {safePage + 1} / {pageCount}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 px-2 text-xs"
                  disabled={safePage >= pageCount - 1}
                  onClick={() => setPage(safePage + 1)}
                >
                  下一页
                  <ChevronRight className="size-3.5" />
                </Button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/* 最近事件卡片已隐藏，EventRow 一并停用。恢复时取消本注释。
function EventRow({ event }: { event: CreditStatsEvent }) {
  if (event.kind === "usage") {
    return (
      <div className="flex min-w-0 items-start gap-3 border-b border-border/60 py-3 last:border-b-0">
        <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary">
          <TrendingDown className="size-3.5" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1 text-xs">
            <span className="font-medium">观察到积分消耗</span>
            <span className="font-medium text-primary">-{formatCredits(event.amount)}</span>
          </div>
          <div className="mt-1 truncate text-[11px] text-muted-foreground">
            {event.accountName} · {formatDateTime(event.ts)}
          </div>
        </div>
      </div>
    );
  }

  const isError = event.result === "error";
  const isAlready = event.result === "already";
  return (
    <div className="flex min-w-0 items-start gap-3 border-b border-border/60 py-3 last:border-b-0">
      <span
        className={`mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-full ${
          isError ? "bg-destructive/10 text-destructive" : isAlready ? "bg-amber-500/10 text-amber-700" : "bg-emerald-500/10 text-emerald-700"
        }`}
      >
        {isError ? <XCircle className="size-3.5" /> : <CircleCheck className="size-3.5" />}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1 text-xs">
          <span className="font-medium">{checkinLabel(event.result)}</span>
          <span className="text-muted-foreground">{formatDateTime(event.ts)}</span>
        </div>
        <div className="mt-1 truncate text-[11px] text-muted-foreground">
          {event.accountName}{event.error ? ` · ${event.error}` : ""}
        </div>
      </div>
    </div>
  );
}
*/

function ResourcesByAccount({
  accounts,
  creditMap,
  creditLoadingMap,
}: {
  accounts: CreditStatsAccount[];
  creditMap: Record<string, CreditExpiry>;
  creditLoadingMap: Record<string, boolean>;
}) {
  if (accounts.length === 0) {
    return <div className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">暂无账号统计。</div>;
  }
  if (accounts.length === 1) {
    const account = accounts[0];
    return <ResourceBreakdown credit={creditMap[account.accountId]} loading={creditLoadingMap[account.accountId]} />;
  }
  return (
    <div className="divide-y divide-border/60">
      {accounts.map((account) => (
        <div key={account.accountId} className="min-w-0">
          <div className="px-4 py-2.5 text-xs font-medium sm:px-5">{accountLabel(account)}</div>
          <ResourceBreakdown credit={creditMap[account.accountId]} loading={creditLoadingMap[account.accountId]} />
        </div>
      ))}
    </div>
  );
}

function SelectedAccountDetails({
  stats,
  officialUsage,
  creditMap,
  creditLoadingMap,
  accountFilter,
  onAccountFilterChange,
}: {
  stats: CreditStatistics;
  officialUsage?: CreditOfficialUsage;
  creditMap: Record<string, CreditExpiry>;
  creditLoadingMap: Record<string, boolean>;
  /** 与同页其他模块共享；本卡片只允许单账号，未指定时回退到第一个账号 */
  accountFilter: string | null;
  onAccountFilterChange: (accountId: string | null) => void;
}) {
  const [detailTab, setDetailTab] = useState<"credits" | "requests">("credits");
  const official = isOfficialUsageAvailable(officialUsage) ? officialUsage : undefined;
  const filterAccounts = official ? official.accounts : stats.accounts;
  const activeFilterAccount =
    accountFilter && filterAccounts.some((account) => account.accountId === accountFilter)
      ? filterAccounts.find((account) => account.accountId === accountFilter)
      : undefined;
  // 筛选失效（如账号列表刷新变化）时回退到第一个账号
  const effectiveFilter = activeFilterAccount?.accountId ?? filterAccounts[0]?.accountId ?? null;
  const visibleAccounts = effectiveFilter
    ? stats.accounts.filter((account) => account.accountId === effectiveFilter)
    : stats.accounts;
  // 最近事件卡片已隐藏，events 不再使用。恢复时取消本注释。
  // const events = (effectiveFilter
  //   ? stats.events.filter((event) => event.accountId === effectiveFilter)
  //   : stats.events
  // ).slice(0, 50);
  const latestSnapshotAt = visibleAccounts.reduce<number | null>((latest, account) => {
    if (account.lastSnapshotAt == null) return latest;
    if (latest == null || account.lastSnapshotAt > latest) return account.lastSnapshotAt;
    return latest;
  }, null);

  useEffect(() => {
    setDetailTab("credits");
  }, [effectiveFilter]);

  return (
    <div className="flex min-w-0 flex-col gap-12">
      <section className="min-w-0 space-y-2.5" aria-labelledby="credit-detail-title">
        <div className="px-1">
          <h2 id="credit-detail-title" className="text-[13px] font-medium leading-5">积分明细</h2>
        </div>
        <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
          <CardHeader className="gap-0 border-b px-4 pt-3 pb-3 sm:px-5">
            <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
              <CardDescription className="min-w-0 truncate text-xs">
                {latestSnapshotAt ? `最近采集 ${formatDateTime(latestSnapshotAt)}` : "暂无账号资源包。"}
              </CardDescription>
              <AccountFilterMenu
                accounts={filterAccounts}
                accountFilter={effectiveFilter}
                onAccountFilterChange={onAccountFilterChange}
                ariaLabel="按账号筛选积分明细"
                allowAll={false}
              />
            </div>
            <div className="mt-3 flex max-w-full gap-1 rounded-lg bg-muted p-1" role="tablist" aria-label="积分详情类型">
              {(
                [
                  ["credits", "积分明细"],
                  ["requests", "请求用量"],
                ] as const
              ).map(([value, label]) => (
                <button
                  key={value}
                  type="button"
                  role="tab"
                  aria-selected={detailTab === value}
                  className={`min-w-0 flex-1 rounded-md px-2.5 py-1.5 text-xs transition-colors ${
                    detailTab === value
                      ? "bg-background font-medium text-foreground shadow-sm"
                      : "text-muted-foreground hover:text-foreground"
                  }`}
                  onClick={() => setDetailTab(value)}
                >
                  {label}
                </button>
              ))}
            </div>
          </CardHeader>
          {detailTab === "credits" ? (
            <div className="min-w-0">
              <ResourcesByAccount accounts={visibleAccounts} creditMap={creditMap} creditLoadingMap={creditLoadingMap} />
            </div>
          ) : (
            <OfficialUsageBreakdown officialUsage={officialUsage} accountId={effectiveFilter} />
          )}
        </Card>
      </section>
      {/* 最近事件卡片已隐藏。恢复时取消本注释。
      <section className="min-w-0 space-y-2.5" aria-labelledby="account-events-title">
        <div className="px-1">
          <h2 id="account-events-title" className="text-[13px] font-medium leading-5">最近事件</h2>
        </div>
        <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
          <CardHeader className="border-b px-4 py-3 sm:px-5">
            <CardDescription className="text-xs">签到单独记录，不会计入官方请求用量。</CardDescription>
          </CardHeader>
          <CardContent className="max-h-[340px] min-w-0 overflow-y-auto px-4 py-1 sm:px-5">
            {events.length === 0 ? (
              <div className="py-8 text-center text-sm text-muted-foreground">
                {effectiveFilter ? "该账号暂无最近事件。" : "暂无最近事件。"}
              </div>
            ) : (
              events.map((event, index) => <EventRow key={`${event.kind}-${event.ts}-${index}`} event={event} />)
            )}
          </CardContent>
        </Card>
      </section>
      */}
    </div>
  );
}

/* 积分明细默认展示全部账号后不再单独使用。
function UnselectedRecentEvents({ events }: { events: CreditStatsEvent[] }) {
  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="all-events-title">
      <div className="px-1">
        <h2 id="all-events-title" className="text-[13px] font-medium leading-5">最近事件</h2>
      </div>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="border-b px-4 py-3 sm:px-5">
          <CardDescription className="text-xs">签到与积分观察分开记录，签到不会计入消耗。</CardDescription>
        </CardHeader>
        <CardContent className="max-h-[340px] min-w-0 overflow-y-auto px-4 py-1 sm:px-5">
          {events.slice(0, 50).map((event, index) => (
            <EventRow key={`${event.kind}-${event.ts}-${index}`} event={event} />
          ))}
        </CardContent>
      </Card>
    </section>
  );
}
*/

/** 当前会话内共用一份统计数据；只有「刷新统计」才会重新采集。 */
let cachedStatistics: CreditStatistics | null = null;
let statisticsInflight: Promise<CreditStatistics> | null = null;

/** 进入统计页时距上次采集超过此时长（ms）则自动重新采集一次 */
const STATISTICS_AUTO_REFRESH_MS = 30 * 60 * 1000;

function rememberStatistics(next: CreditStatistics): CreditStatistics {
  cachedStatistics = next;
  return next;
}

function loadCachedStatistics(refresh: boolean): Promise<CreditStatistics> {
  if (!refresh && cachedStatistics) return Promise.resolve(cachedStatistics);
  if (!refresh && statisticsInflight) return statisticsInflight;
  const pending = api.getCreditStatistics(refresh).then(rememberStatistics);
  statisticsInflight = pending;
  return pending.finally(() => {
    if (statisticsInflight === pending) statisticsInflight = null;
  });
}

export default function CreditStatsPage() {
  const {
    accounts,
    creditMap,
    creditLoadingMap,
    fetchAll,
    refreshCredits,
  } = useAccountsStore();
  const [stats, setStats] = useState<CreditStatistics | null>(cachedStatistics);
  const [loading, setLoading] = useState(!cachedStatistics);
  const [error, setError] = useState<string | null>(null);
  const [viewVariant, setViewVariant] = useState<StatsVariantView>("all");
  /** 页面级共享：在任一模块切换账号，趋势 / 模型分类 / 积分明细三处同步联动 */
  const [sharedAccountFilter, setSharedAccountFilter] = useState<string | null>(null);

  const load = useCallback(
    async (refresh = false) => {
      if (!refresh && cachedStatistics) {
        setStats(cachedStatistics);
        setLoading(false);
        setError(null);
        return;
      }
      setLoading(true);
      setError(null);
      try {
        await fetchAll();
        const accountState = useAccountsStore.getState();
        if (accountState.error) {
          throw new Error(accountState.error);
        }
        const currentAccounts = accountState.accounts;
        const ids = currentAccounts.map((account) => account.id);
        if (refresh && ids.length > 0) {
          await refreshCredits(ids);
        }
        setStats(await loadCachedStatistics(refresh));
      } catch (cause) {
        setError(api.asError(cause));
      } finally {
        setLoading(false);
      }
    },
    [fetchAll, refreshCredits],
  );

  useEffect(() => {
    void (async () => {
      // 先渲染本地缓存（后端只读磁盘，不用等网络）
      await load(false);
      if (api.isDemoMode()) return;
      // 过期只看后端记录的采集时刻：会话内变量每次启动都归零，判断不出
      // 「缓存其实是几小时前采的」，所以刚打开应用时不会自动刷新。
      const collectedAt = cachedStatistics?.officialUsage?.collectedAt ?? 0;
      if (collectedAt > 0 && Date.now() - collectedAt < STATISTICS_AUTO_REFRESH_MS) return;
      if (useAccountsStore.getState().accounts.length === 0) return;
      await load(true);
    })();
  }, [load]);

  const variantByAccountId = useMemo(() => {
    const map = new Map<string, WbVariant>();
    for (const account of accounts) {
      map.set(account.id, accountVariant(account));
    }
    return map;
  }, [accounts]);

  const variantAccountIds = useMemo(() => {
    if (!stats || viewVariant === "all") return null;
    return new Set(
      stats.accounts
        .filter((account) => resolveAccountVariant(account, variantByAccountId) === viewVariant)
        .map((account) => account.accountId),
    );
  }, [stats, viewVariant, variantByAccountId]);

  const filteredStats = useMemo(() => {
    // D1: viewVariant === "all" 时直接沿用后端聚合值（stats.summary / stats.daily），
    // 不得走过滤重算路径，避免把国内版与国际版不可比积分加回一起，破坏改动前回归基线。
    if (viewVariant === "all" || !stats || !variantAccountIds) return stats;
    return recomputeLocalStats(stats, variantAccountIds, viewVariant, variantByAccountId);
  }, [stats, viewVariant, variantAccountIds, variantByAccountId]);

  const filteredOfficial = useMemo(() => {
    const officialUsage = stats?.officialUsage;
    // D1: 「全部」原样使用 officialUsage.summary / daily / models，不走过滤重算。
    if (viewVariant === "all" || !officialUsage || !variantAccountIds) return officialUsage;
    return recomputeOfficialUsage(officialUsage, variantAccountIds);
  }, [stats, viewVariant, variantAccountIds]);

  const officialUsage = filteredOfficial;
  const official = isOfficialUsageAvailable(officialUsage) ? officialUsage : undefined;
  const variantEmpty =
    viewVariant !== "all" && variantAccountIds !== null && variantAccountIds.size === 0;

  return (
    <div className="mx-auto w-full max-w-[1180px] min-w-0 px-4 py-6 sm:px-8 sm:py-9">
      <header className="mb-10 flex min-w-0 flex-wrap items-start justify-between gap-4 sm:mb-12">
        <div className="min-w-0">
          <h1 className="text-[28px] font-semibold tracking-tight">积分统计</h1>
          <p className="mt-2 max-w-2xl text-sm leading-6 text-muted-foreground">
            当前数据更新于 {stats ? formatDateTime(official?.collectedAt ?? stats.generatedAt) : "—"}
          </p>
        </div>
        <div className="flex min-w-0 max-w-full flex-wrap items-center justify-end gap-2">
          <div
            className="flex max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1"
            role="tablist"
            aria-label="档位筛选"
          >
            {VARIANT_VIEW_OPTIONS.map((option) => (
              <button
                key={option.key}
                type="button"
                role="tab"
                aria-selected={viewVariant === option.key}
                className={`cursor-pointer rounded-md px-2.5 py-1.5 text-xs transition-colors ${
                  viewVariant === option.key
                    ? "bg-background font-medium text-foreground shadow-sm"
                    : "text-muted-foreground hover:text-foreground"
                }`}
                onClick={() => setViewVariant(option.key)}
              >
                {option.label}
              </button>
            ))}
          </div>
          <DemoAction>
            <Button
              className="shrink-0"
              variant="outline"
              size="sm"
              onClick={() => void load(true)}
              disabled={loading}
            >
              {loading ? <Loader2 className="animate-spin" /> : <RefreshCw />}
              刷新统计
            </Button>
          </DemoAction>
        </div>
      </header>

      {error && (
        <Alert variant="destructive" className="mb-5">
          <CircleAlert />
          <AlertTitle>统计加载失败</AlertTitle>
          <AlertDescription className="flex flex-wrap items-center gap-3">
            <span>{error}</span>
            <Button size="sm" variant="outline" onClick={() => void load()}>
              重试
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {loading && !stats ? (
        <div className="flex items-center gap-2 py-20 text-sm text-muted-foreground">
          <Loader2 className="animate-spin" />
          正在采集账号积分并加载统计…
        </div>
      ) : stats && filteredStats ? (
        <div className="min-w-0 space-y-12">
          {viewVariant === "all" && accounts.length === 0 && (
            <Alert>
              <CircleAlert />
              <AlertTitle>暂无当前账号</AlertTitle>
              <AlertDescription>可以先去账号管理导入或登录账号；历史事件仍会保留在下方最近事件中。</AlertDescription>
            </Alert>
          )}

          {variantEmpty ? (
            <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
              <p>暂无{statsVariantViewLabel(viewVariant)}账号的积分数据。</p>
              <p className="mt-2 text-xs leading-5">国内版与国际版积分体系不同，不会合并计算。</p>
            </div>
          ) : (
            <>
              {officialUsage && officialUsage.status !== "complete" && (
                <Alert variant="warning">
                  <CircleAlert />
                  <AlertTitle>
                    {officialUsage.status === "partial" ? "部分账号官方用量未同步" : "官方用量暂不可用"}
                  </AlertTitle>
                  <AlertDescription>
                    {officialUsage.status === "partial"
                      ? `已同步 ${officialUsage.accounts.filter((account) => account.ok).length}/${officialUsage.accounts.length} 个当前账号；失败账号的官方数值显示为“—”。`
                      : "今日、近 7 天和本月消耗将使用本地观察口径；官方接口恢复后刷新即可重新同步。"}
                    {officialUsage.errors.length > 0 && (
                      <span className="text-xs text-amber-900/75">
                        {officialUsage.errors.map((item) => `${item.accountName}: ${item.error}`).join("；")}
                      </span>
                    )}
                  </AlertDescription>
                </Alert>
              )}

              <Card className="min-w-0 gap-0 overflow-hidden rounded-2xl bg-card/70 py-0 shadow-none" aria-label="积分总览">
                <CardContent className="grid min-w-0 grid-cols-1 divide-y divide-border/60 p-0 sm:grid-cols-4 sm:divide-y-0 sm:py-5">
                  <StatMetric
                    icon={Sparkles}
                    label="剩余积分"
                    value={formatCredits(filteredStats.summary.currentRemaining)}
                  />
                  <StatMetric
                    icon={TrendingDown}
                    label="今日消耗"
                    value={formatCredits(official ? official.summary.usageToday : filteredStats.summary.usageToday)}
                    divided
                  />
                  <StatMetric
                    icon={CalendarDays}
                    label="近 7 天消耗"
                    value={formatCredits(official ? official.summary.usage7Days : filteredStats.summary.usage7Days)}
                    divided
                  />
                  <StatMetric
                    icon={CalendarRange}
                    label="本月消耗"
                    value={formatCredits(official ? official.summary.usageThisMonth : filteredStats.summary.usageThisMonth)}
                    divided
                  />
                </CardContent>
              </Card>

              {!official && !filteredStats.coverageStartAt && filteredStats.events.some((event) => event.kind === "checkin") && (
                <Alert>
                  <CircleCheck />
                  <AlertTitle>目前只有签到记录</AlertTitle>
                  <AlertDescription>签到不会被计入积分消耗。首次成功采集积分资源后，趋势统计才会开始累计。</AlertDescription>
                </Alert>
              )}

              <AccountUsageShare
                stats={filteredStats}
                officialUsage={officialUsage}
                variantByAccountId={variantByAccountId}
              />

              <TrendChart
                stats={filteredStats}
                officialUsage={officialUsage}
                accountFilter={sharedAccountFilter}
                onAccountFilterChange={setSharedAccountFilter}
              />

              {official && (
                <ModelBreakdown
                  officialUsage={official}
                  accountFilter={sharedAccountFilter}
                  onAccountFilterChange={setSharedAccountFilter}
                />
              )}

              {/* 与下方「积分明细」重复，先隐藏。
              <AccountTable stats={stats} officialUsage={officialUsage} selectedId={selectedId} onSelect={setSelectedId} />
              */}

              <SelectedAccountDetails
                stats={filteredStats}
                officialUsage={officialUsage}
                creditMap={creditMap}
                creditLoadingMap={creditLoadingMap}
                accountFilter={sharedAccountFilter}
                onAccountFilterChange={setSharedAccountFilter}
              />
            </>
          )}
        </div>
      ) : (
        <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
          暂无统计数据，请点击刷新重试。
        </div>
      )}
    </div>
  );
}
