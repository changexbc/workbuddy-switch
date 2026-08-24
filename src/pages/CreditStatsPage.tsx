import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Activity,
  CalendarCheck,
  CheckCircle2,
  CircleAlert,
  CreditCard,
  Database,
  Loader2,
  RefreshCw,
  TrendingDown,
  XCircle,
} from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import * as api from "@/lib/api";
import type {
  CreditExpiry,
  CreditResource,
  CreditStatsAccount,
  CreditStatsEvent,
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

function accountLabel(account: CreditStatsAccount): string {
  return account.accountName || account.accountId;
}

function chartPoints(stats: CreditStatistics, range: RangeKey) {
  const today = dateKey(new Date());
  const firstDate = range === "today" ? today : range === "7d" ? dateDaysAgo(6) : dateDaysAgo(29);
  return stats.daily.filter((point) => {
    if (range === "month") {
      return point.date.startsWith(`${today.slice(0, 7)}-`);
    }
    return point.date >= firstDate && point.date <= today;
  });
}

function rangeUsage(stats: CreditStatistics, range: RangeKey): number {
  switch (range) {
    case "today":
      return stats.summary.usageToday;
    case "7d":
      return stats.summary.usage7Days;
    case "month":
      return stats.summary.usageThisMonth;
    case "30d":
      return stats.daily
        .filter((point) => point.date >= dateDaysAgo(29) && point.date <= dateKey(new Date()))
        .reduce((sum, point) => sum + point.usage, 0);
  }
}

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

function checkinBadgeVariant(
  result: string | null | undefined,
): "success" | "warning" | "destructive" | "outline" {
  switch (result) {
    case "success":
      return "success";
    case "already":
      return "warning";
    case "error":
      return "destructive";
    default:
      return "outline";
  }
}

function resourceName(resource: CreditResource): string {
  return resource.packageName || resource.packageCode || "未命名资源包";
}

function StatCard({
  icon,
  label,
  value,
  hint,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  hint?: string;
}) {
  return (
    <Card className="min-w-0 gap-3 rounded-xl py-4 shadow-none">
      <CardContent className="min-w-0 px-4">
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          {icon}
          <span className="truncate">{label}</span>
        </div>
        <div className="mt-2 truncate text-2xl font-semibold tracking-tight">{value}</div>
        {hint && <div className="mt-1 truncate text-[11px] text-muted-foreground/75">{hint}</div>}
      </CardContent>
    </Card>
  );
}

function TrendChart({
  stats,
  range,
  onRangeChange,
}: {
  stats: CreditStatistics;
  range: RangeKey;
  onRangeChange: (range: RangeKey) => void;
}) {
  const points = chartPoints(stats, range);
  const maxUsage = Math.max(1, ...points.map((point) => point.usage));
  const hasObservedUsage = points.some((point) => point.usage > 0);

  return (
    <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
      <CardHeader className="gap-3 border-b px-4 py-4 sm:px-5">
        <div className="flex min-w-0 flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <CardTitle className="flex items-center gap-2 text-sm">
              <TrendingDown className="size-4 text-primary" />
              观察到的积分消耗
            </CardTitle>
            <CardDescription className="mt-1 text-xs">
              只统计连续快照中余额下降的正差值，不代表服务端官方账单。
            </CardDescription>
          </div>
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
                onClick={() => onRangeChange(option.key)}
                aria-pressed={range === option.key}
              >
                {option.label}
              </button>
            ))}
          </div>
        </div>
      </CardHeader>
      <CardContent className="min-w-0 px-4 py-5 sm:px-5">
        {!stats.coverageStartAt ? (
          <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
            尚无积分快照。首次成功采集后，统计会从该时刻开始累计。
          </div>
        ) : points.length === 0 ? (
          <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
            当前口径暂无可展示的观察数据。
          </div>
        ) : (
          <>
            <div className="overflow-x-auto pb-1" tabIndex={0} aria-label="积分消耗趋势图，可横向滚动">
              <div
                className="flex h-56 min-w-[560px] items-end gap-1.5 border-b border-l border-border/70 px-2 pb-0 pt-4"
                role="img"
                aria-label={`${RANGE_OPTIONS.find((option) => option.key === range)?.label}积分消耗柱状图`}
              >
                {points.map((point, index) => {
                  const height = point.usage > 0 ? Math.max(4, (point.usage / maxUsage) * 100) : 1.5;
                  const showLabel = index === 0 || index === points.length - 1 || index % 5 === 0;
                  return (
                    <div key={point.date} className="flex h-full min-w-3 flex-1 flex-col justify-end gap-1">
                      <div className="flex min-h-0 flex-1 items-end justify-center">
                        <div
                          className="w-full max-w-6 rounded-t bg-primary/75 transition-[height] hover:bg-primary"
                          style={{ height: `${height}%` }}
                          title={`${point.date}：${formatCredits(point.usage)} 积分`}
                        />
                      </div>
                      <span className="h-4 text-center text-[10px] text-muted-foreground">
                        {showLabel ? point.date.slice(5) : ""}
                      </span>
                    </div>
                  );
                })}
              </div>
            </div>
            <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
              <span>{hasObservedUsage ? `当前口径合计 ${formatCredits(rangeUsage(stats, range))} 积分` : "已采集快照，暂未观察到余额下降"}</span>
              <span>数据覆盖至 {formatDate(stats.generatedAt)}</span>
            </div>
            <p className="sr-only">
              {points.map((point) => `${point.date} 消耗 ${formatCredits(point.usage)} 积分`).join("；")}
            </p>
          </>
        )}
      </CardContent>
    </Card>
  );
}

function AccountTable({
  stats,
  selectedId,
  onSelect,
}: {
  stats: CreditStatistics;
  selectedId: string | null;
  onSelect: (id: string) => void;
}) {
  return (
    <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
      <CardHeader className="border-b px-4 py-4 sm:px-5">
        <CardTitle className="flex items-center gap-2 text-sm">
          <Activity className="size-4 text-primary" />
          账号积分明细
        </CardTitle>
        <CardDescription className="mt-1 text-xs">
          账号 ID 是统计关联键，名称只用于展示；点击一行查看资源包和事件。
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
                        </span>
                        <span className="mt-0.5 block truncate text-[11px] text-muted-foreground">
                          {account.accountId}
                        </span>
                      </button>
                    </td>
                    <td className="px-3 py-3 text-right font-medium">
                      {formatCredits(account.currentRemaining)}
                    </td>
                    <td className="px-3 py-3 text-right">{formatCredits(account.usageToday)}</td>
                    <td className="px-3 py-3 text-right">{formatCredits(account.usage7Days)}</td>
                    <td className="px-3 py-3 text-right">{formatCredits(account.usageThisMonth)}</td>
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
  );
}

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
          <div key={`${resource.packageCode || resource.packageName || "resource"}-${index}`} className="min-w-0 px-4 py-3 sm:px-5">
            <div className="flex min-w-0 items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="truncate text-sm font-medium">{resourceName(resource)}</div>
                <div className="mt-1 text-[11px] text-muted-foreground">
                  {resource.expired ? "已到期" : resource.expiringSoon ? "7 天内到期" : `到期 ${formatDate(resource.expireAt)}`}
                </div>
              </div>
              <div className="shrink-0 text-right text-xs">
                <div className="font-medium">{formatCredits(resource.remaining)} / {formatCredits(resource.total)}</div>
                <div className="mt-1 text-[11px] text-muted-foreground">已用 {formatCredits(resource.used)}</div>
              </div>
            </div>
            <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
              <div className="h-full rounded-full bg-primary/75" style={{ width: `${ratio}%` }} />
            </div>
          </div>
        );
      })}
    </div>
  );
}

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
        {isError ? <XCircle className="size-3.5" /> : isAlready ? <CalendarCheck className="size-3.5" /> : <CheckCircle2 className="size-3.5" />}
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

function SelectedAccountDetails({
  account,
  credit,
  creditLoading,
  events,
}: {
  account: CreditStatsAccount | null;
  credit?: CreditExpiry;
  creditLoading?: boolean;
  events: CreditStatsEvent[];
}) {
  if (!account) {
    return (
      <Card className="min-w-0 rounded-xl shadow-none">
        <CardContent className="px-4 py-10 text-center text-sm text-muted-foreground sm:px-5">选择账号后查看详情。</CardContent>
      </Card>
    );
  }

  return (
    <div className="grid min-w-0 gap-4 lg:grid-cols-2">
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="border-b px-4 py-4 sm:px-5">
          <CardTitle className="flex items-center gap-2 text-sm">
            <CreditCard className="size-4 text-primary" />
            当前资源包
          </CardTitle>
          <CardDescription className="mt-1 truncate text-xs" title={account.accountId}>
            {accountLabel(account)} · 最近采集 {formatDateTime(account.lastSnapshotAt)}
          </CardDescription>
        </CardHeader>
        <ResourceBreakdown credit={credit} loading={creditLoading} />
      </Card>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="border-b px-4 py-4 sm:px-5">
          <CardTitle className="flex items-center gap-2 text-sm">
            <Database className="size-4 text-primary" />
            最近事件
          </CardTitle>
          <CardDescription className="mt-1 text-xs">签到与积分观察分开记录，签到不会计入消耗。</CardDescription>
        </CardHeader>
        <CardContent className="max-h-[340px] min-w-0 overflow-y-auto px-4 py-1 sm:px-5">
          {events.length === 0 ? (
            <div className="py-8 text-center text-sm text-muted-foreground">该账号暂无最近事件。</div>
          ) : (
            events.map((event, index) => <EventRow key={`${event.kind}-${event.ts}-${index}`} event={event} />)
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function UnselectedRecentEvents({ events }: { events: CreditStatsEvent[] }) {
  return (
    <Card className="mt-4 min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
      <CardHeader className="border-b px-4 py-4 sm:px-5">
        <CardTitle className="flex items-center gap-2 text-sm">
          <Database className="size-4 text-primary" />
          最近事件
        </CardTitle>
        <CardDescription className="mt-1 text-xs">签到与积分观察分开记录，签到不会计入消耗。</CardDescription>
      </CardHeader>
      <CardContent className="max-h-[340px] min-w-0 overflow-y-auto px-4 py-1 sm:px-5">
        {events.slice(0, 50).map((event, index) => (
          <EventRow key={`${event.kind}-${event.ts}-${index}`} event={event} />
        ))}
      </CardContent>
    </Card>
  );
}

export default function CreditStatsPage() {
  const {
    accounts,
    creditMap,
    creditLoadingMap,
    ensureCredits,
    fetchAll,
    refreshCredits,
  } = useAccountsStore();
  const [stats, setStats] = useState<CreditStatistics | null>(null);
  const [range, setRange] = useState<RangeKey>("30d");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(
    async (forceCredits = false) => {
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
        if (ids.length > 0) {
          if (forceCredits) {
            await refreshCredits(ids);
          } else {
            await ensureCredits(ids);
          }
        }
        const next = await api.getCreditStatistics();
        setStats(next);
        setSelectedId((current) =>
          current && next.accounts.some((account) => account.accountId === current)
            ? current
            : next.accounts[0]?.accountId ?? ids[0] ?? null,
        );
      } catch (cause) {
        setError(api.asError(cause));
      } finally {
        setLoading(false);
      }
    },
    [ensureCredits, fetchAll, refreshCredits],
  );

  useEffect(() => {
    void load();
  }, [load]);

  const selectedAccount = useMemo(
    () => stats?.accounts.find((account) => account.accountId === selectedId) ?? null,
    [selectedId, stats],
  );
  const selectedEvents = useMemo(
    () => stats?.events.filter((event) => event.accountId === selectedId).slice(0, 50) ?? [],
    [selectedId, stats],
  );

  return (
    <div className="mx-auto w-full max-w-[1180px] min-w-0 px-4 py-6 sm:px-8 sm:py-9">
      <header className="mb-6 flex min-w-0 flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-[28px] font-semibold tracking-tight">积分统计</h1>
          <p className="mt-2 max-w-2xl text-sm leading-6 text-muted-foreground">
            用本地积分快照追踪账号余额变化，并把签到记录作为独立事件查看。
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
        <>
          {accounts.length === 0 && (
            <Alert className="mb-5">
              <CircleAlert />
              <AlertTitle>暂无当前账号</AlertTitle>
              <AlertDescription>可以先去账号管理导入或登录账号；历史事件仍会保留在下方最近事件中。</AlertDescription>
            </Alert>
          )}

          <section className="mb-5 grid min-w-0 grid-cols-1 gap-3 min-[420px]:grid-cols-2 sm:grid-cols-5" aria-label="积分总览">
            <StatCard icon={<CreditCard className="size-3.5" />} label="当前剩余积分" value={formatCredits(stats.summary.currentRemaining)} />
            <StatCard icon={<TrendingDown className="size-3.5" />} label="今日观察消耗" value={formatCredits(stats.summary.usageToday)} />
            <StatCard icon={<TrendingDown className="size-3.5" />} label="近 7 天消耗" value={formatCredits(stats.summary.usage7Days)} />
            <StatCard icon={<TrendingDown className="size-3.5" />} label="本月观察消耗" value={formatCredits(stats.summary.usageThisMonth)} />
            <StatCard icon={<CalendarCheck className="size-3.5" />} label="今日签到账号" value={`${stats.summary.todayCheckedInAccounts}`} hint={`成功 ${stats.summary.todaySuccess} · 已签 ${stats.summary.todayAlready}`} />
          </section>

          {!stats.coverageStartAt && stats.events.some((event) => event.kind === "checkin") && (
            <Alert className="mb-5">
              <CalendarCheck />
              <AlertTitle>目前只有签到记录</AlertTitle>
              <AlertDescription>签到不会被计入积分消耗。首次成功采集积分资源后，趋势统计才会开始累计。</AlertDescription>
            </Alert>
          )}

          <div className="mb-5">
            <TrendChart stats={stats} range={range} onRangeChange={setRange} />
          </div>

          <div className="mb-5">
          <AccountTable stats={stats} selectedId={selectedId} onSelect={setSelectedId} />
          </div>

          {!selectedAccount && stats.events.length > 0 && <UnselectedRecentEvents events={stats.events} />}

          <SelectedAccountDetails
            account={selectedAccount}
            credit={selectedId ? creditMap[selectedId] : undefined}
            creditLoading={selectedId ? creditLoadingMap[selectedId] : undefined}
            events={selectedEvents}
          />
        </>
      ) : (
        <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
          暂无统计数据，请点击刷新重试。
        </div>
      )}
    </div>
  );
}
