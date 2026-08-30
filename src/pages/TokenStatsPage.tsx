import { useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import {
  CircleAlert,
  Database,
  Gauge,
  Info,
  Loader2,
  MessagesSquare,
  RefreshCw,
  Sparkles,
  type LucideIcon,
} from "lucide-react";
import { Bar, CartesianGrid, ComposedChart, Line, XAxis, YAxis } from "recharts";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader } from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import { DemoAction } from "@/components/demo-action";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import * as api from "@/lib/api";
import type {
  TokenStatistics,
  TokenStatsGroup,
  TokenStatsSource,
  TokenStatsTotals,
} from "@/lib/types";

type SourceKey = TokenStatsSource["source"];
type RangeKey = "30d" | "today" | "7d" | "month";
type DistributionKey = "projects" | "models";

const RANGE_OPTIONS: { key: RangeKey; label: string }[] = [
  { key: "30d", label: "近 30 天" },
  { key: "today", label: "今天" },
  { key: "7d", label: "近 7 天" },
  { key: "month", label: "本月" },
];

const chartConfig = {
  cacheRead: { label: "缓存读取", color: "var(--primary)" },
  uncachedInput: { label: "新增输入", color: "#55a9d8" },
  output: { label: "输出", color: "#9a80d8" },
  records: { label: "调用", color: "#d6973d" },
} satisfies ChartConfig;

const compact = new Intl.NumberFormat("zh-CN", {
  maximumFractionDigits: 1,
  notation: "compact",
});
const exact = new Intl.NumberFormat("zh-CN");

/** 展示总量：input 已包含 cacheRead，因此不能再次加上 cacheRead。 */
const tokenTotal = (value: TokenStatsTotals) =>
  value.input + value.output + value.cacheWrite;

const percentage = (value: number, sum: number) =>
  sum > 0 ? `${((value / sum) * 100).toFixed(1)}%` : "—";

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

function formatDateTime(timestamp?: number | null): string {
  if (!timestamp) return "—";
  return new Date(timestamp).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatChartDate(date: string): string {
  return date.slice(5).replace("-", "/");
}

function rangeLabel(range: RangeKey): string {
  return RANGE_OPTIONS.find((option) => option.key === range)?.label ?? "近 30 天";
}

function rangePoints(daily: TokenStatsGroup[], range: RangeKey): TokenStatsGroup[] {
  const today = dateKey(new Date());
  const firstDate =
    range === "today" ? today : range === "7d" ? dateDaysAgo(6) : dateDaysAgo(29);

  return daily
    .filter((point) => {
      if (range === "month") {
        return point.key.startsWith(`${today.slice(0, 7)}-`);
      }
      return point.key >= firstDate && point.key <= today;
    })
    .sort((left, right) => left.key.localeCompare(right.key));
}

function rangeTotals(points: TokenStatsGroup[]): TokenStatsTotals {
  const totals = points.reduce(
    (sum, point) => ({
      total: sum.total + tokenTotal(point),
      input: sum.input + point.input,
      output: sum.output + point.output,
      cacheRead: sum.cacheRead + point.cacheRead,
      cacheWrite: sum.cacheWrite + point.cacheWrite,
      uncachedInput: sum.uncachedInput + point.uncachedInput,
      records: sum.records + point.records,
      cacheHitRate: null,
    }),
    {
      total: 0,
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      uncachedInput: 0,
      records: 0,
      cacheHitRate: null,
    } satisfies TokenStatsTotals,
  );

  return {
    ...totals,
    cacheHitRate: totals.input > 0 ? totals.cacheRead / totals.input : null,
  };
}

function SectionTitle({ id, children }: { id: string; children: string }) {
  return (
    <div className="px-1">
      <h2 id={id} className="text-[13px] font-medium leading-5">
        {children}
      </h2>
    </div>
  );
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
      <div
        className="mt-3 max-w-full truncate text-[26px] font-semibold leading-8 tracking-[-0.025em] text-foreground tabular-nums"
        style={{
          fontFamily:
            '"Bricolage Grotesque Variable", "SF Pro Display", ui-sans-serif, sans-serif',
        }}
      >
        {value}
      </div>
    </div>
  );
}

function Overview({ source }: { source: TokenStatsSource }) {
  const { summary } = source;
  const cacheRate = summary.cacheHitRate;

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-overview-title">
      <SectionTitle id="token-overview-title">Token 总览</SectionTitle>
      <Card
        className="min-w-0 gap-0 overflow-hidden rounded-2xl bg-card/70 py-0 shadow-none"
        aria-label="Token 总览"
      >
        <CardContent className="grid min-w-0 grid-cols-1 divide-y divide-border/60 p-0 sm:grid-cols-4 sm:divide-y-0 sm:py-5">
          <StatMetric icon={Sparkles} label="总 Token" value={compact.format(tokenTotal(summary))} />
          <StatMetric icon={Database} label="输入 Token" value={compact.format(summary.input)} divided />
          <StatMetric
            icon={MessagesSquare}
            label="输出 Token"
            value={compact.format(summary.output)}
            divided
          />
          <StatMetric
            icon={Gauge}
            label="缓存命中率"
            value={cacheRate == null ? "—" : `${(cacheRate * 100).toFixed(1)}%`}
            divided
          />
        </CardContent>
      </Card>
    </section>
  );
}

function TrendChart({ source }: { source: TokenStatsSource }) {
  const [range, setRange] = useState<RangeKey>("30d");
  const points = useMemo(() => rangePoints(source.daily, range), [range, source.daily]);
  const totals = useMemo(() => rangeTotals(points), [points]);
  const chartData = points.map((point) => ({ ...point, date: point.key }));

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-trend-title">
      <SectionTitle id="token-trend-title">流量趋势</SectionTitle>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
          <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <CardDescription className="min-w-0 text-xs">
              新增输入、缓存读取与输出按日展示；折线表示调用次数。
            </CardDescription>
            <div
              className="flex max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1"
              aria-label="趋势范围"
            >
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
        </CardHeader>
        <CardContent className="min-w-0 px-4 pt-3 pb-4 sm:px-5">
          {chartData.length === 0 ? (
            <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
              当前范围暂无可展示的 Token 数据。
            </div>
          ) : (
            <>
              <ChartContainer config={chartConfig} className="h-56 w-full">
                <ComposedChart data={chartData} margin={{ top: 8, right: 6, left: 0, bottom: 0 }}>
                  <CartesianGrid vertical={false} strokeDasharray="3 3" />
                  <XAxis
                    dataKey="date"
                    tickLine={false}
                    axisLine={false}
                    tickMargin={8}
                    tickFormatter={(value) => formatChartDate(String(value))}
                  />
                  <YAxis
                    yAxisId="tokens"
                    tickLine={false}
                    axisLine={false}
                    width={46}
                    tickFormatter={(value) => compact.format(Number(value))}
                  />
                  <YAxis
                    yAxisId="calls"
                    orientation="right"
                    tickLine={false}
                    axisLine={false}
                    width={34}
                  />
                  <ChartTooltip
                    cursor={{ fill: "var(--muted)", opacity: 0.4 }}
                    content={
                      <ChartTooltipContent
                        labelFormatter={(value) => formatChartDate(String(value))}
                      />
                    }
                  />
                  <Bar
                    yAxisId="tokens"
                    dataKey="cacheRead"
                    stackId="token"
                    fill="var(--color-cacheRead)"
                  />
                  <Bar
                    yAxisId="tokens"
                    dataKey="uncachedInput"
                    stackId="token"
                    fill="var(--color-uncachedInput)"
                  />
                  <Bar
                    yAxisId="tokens"
                    dataKey="output"
                    stackId="token"
                    fill="var(--color-output)"
                    radius={[3, 3, 0, 0]}
                  />
                  <Line
                    yAxisId="calls"
                    dataKey="records"
                    stroke="var(--color-records)"
                    strokeWidth={2}
                    dot={false}
                  />
                </ComposedChart>
              </ChartContainer>
              <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
                <span>
                  {rangeLabel(range)}合计 {compact.format(tokenTotal(totals))} Token · {exact.format(totals.records)} 次调用
                </span>
                <span>数据覆盖至 {formatDateTime(source.coverageEndAt)}</span>
              </div>
              <p className="sr-only">
                {chartData
                  .map(
                    (point) =>
                      `${point.date} 使用 ${exact.format(tokenTotal(point))} Token，${exact.format(point.records)} 次调用`,
                  )
                  .join("；")}
              </p>
            </>
          )}
        </CardContent>
      </Card>
    </section>
  );
}

function Composition({ value }: { value: TokenStatsTotals }) {
  const sum = tokenTotal(value);
  const rows = [
    { label: "缓存读取", value: value.cacheRead, color: "bg-primary" },
    { label: "新增输入", value: value.uncachedInput, color: "bg-sky-500" },
    { label: "输出", value: value.output, color: "bg-violet-500" },
    { label: "缓存写入", value: value.cacheWrite, color: "bg-amber-500" },
  ];

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-composition-title">
      <SectionTitle id="token-composition-title">Token 构成</SectionTitle>
      <Card className="min-w-0 gap-0 rounded-xl py-0 shadow-none">
        <CardHeader className="px-4 pt-3 pb-0 sm:px-5">
          <CardDescription className="text-xs">
            输入拆分为缓存读取与真正新增的上下文。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4 px-4 pt-3 pb-5 sm:px-5">
          {rows.map((row) => {
            const share = percentage(row.value, sum);
            return (
              <div key={row.label}>
                <div className="mb-1.5 flex items-center justify-between gap-2 text-xs">
                  <span className="flex items-center gap-2">
                    <span className={`size-2.5 rounded-[3px] ${row.color}`} aria-hidden="true" />
                    {row.label}
                  </span>
                  <span className="shrink-0 tabular-nums">
                    {compact.format(row.value)}
                    <span className="ml-1 text-muted-foreground">{share}</span>
                  </span>
                </div>
                <div className="h-1.5 overflow-hidden rounded-full bg-muted">
                  <div
                    className={`h-full rounded-full ${row.color}`}
                    style={{ width: share === "—" ? "0%" : share }}
                  />
                </div>
              </div>
            );
          })}
          {value.cacheWrite === 0 && value.input > 0 ? (
            <p className="flex items-start gap-1.5 border-t border-border/60 pt-3 text-[11px] leading-4 text-muted-foreground">
              <Info className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
              当前来源尚未产生显式缓存写入量；缓存未命中已计入新增输入。
            </p>
          ) : null}
        </CardContent>
      </Card>
    </section>
  );
}

function Heatmap({ groups }: { groups: TokenStatsGroup[] }) {
  const map = new Map(groups.map((group) => [group.key, tokenTotal(group)]));
  const max = Math.max(1, ...map.values());
  const days = ["一", "二", "三", "四", "五", "六", "日"];

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-heatmap-title">
      <SectionTitle id="token-heatmap-title">使用热力图</SectionTitle>
      <Card className="min-w-0 gap-0 rounded-xl py-0 shadow-none">
        <CardHeader className="px-4 pt-3 pb-0 sm:px-5">
          <CardDescription className="text-xs">
            按本地星期与小时汇总 Token 活跃度。
          </CardDescription>
        </CardHeader>
        <CardContent className="min-w-0 px-4 pt-3 pb-5 sm:px-5">
          {map.size === 0 ? (
            <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
              暂无小时数据
            </div>
          ) : (
            <div role="img" aria-label="按星期和小时显示的 Token 使用热力图">
              <div className="mb-1 ml-6 flex justify-between text-[10px] text-muted-foreground">
                <span>0</span>
                <span>6</span>
                <span>12</span>
                <span>18</span>
                <span>23</span>
              </div>
              <div className="grid grid-cols-[12px_repeat(24,minmax(3px,1fr))] gap-0.5">
                {days.map((day, dayIndex) => (
                  <div className="contents" key={day}>
                    <span className="flex items-center text-[10px] text-muted-foreground">{day}</span>
                    {Array.from({ length: 24 }, (_, hour) => {
                      const value = map.get(`${dayIndex}-${hour}`) ?? 0;
                      const opacity = value ? 0.12 + 0.88 * Math.sqrt(value / max) : 0.04;
                      return (
                        <span
                          key={hour}
                          className="aspect-square min-w-0 rounded-[2px] bg-foreground"
                          style={{ opacity }}
                          title={`周${day} ${String(hour).padStart(2, "0")}:00 · ${exact.format(value)} Token`}
                          aria-label={`周${day} ${hour} 时 ${exact.format(value)} Token`}
                        />
                      );
                    })}
                  </div>
                ))}
              </div>
            </div>
          )}
        </CardContent>
      </Card>
    </section>
  );
}

function Ranking({
  groups,
  denominator,
  description,
  controls,
}: {
  groups: TokenStatsGroup[];
  denominator: number;
  description: string;
  controls?: ReactNode;
}) {
  const rows = groups.slice(0, 8);

  return (
    <Card className="min-w-0 gap-0 rounded-xl py-0 shadow-none">
      <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
        <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
          <CardDescription className="min-w-0 text-xs">{description}</CardDescription>
          {controls}
        </div>
      </CardHeader>
      <CardContent className="space-y-3 px-4 pt-3 pb-5 sm:px-5">
        {rows.map((row, index) => {
          const amount = tokenTotal(row);
          const share = percentage(amount, denominator);
          return (
            <div key={row.key}>
              <div className="mb-1.5 flex min-w-0 items-center gap-3 text-xs">
                <span className="w-5 shrink-0 font-mono text-muted-foreground">
                  {String(index + 1).padStart(2, "0")}
                </span>
                <span className="min-w-0 flex-1 truncate font-medium" title={row.key}>
                  {row.key}
                </span>
                <span className="shrink-0 tabular-nums">{compact.format(amount)}</span>
                <span className="w-12 shrink-0 text-right text-muted-foreground tabular-nums">
                  {share}
                </span>
              </div>
              <div className="ml-8 h-1.5 overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full rounded-full bg-primary"
                  style={{ width: share === "—" ? "0%" : share }}
                />
              </div>
            </div>
          );
        })}
        {rows.length === 0 && (
          <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
            暂无统计数据
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function SessionRanking({ groups, denominator }: { groups: TokenStatsGroup[]; denominator: number }) {
  const rows = groups.slice(0, 8);

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-sessions-title">
      <SectionTitle id="token-sessions-title">消耗最高的会话</SectionTitle>
      <Card className="min-w-0 gap-0 rounded-xl py-0 shadow-none">
        <CardHeader className="px-4 pt-3 pb-0 sm:px-5">
          <CardDescription className="text-xs">按本地聚合 Token 从高到低排列。</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3 px-4 pt-3 pb-5 sm:px-5">
          {rows.map((row, index) => {
            const amount = tokenTotal(row);
            const share = percentage(amount, denominator);
            const label = row.title?.trim() || "未命名会话";
            const detail = [row.project, row.title ? undefined : row.sessionId]
              .filter(Boolean)
              .join(" · ");
            return (
              <div key={row.key}>
                <div className="mb-1.5 flex min-w-0 items-start gap-3 text-xs">
                  <span className="mt-0.5 w-5 shrink-0 font-mono text-muted-foreground">
                    {String(index + 1).padStart(2, "0")}
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-medium" title={label} aria-label={label}>
                      {label}
                    </div>
                    {detail && (
                      <div className="mt-0.5 truncate text-[11px] text-muted-foreground" title={detail}>
                        {detail}
                      </div>
                    )}
                  </div>
                  <span className="mt-0.5 shrink-0 tabular-nums">{compact.format(amount)}</span>
                  <span className="mt-0.5 w-12 shrink-0 text-right text-muted-foreground tabular-nums">
                    {share}
                  </span>
                </div>
                <div className="ml-8 h-1.5 overflow-hidden rounded-full bg-muted">
                  <div
                    className="h-full rounded-full bg-primary"
                    style={{ width: share === "—" ? "0%" : share }}
                  />
                </div>
              </div>
            );
          })}
          {rows.length === 0 && (
            <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
              暂无统计数据
            </div>
          )}
        </CardContent>
      </Card>
    </section>
  );
}

function Distribution({ source }: { source: TokenStatsSource }) {
  const [distribution, setDistribution] = useState<DistributionKey>("projects");
  const groups = source[distribution];

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-distribution-title">
      <SectionTitle id="token-distribution-title">用量分布</SectionTitle>
      <Ranking
        groups={groups}
        denominator={tokenTotal(source.summary)}
        description={
          distribution === "projects"
            ? "按项目汇总本地 Token 用量。"
            : "按模型汇总本地 Token 用量。"
        }
        controls={
          <div className="flex rounded-lg bg-muted p-1" role="group" aria-label="用量分布维度">
            <Button
              type="button"
              variant={distribution === "projects" ? "secondary" : "ghost"}
              size="sm"
              className="h-7 px-2.5 text-xs"
              aria-pressed={distribution === "projects"}
              onClick={() => setDistribution("projects")}
            >
              按项目
            </Button>
            <Button
              type="button"
              variant={distribution === "models" ? "secondary" : "ghost"}
              size="sm"
              className="h-7 px-2.5 text-xs"
              aria-pressed={distribution === "models"}
              onClick={() => setDistribution("models")}
            >
              按模型
            </Button>
          </div>
        }
      />
    </section>
  );
}

function Dashboard({ source }: { source: TokenStatsSource }) {
  const denominator = tokenTotal(source.summary);

  if (source.summary.records === 0) {
    return (
      <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
        <div>
          {source.filesScanned > 0
            ? `已扫描 ${exact.format(source.filesScanned)} 个会话文件，但没有可用的 usage。`
            : "尚未发现该来源的本地会话日志。"}
        </div>
        {source.parseErrors > 0 && (
          <div className="mt-2 text-xs text-amber-600">
            已跳过 {exact.format(source.parseErrors)} 条无法解析的本地记录。
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="min-w-0 space-y-12">
      <Overview source={source} />
      <TrendChart source={source} />
      <div className="grid min-w-0 gap-6 lg:grid-cols-2">
        <Composition value={source.summary} />
        <Heatmap groups={source.hours} />
      </div>
      <Distribution source={source} />
      <SessionRanking groups={source.sessions} denominator={denominator} />
      {source.parseErrors > 0 && (
        <p className="flex items-center gap-1.5 px-1 text-xs text-amber-600">
          <CircleAlert className="size-3.5" aria-hidden="true" />
          已跳过 {exact.format(source.parseErrors)} 条无法解析的本地记录。
        </p>
      )}
    </div>
  );
}

export default function TokenStatsPage() {
  const [stats, setStats] = useState<TokenStatistics | null>(null);
  const [active, setActive] = useState<SourceKey>("workbuddy");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    let disposed = false;
    setLoading(true);
    setError(null);
    api
      .getTokenStatistics()
      .then((result) => {
        if (!disposed) setStats(result);
      })
      .catch((cause) => {
        if (!disposed) setError(api.asError(cause));
      })
      .finally(() => {
        if (!disposed) setLoading(false);
      });
    return () => {
      disposed = true;
    };
  }, [reload]);

  const source = stats?.sources.find((item) => item.source === active);

  return (
    <div className="mx-auto w-full max-w-[1180px] min-w-0 px-4 py-6 sm:px-8 sm:py-9">
      <header className="mb-10 flex min-w-0 flex-wrap items-start justify-between gap-4 sm:mb-12">
        <div className="min-w-0">
          <h1 className="text-[28px] font-semibold tracking-tight">Token 统计</h1>
          <p className="mt-2 max-w-2xl text-sm leading-6 text-muted-foreground">
            当前数据更新于 {stats ? formatDateTime(stats.generatedAt) : "—"}
          </p>
        </div>
        <div className="flex max-w-full flex-wrap items-center justify-end gap-2">
          <Tabs
            className="min-w-0 gap-0"
            value={active}
            onValueChange={(value) => setActive(value as SourceKey)}
          >
            <TabsList className="h-auto max-w-full flex-wrap" aria-label="Token 数据来源">
              <TabsTrigger className="max-w-full whitespace-normal" value="workbuddy">
                WorkBuddy
              </TabsTrigger>
              <TabsTrigger className="max-w-full whitespace-normal" value="codebuddy-cli">
                CodeBuddy CLI
              </TabsTrigger>
            </TabsList>
          </Tabs>
          <DemoAction>
            <Button
              className="shrink-0"
              variant="outline"
              size="sm"
              onClick={() => setReload((value) => value + 1)}
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
            <Button size="sm" variant="outline" onClick={() => setReload((value) => value + 1)}>
              重试
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {loading && !stats ? (
        <div className="flex items-center gap-2 py-20 text-sm text-muted-foreground">
          <Loader2 className="animate-spin" />
          正在扫描本地会话日志…
        </div>
      ) : source ? (
        <Dashboard source={source} />
      ) : (
        !error && (
          <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
            该来源暂无可用统计数据，请点击刷新重试。
          </div>
        )
      )}
    </div>
  );
}
