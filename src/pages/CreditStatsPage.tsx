import { useCallback, useEffect, useState } from "react";
import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts";
import {
  CalendarDays,
  CalendarRange,
  Check,
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
import type {
  CreditExpiry,
  CreditOfficialUsage,
  CreditOfficialUsageAccount,
  CreditOfficialUsageModel,
  CreditOfficialUsageRequest,
  CreditResource,
  CreditStatsAccount,
  CreditStatsDailyPoint,
  // CreditStatsEvent, // 最近事件卡片隐藏后未使用
  CreditStatistics,
} from "@/lib/types";
import { useAccountsStore } from "@/stores/accounts";

type RangeKey = "30d" | "today" | "7d" | "month";

const RANGE_OPTIONS: { key: RangeKey; label: string }[] = [
  { key: "30d", label: "近 30 天" },
  { key: "today", label: "今天" },
  { key: "7d", label: "近 7 天" },
  { key: "month", label: "本月" },
];

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

function formatChartDate(date: string): string {
  return date.slice(5).replace("-", "/");
}

function accountLabel(account: { accountName?: string | null; accountId: string }): string {
  return account.accountName || account.accountId;
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

function resourceName(resource: CreditResource): string {
  return resource.packageName || resource.packageCode || "未命名资源包";
}

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

/** 层叠柱状图的模型固定色板（区分度高，深浅模式通用）。 */
const MODEL_COLORS = ["#2563eb", "#f59e0b", "#10b981", "#8b5cf6", "#ef4444", "#06b6d4", "#f97316", "#84cc16"];
const MAX_MODELS = 5;
const OTHER_MODEL = "其他";

interface ModelChartPoint {
  date: string;
  total: number;
  [model: string]: number | string;
}

/** 从官方 daily（全量按模型聚合）构建层叠数据；模型按总消耗取前 N，其余并入「其他」。 */
function buildStackedChart(
  daily: CreditStatsDailyPoint[],
): { models: string[]; points: ModelChartPoint[] } {
  const modelTotals = new Map<string, number>();
  for (const point of daily) {
    for (const model of point.models ?? []) {
      modelTotals.set(model.model, (modelTotals.get(model.model) ?? 0) + model.credit);
    }
  }
  const topModels = [...modelTotals.entries()]
    .sort((left, right) => right[1] - left[1])
    .slice(0, MAX_MODELS)
    .map(([model]) => model);

  const points: ModelChartPoint[] = daily.map((point) => {
    const entry: ModelChartPoint = { date: point.date, total: point.usage };
    for (const model of point.models ?? []) {
      const key = topModels.includes(model.model) ? model.model : OTHER_MODEL;
      entry[key] = (typeof entry[key] === "number" ? entry[key] : 0) + model.credit;
    }
    return entry;
  });
  const models = [...topModels];
  if (points.some((point) => point[OTHER_MODEL] !== undefined)) {
    models.push(OTHER_MODEL);
  }
  return { models, points };
}

function TrendChart({
  stats,
  officialUsage,
}: {
  stats: CreditStatistics;
  officialUsage?: CreditOfficialUsage;
}) {
  /** null = 所有账号汇总；本卡片独立，不影响其他卡片 */
  const [accountFilter, setAccountFilter] = useState<string | null>(null);
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

  const series = stacked ? stacked.models : ["总消耗"];
  const chartConfig: ChartConfig = {};
  for (const model of series) {
    chartConfig[model] = {
      label: model,
      ...(stacked
        ? { color: MODEL_COLORS[series.indexOf(model) % MODEL_COLORS.length] }
        : { theme: { light: "var(--primary)", dark: "var(--primary)" } as const }),
    };
  }

  const hasObservedUsage = chartData.some((point) => point.total > 0);
  const barRadius = stacked ? [2, 2, 0, 0] as [number, number, number, number] : [3, 3, 0, 0] as [number, number, number, number];

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
                onAccountFilterChange={setAccountFilter}
                ariaLabel="按账号筛选趋势"
              />
              <div className="flex max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1" aria-label="趋势范围">
                {RANGE_OPTIONS.map((option) => (
                  <button
                    key={option.key}
                    type="button"
                    className={`rounded-md px-2.5 py-1.5 text-xs transition-colors ${
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
                    fill={stacked ? MODEL_COLORS[index % MODEL_COLORS.length] : "var(--color-总消耗)"}
                    radius={index === series.length - 1 ? barRadius : undefined}
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
                        className="min-w-0 max-w-full text-left outline-none focus-visible:rounded-md focus-visible:ring-2 focus-visible:ring-ring"
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
              <div className="min-w-0 truncate text-[13px] font-medium">{resourceName(resource)}</div>
              <div className="flex shrink-0 items-center gap-2.5">
                <span className="text-[11px] text-muted-foreground">
                  {resource.expired ? "已到期" : resource.expiringSoon ? "7 天内到期" : `到期 ${formatDate(resource.expireAt)}`}
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
      {models.slice(0, 8).map((model) => {
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
}: {
  officialUsage: CreditOfficialUsage;
}) {
  /** null = 所有账号汇总；本卡片独立，不影响其他卡片 */
  const [accountFilter, setAccountFilter] = useState<string | null>(null);
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
                onAccountFilterChange={setAccountFilter}
                ariaLabel="按账号筛选模型分类"
              />
              <div className="flex max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1" aria-label="模型分类时间范围">
                {RANGE_OPTIONS.map((option) => (
                  <button
                    key={option.key}
                    type="button"
                    className={`rounded-md px-2.5 py-1.5 text-xs transition-colors ${
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
          {models.length > 8 && <p className="mt-3 text-[11px] text-muted-foreground">已展示消耗最高的 8 个模型，其余模型仍计入上方合计。</p>}
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

function OfficialUsageBreakdown({
  officialUsage,
  accountId,
}: {
  officialUsage?: CreditOfficialUsage;
  accountId: string | null;
}) {
  const officialAvailable = isOfficialUsageAvailable(officialUsage);
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

  return (
    <div className="min-w-0">
      {detailTruncated && (
        <div className="flex items-start gap-2 border-b bg-amber-500/[0.06] px-4 py-2.5 text-xs text-amber-800 sm:px-5">
          <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
          <span>
            仅展示最近 {officialUsage.detailLimitPerAccount} 条请求明细；合计使用官方返回的全部 {formatCredits(totalRequests)} 条请求。
          </span>
        </div>
      )}
      {requests.length === 0 ? (
        <div className="px-4 py-8 text-center text-sm text-muted-foreground sm:px-5">
          {totalRequests > 0 ? "官方返回了请求总数，但明细未通过格式校验。" : "官方暂无请求用量。"}
        </div>
      ) : (
        <div className="max-h-[340px] min-w-0 overflow-auto">
          <table className="w-full min-w-[700px] text-left text-[11px]">
            <thead className="sticky top-0 bg-muted/95 text-muted-foreground">
              <tr>
                <th className="px-3 py-2.5 font-medium">请求时间</th>
                {showAccount && <th className="px-3 py-2.5 font-medium">账号</th>}
                <th className="px-3 py-2.5 text-right font-medium">消耗</th>
                <th className="px-3 py-2.5 font-medium">模型</th>
                <th className="px-3 py-2.5 font-medium">客户端</th>
                <th className="px-3 py-2.5 font-medium">请求 ID</th>
              </tr>
            </thead>
            <tbody>
              {requests.map((request) => (
                <OfficialRequestRow
                  key={`${request.requestId}-${request.requestTime}`}
                  request={request}
                  showAccount={showAccount}
                />
              ))}
            </tbody>
          </table>
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
}: {
  stats: CreditStatistics;
  officialUsage?: CreditOfficialUsage;
  creditMap: Record<string, CreditExpiry>;
  creditLoadingMap: Record<string, boolean>;
}) {
  const [detailTab, setDetailTab] = useState<"credits" | "requests">("credits");
  const official = isOfficialUsageAvailable(officialUsage) ? officialUsage : undefined;
  const filterAccounts = official ? official.accounts : stats.accounts;
  /** 本卡片仅允许选择单个账号，默认第一个账号 */
  const [accountFilter, setAccountFilter] = useState<string | null>(
    () => filterAccounts[0]?.accountId ?? null,
  );
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
                onAccountFilterChange={setAccountFilter}
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
            <div className="max-h-[340px] min-w-0 overflow-y-auto">
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

/** 进入统计页时距上次刷新超过此时长（ms）则自动触发一次刷新统计 */
const STATISTICS_AUTO_REFRESH_MS = 30 * 60 * 1000;

/** 最近一次「刷新统计」完成的时刻（会话级，0 = 从未刷新过） */
let lastStatisticsRefreshAt = 0;

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
        if (refresh) lastStatisticsRefreshAt = Date.now();
      } catch (cause) {
        setError(api.asError(cause));
      } finally {
        setLoading(false);
      }
    },
    [fetchAll, refreshCredits],
  );

  useEffect(() => {
    // 已有会话缓存且距上次刷新超过 30 分钟时，进入页面自动刷新一次统计
    const autoRefresh =
      cachedStatistics !== null && Date.now() - lastStatisticsRefreshAt >= STATISTICS_AUTO_REFRESH_MS;
    void load(autoRefresh);
  }, [load]);

  const officialUsage = stats?.officialUsage;
  const official = isOfficialUsageAvailable(officialUsage) ? officialUsage : undefined;

  return (
    <div className="mx-auto w-full max-w-[1180px] min-w-0 px-4 py-6 sm:px-8 sm:py-9">
      <header className="mb-10 flex min-w-0 flex-wrap items-start justify-between gap-4 sm:mb-12">
        <div className="min-w-0">
          <h1 className="text-[28px] font-semibold tracking-tight">积分统计</h1>
          <p className="mt-2 max-w-2xl text-sm leading-6 text-muted-foreground">
            当前数据更新于 {stats ? formatDateTime(official?.collectedAt ?? stats.generatedAt) : "—"}
          </p>
        </div>
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
      ) : stats ? (
        <div className="min-w-0 space-y-12">
          {accounts.length === 0 && (
            <Alert>
              <CircleAlert />
              <AlertTitle>暂无当前账号</AlertTitle>
              <AlertDescription>可以先去账号管理导入或登录账号；历史事件仍会保留在下方最近事件中。</AlertDescription>
            </Alert>
          )}

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
                value={formatCredits(stats.summary.currentRemaining)}
              />
              <StatMetric
                icon={TrendingDown}
                label="今日消耗"
                value={formatCredits(official ? official.summary.usageToday : stats.summary.usageToday)}
                divided
              />
              <StatMetric
                icon={CalendarDays}
                label="近 7 天消耗"
                value={formatCredits(official ? official.summary.usage7Days : stats.summary.usage7Days)}
                divided
              />
              <StatMetric
                icon={CalendarRange}
                label="本月消耗"
                value={formatCredits(official ? official.summary.usageThisMonth : stats.summary.usageThisMonth)}
                divided
              />
            </CardContent>
          </Card>

          {!official && !stats.coverageStartAt && stats.events.some((event) => event.kind === "checkin") && (
            <Alert>
              <CircleCheck />
              <AlertTitle>目前只有签到记录</AlertTitle>
              <AlertDescription>签到不会被计入积分消耗。首次成功采集积分资源后，趋势统计才会开始累计。</AlertDescription>
            </Alert>
          )}

          <TrendChart
            stats={stats}
            officialUsage={officialUsage}
          />

          {official && (
            <ModelBreakdown
              officialUsage={official}
            />
          )}

          {/* 与下方「积分明细」重复，先隐藏。
          <AccountTable stats={stats} officialUsage={officialUsage} selectedId={selectedId} onSelect={setSelectedId} />
          */}

          <SelectedAccountDetails
            stats={stats}
            officialUsage={officialUsage}
            creditMap={creditMap}
            creditLoadingMap={creditLoadingMap}
          />
        </div>
      ) : (
        <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
          暂无统计数据，请点击刷新重试。
        </div>
      )}
    </div>
  );
}
