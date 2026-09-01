import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ComponentProps, ReactNode } from "react";
import {
  CircleAlert,
  ArrowDownToLine,
  ArrowUpFromLine,
  Gauge,
  Loader2,
  MessagesSquare,
  RefreshCw,
  type LucideIcon,
} from "lucide-react";
import {
  Bar,
  CartesianGrid,
  ComposedChart,
  Line,
  Rectangle,
  XAxis,
  YAxis,
} from "recharts";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader } from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  type ChartConfig,
} from "@/components/ui/chart";
import { DemoAction } from "@/components/demo-action";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Skeleton } from "@/components/ui/skeleton";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import * as api from "@/lib/api";
import { getStackedSegmentVisualLayout } from "@/lib/stacked-bar-visuals";
import type {
  TokenStatistics,
  TokenStatsGroup,
  TokenStatsSource,
  TokenStatsTotals,
} from "@/lib/types";

type SourceKey = TokenStatsSource["source"];
type RangeKey = "30d" | "today" | "7d" | "month";
type OverviewRangeKey = "today" | "7d" | "30d" | "total";
type DistributionKey = "projects" | "models";

const TOKEN_SOURCE_STORAGE_KEY = "wb-switch:token-stats:source";
const RANKING_LIMIT = 10;

function isSourceKey(value: unknown): value is SourceKey {
  return value === "workbuddy" || value === "codebuddy-cli";
}

function readPreferredTokenSource(): SourceKey {
  if (typeof window === "undefined") return "workbuddy";
  try {
    const stored = window.localStorage.getItem(TOKEN_SOURCE_STORAGE_KEY);
    return isSourceKey(stored) ? stored : "workbuddy";
  } catch {
    return "workbuddy";
  }
}

function persistPreferredTokenSource(source: SourceKey): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(TOKEN_SOURCE_STORAGE_KEY, source);
  } catch {
    // localStorage 在受限 WebView/隐私模式下可能不可写，不影响页面切换。
  }
}

const RANGE_OPTIONS: { key: RangeKey; label: string }[] = [
  { key: "30d", label: "近 30 天" },
  { key: "today", label: "今天" },
  { key: "7d", label: "近 7 天" },
  { key: "month", label: "本月" },
];

const OVERVIEW_RANGE_OPTIONS: { key: OverviewRangeKey; label: string }[] = [
  { key: "today", label: "今日" },
  { key: "7d", label: "近 7 天" },
  { key: "30d", label: "近 30 天" },
  { key: "total", label: "总计" },
];

const chartConfig = {
  cacheRead: { label: "缓存读取", color: "var(--data-series-emerald)" },
  uncachedInput: { label: "新增输入", color: "var(--data-series-teal)" },
  output: { label: "输出", color: "var(--data-series-violet)" },
  cacheWrite: { label: "缓存写入", color: "var(--data-series-amber)" },
  records: { label: "调用次数", color: "var(--data-series-indigo)" },
} satisfies ChartConfig;

const compactTokenFormatter = new Intl.NumberFormat("en-US", {
  notation: "compact",
  compactDisplay: "short",
  maximumFractionDigits: 1,
});
const exact = new Intl.NumberFormat("zh-CN");
const exactTokenFormatter = new Intl.NumberFormat("en-US");

function formatTokenCompact(value: number): string {
  return compactTokenFormatter
    .format(value)
    .replace(/([kmb])$/i, (unit) => unit.toUpperCase());
}

function formatTokenExact(value: number): string {
  return exactTokenFormatter.format(value);
}

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

function formatHeatmapDate(date: Date): string {
  return date.toLocaleDateString("zh-CN", {
    month: "long",
    day: "numeric",
  });
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

function rangeBounds(range: RangeKey): { start: string; end: string } {
  const today = dateKey(new Date());
  if (range === "today") return { start: today, end: today };
  if (range === "7d") return { start: dateDaysAgo(6), end: today };
  if (range === "30d") return { start: dateDaysAgo(29), end: today };
  const monthStart = new Date();
  monthStart.setHours(12, 0, 0, 0);
  monthStart.setDate(1);
  return { start: dateKey(monthStart), end: today };
}

function fillRangePoints(points: TokenStatsGroup[], range: RangeKey): TokenStatsGroup[] {
  if (points.length === 0) return [];
  const { start, end } = rangeBounds(range);
  const byDate = new Map(points.map((point) => [point.key, point]));
  const cursor = new Date(`${start}T12:00:00`);
  const endDate = new Date(`${end}T12:00:00`);
  const filled: TokenStatsGroup[] = [];
  while (cursor <= endDate) {
    const key = dateKey(cursor);
    const point = byDate.get(key);
    filled.push(
      point ?? {
        key,
        total: 0,
        input: 0,
        output: 0,
        cacheRead: 0,
        cacheWrite: 0,
        uncachedInput: 0,
        records: 0,
        cacheHitRate: null,
      },
    );
    cursor.setDate(cursor.getDate() + 1);
  }
  return filled;
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

function overviewTotals(source: TokenStatsSource, range: OverviewRangeKey): TokenStatsTotals {
  if (range === "total") return source.summary;
  return rangeTotals(rangePoints(source.daily, range));
}

function SectionTitle({ id, children }: { id: string; children: ReactNode }) {
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

function CompactComposition({ value }: { value: TokenStatsTotals }) {
  const total = tokenTotal(value);
  const rows = [
    { label: "缓存占比", value: value.cacheRead, color: "bg-primary" },
    { label: "新增", value: value.uncachedInput, color: "bg-sky-500" },
    { label: "输出", value: value.output, color: "bg-violet-500" },
    { label: "写入", value: value.cacheWrite, color: "bg-amber-500" },
  ];

  return (
    <div className="min-w-0 flex-1">
      <div className="min-w-0">
        <div
          className="flex h-2 w-full max-w-[360px] overflow-hidden rounded-full bg-muted"
          role="img"
          aria-label={rows
            .map((row) => `${row.label} ${percentage(row.value, total)}`)
            .join("，")}
        >
          {rows.map((row) => (
            <span
              key={row.label}
              className={`h-full min-w-0 ${row.color}`}
              style={{ width: total > 0 ? `${(row.value / total) * 100}%` : "0%" }}
              title={`${row.label} ${formatTokenCompact(row.value)} · ${percentage(row.value, total)}`}
              aria-label={`${row.label} ${formatTokenExact(row.value)} Token，${percentage(row.value, total)}`}
            />
          ))}
        </div>
        <div className="mt-1 flex flex-wrap gap-x-2.5 gap-y-0.5 text-[10px] text-muted-foreground">
          {rows.map((row) => (
            <span key={row.label} className="inline-flex items-center gap-1 whitespace-nowrap">
              <span className={`size-1.5 rounded-full ${row.color}`} aria-hidden="true" />
              {row.label} {percentage(row.value, total)}
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}

function Overview({ source }: { source: TokenStatsSource }) {
  const [range, setRange] = useState<OverviewRangeKey>("today");
  const summary = useMemo(() => overviewTotals(source, range), [range, source]);
  const cacheRate = summary.cacheHitRate;

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-overview-title">
      <SectionTitle id="token-overview-title">Token 总览</SectionTitle>
      <Card
        className="min-w-0 gap-0 overflow-hidden rounded-2xl bg-card/70 py-0 shadow-none"
        aria-label="Token 总览"
      >
        <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
          <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <CompactComposition value={summary} />
            <Tabs
              className="min-w-0 shrink-0 gap-0"
              value={range}
              onValueChange={(value) => setRange(value as OverviewRangeKey)}
            >
              <TabsList
                className="grid h-auto w-full grid-cols-2 sm:inline-flex sm:w-fit sm:flex-wrap"
                aria-label="总览范围"
              >
                {OVERVIEW_RANGE_OPTIONS.map((option) => (
                  <TabsTrigger key={option.key} value={option.key} className="px-2">
                    {option.label}
                  </TabsTrigger>
                ))}
              </TabsList>
            </Tabs>
          </div>
        </CardHeader>
        <CardContent className="grid min-w-0 grid-cols-1 divide-y divide-border/60 p-0 sm:grid-cols-4 sm:divide-y-0 sm:py-5">
          <StatMetric icon={MessagesSquare} label="总 Token" value={formatTokenCompact(tokenTotal(summary))} />
          <StatMetric icon={ArrowDownToLine} label="输入 Token" value={formatTokenCompact(summary.input)} divided />
          <StatMetric
            icon={ArrowUpFromLine}
            label="输出 Token"
            value={formatTokenCompact(summary.output)}
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

type TrendChartPoint = TokenStatsGroup & { date: string };
type TokenSeriesKey = "cacheRead" | "uncachedInput" | "output" | "cacheWrite";
type TokenBarShapeProps = ComponentProps<typeof Rectangle> & {
  segmentKey: TokenSeriesKey;
  payload?: TrendChartPoint;
  value?: number | [number, number];
};

const TOKEN_SERIES: TokenSeriesKey[] = [
  "cacheRead",
  "uncachedInput",
  "output",
  "cacheWrite",
];

/**
 * 使用真实 Rectangle 形状绘制每个堆叠段：整柱约束分配小段的 5px 视觉保底，
 * 但不修改 data 值；每个日期实际顶部的非零段才使用顶部圆角。
 */
function TokenBarShape({
  segmentKey,
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
}: TokenBarShapeProps) {
  if (width <= 0 || height <= 0) return null;
  const segmentIndex = TOKEN_SERIES.indexOf(segmentKey);
  const stackStart = Array.isArray(value) ? Number(value[0]) : 0;
  const layout = payload
    ? getStackedSegmentVisualLayout({
        values: TOKEN_SERIES.map((key) => payload[key]),
        segmentIndex,
        segmentHeight: height,
        segmentY: y,
        stackStart,
      })
    : null;
  if (!layout) {
    return (
      <Rectangle
        {...rest}
        x={x}
        y={y}
        width={width}
        height={height}
        fill={fill}
        radius={0}
        stroke={stroke ?? "var(--background)"}
        strokeWidth={strokeWidth ?? 1}
      />
    );
  }

  return (
    <Rectangle
      {...rest}
      x={x}
      y={layout.y}
      width={width}
      height={layout.height}
      fill={fill}
      radius={layout.isTop ? [6, 6, 0, 0] : 0}
      stroke={stroke ?? "var(--background)"}
      strokeWidth={strokeWidth ?? 1}
    />
  );
}

function TrendLegend() {
  const items = [
    { key: "cacheRead", label: "缓存读取", color: "var(--data-series-emerald)", kind: "area" },
    { key: "uncachedInput", label: "新增输入", color: "var(--data-series-teal)", kind: "area" },
    { key: "output", label: "输出", color: "var(--data-series-violet)", kind: "area" },
    { key: "cacheWrite", label: "缓存写入", color: "var(--data-series-amber)", kind: "area" },
    { key: "records", label: "调用次数", color: "var(--data-series-indigo)", kind: "line" },
  ];

  return (
    <div
      className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-muted-foreground"
      aria-label="图表图例"
    >
      {items.map((item) => (
        <span key={item.key} className="inline-flex items-center gap-1.5 whitespace-nowrap">
          {item.kind === "line" ? (
            <span
              className="relative inline-flex h-2 w-4 shrink-0 items-center"
              aria-hidden="true"
            >
              <span
                className="absolute inset-x-0 top-1/2 border-t-2 border-dashed"
                style={{ borderColor: item.color }}
              />
              <span
                className="relative z-10 mx-auto size-1.5 rounded-full border border-background"
                style={{ backgroundColor: item.color }}
              />
            </span>
          ) : (
            <span
              className="size-2.5 shrink-0 rounded-[3px]"
              style={{ backgroundColor: item.color }}
              aria-hidden="true"
            />
          )}
          {item.label}
        </span>
      ))}
    </div>
  );
}

function TrendTooltipContent({
  active,
  payload,
}: {
  active?: boolean;
  payload?: Array<{ payload?: TrendChartPoint }>;
}) {
  if (!active || !payload?.length) return null;
  const point = payload[0]?.payload;
  if (!point) return null;
  const rows = [
    { key: "cacheRead", label: "缓存读取", value: point.cacheRead, color: "var(--data-series-emerald)" },
    {
      key: "uncachedInput",
      label: "新增输入",
      value: point.uncachedInput,
      color: "var(--data-series-teal)",
    },
    { key: "output", label: "输出", value: point.output, color: "var(--data-series-violet)" },
    { key: "cacheWrite", label: "缓存写入", value: point.cacheWrite, color: "var(--data-series-amber)" },
    { key: "records", label: "调用次数", value: point.records, color: "var(--data-series-indigo)" },
  ];
  const total = tokenTotal(point);

  return (
    <div className="grid min-w-[13rem] gap-2 rounded-lg border border-border/50 bg-background px-3 py-2.5 text-xs shadow-xl">
      <div className="font-medium text-foreground">{formatChartDate(point.date)}</div>
      <div className="flex items-center justify-between border-b border-border/60 pb-1.5">
        <span className="text-muted-foreground">Token 总量</span>
        <span
          className="whitespace-nowrap font-mono font-semibold tabular-nums text-foreground"
          title={`${formatTokenExact(total)} Token`}
          aria-label={`${formatTokenExact(total)} Token`}
        >
          {formatTokenCompact(total)} Token
        </span>
      </div>
      <div className="grid gap-1.5">
        {rows.map((row) => (
          <div key={row.key} className="flex items-center gap-2">
            <span
              className={`shrink-0 ${row.key === "records" ? "h-0 w-3 border-t-2 border-dashed" : "size-2.5 rounded-[3px]"}`}
              style={
                row.key === "records"
                  ? { borderColor: row.color }
                  : { backgroundColor: row.color }
              }
              aria-hidden="true"
            />
            <span className="flex-1 text-muted-foreground">{row.label}</span>
            <span
              className="whitespace-nowrap font-mono font-medium tabular-nums text-foreground"
              title={row.key === "records" ? undefined : `${formatTokenExact(row.value)} Token`}
              aria-label={row.key === "records" ? undefined : `${formatTokenExact(row.value)} Token`}
            >
              {row.key === "records" ? exact.format(row.value) : formatTokenCompact(row.value)} {row.key === "records" ? "次" : "Token"}
              {row.key !== "records" ? (
                <span className="ml-1 font-sans text-[11px] font-normal text-muted-foreground">
                  ({percentage(row.value, total)})
                </span>
              ) : null}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function TrendChart({ source }: { source: TokenStatsSource }) {
  const [range, setRange] = useState<RangeKey>("30d");
  const [modelFilter, setModelFilter] = useState("all");
  const modelOptions = useMemo(
    () => (source.dailyByModel ? source.models.map((model) => model.key).filter(Boolean) : []),
    [source.dailyByModel, source.models],
  );
  useEffect(() => {
    if (modelFilter !== "all" && !modelOptions.includes(modelFilter)) setModelFilter("all");
  }, [modelFilter, modelOptions]);
  const dailySeries = modelFilter === "all"
    ? source.daily
    : source.dailyByModel?.[modelFilter] ?? [];
  const points = useMemo(
    () => fillRangePoints(rangePoints(dailySeries, range), range),
    [range, dailySeries],
  );
  const totals = useMemo(() => rangeTotals(points), [points]);
  const chartData: TrendChartPoint[] = points.map((point) => ({
    ...point,
    date: point.key,
  }));

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-trend-title">
      <SectionTitle id="token-trend-title">
        Token 与调用趋势
      </SectionTitle>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
          <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <CardDescription className="min-w-0 text-xs">
              彩色堆叠柱表示每日总 Token 及构成，虚线表示调用次数。
            </CardDescription>
            <div className="flex max-w-full flex-wrap items-center gap-2">
              <Select value={modelFilter} onValueChange={setModelFilter}>
                <SelectTrigger size="sm" className="w-[9rem]" aria-label="按模型筛选">
                  <SelectValue placeholder="所有模型" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">所有模型</SelectItem>
                  {modelOptions.map((model) => <SelectItem key={model} value={model}>{model}</SelectItem>)}
                </SelectContent>
              </Select>
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
          {chartData.length === 0 ? (
            <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
              当前范围暂无可展示的 Token 数据。
            </div>
          ) : (
            <>
              <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
                <TrendLegend />
                <span className="text-[11px] text-muted-foreground">单位：Token / 次调用</span>
              </div>
              <div className="mb-1 flex items-center justify-between px-1 text-[11px] font-medium text-muted-foreground">
                <span>Token 总量、构成与调用</span>
                <span className="font-normal">左轴：Token · 右轴：调用次数</span>
              </div>
              <ChartContainer config={chartConfig} className="h-64 w-full sm:h-72">
                <ComposedChart
                  data={chartData}
                  margin={{ top: 8, right: 8, left: 0, bottom: 0 }}
                  barCategoryGap="18%"
                >
                  <CartesianGrid vertical={false} strokeDasharray="3 3" />
                  <XAxis
                    dataKey="date"
                    tickLine={false}
                    axisLine={false}
                    tickMargin={8}
                    minTickGap={24}
                    tickFormatter={(value) => formatChartDate(String(value))}
                  />
                  <YAxis
                    yAxisId="tokens"
                    tickLine={false}
                    axisLine={false}
                    width={48}
                    tickFormatter={(value) => formatTokenCompact(Number(value))}
                  />
                  <YAxis
                    yAxisId="calls"
                    orientation="right"
                    tickLine={false}
                    axisLine={false}
                    width={46}
                    allowDecimals={false}
                    tickFormatter={(value) => exact.format(Number(value))}
                  />
                  <ChartTooltip
                    cursor={{ fill: "var(--muted)", opacity: 0.4 }}
                    content={<TrendTooltipContent />}
                  />
                  {TOKEN_SERIES.map((key) => (
                    <Bar
                      key={key}
                      yAxisId="tokens"
                      dataKey={key}
                      stackId="token"
                      fill={`var(--color-${key})`}
                      stroke="var(--background)"
                      strokeWidth={2}
                      maxBarSize={28}
                      shape={<TokenBarShape segmentKey={key} />}
                      isAnimationActive={false}
                    >
                    </Bar>
                  ))}
                  <Line
                    yAxisId="calls"
                    type="monotone"
                    dataKey="records"
                    stroke="var(--color-records)"
                    strokeWidth={2}
                    strokeDasharray="7 4"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    dot={false}
                    activeDot={{ r: 4, fill: "var(--color-records)", stroke: "var(--background)", strokeWidth: 2 }}
                    isAnimationActive={false}
                  />
                </ComposedChart>
              </ChartContainer>
              <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
                <span>
                  {rangeLabel(range)}合计 {formatTokenCompact(tokenTotal(totals))} Token · {exact.format(totals.records)} 次调用
                </span>
                <span>数据覆盖至 {formatDateTime(source.coverageEndAt)}</span>
              </div>
              <p className="sr-only">
                {chartData
                  .map(
                    (point) =>
                      `${point.date} 使用 ${formatTokenCompact(tokenTotal(point))} Token，${exact.format(point.records)} 次调用`,
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

const HEATMAP_LEVEL_CLASS = [
  "bg-muted/70",
  "bg-primary/20",
  "bg-primary/40",
  "bg-primary/65",
  "bg-primary",
] as const;

function Heatmap({ groups }: { groups: TokenStatsGroup[] }) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const valueByDate = new Map(groups.map((group) => [group.key, tokenTotal(group)]));
  const recordByDate = new Map(groups.map((group) => [group.key, group.records]));
  const today = new Date();
  today.setHours(12, 0, 0, 0);
  const todayKey = dateKey(today);
  const start = new Date(today);
  start.setDate(start.getDate() - start.getDay() - 52 * 7);

  const weeks = Array.from({ length: 53 }, (_, weekIndex) =>
    Array.from({ length: 7 }, (_, dayIndex) => {
      const date = new Date(start);
      date.setDate(start.getDate() + weekIndex * 7 + dayIndex);
      const key = dateKey(date);
      return {
        date,
        key,
        value: valueByDate.get(key) ?? 0,
        records: recordByDate.get(key) ?? 0,
        future: key > todayKey,
      };
    }),
  );
  const max = Math.max(
    1,
    ...weeks.flatMap((week) => week.filter((day) => !day.future).map((day) => day.value)),
  );
  const monthLabels = weeks.map((week, weekIndex) => {
    const firstOfMonth = week.find((day) => day.date.getDate() === 1);
    let labelDate: Date | null = null;
    if (firstOfMonth && firstOfMonth.key <= todayKey) {
      labelDate = firstOfMonth.date;
    } else if (weekIndex === 0) {
      labelDate = week[0].date;
    }
    if (!labelDate || dateKey(labelDate) > todayKey) return null;
    return labelDate.toLocaleDateString("zh-CN", {
      month: "short",
    });
  });
  const activeDays = weeks
    .flat()
    .filter((day) => !day.future && day.value > 0).length;

  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    scroller.scrollLeft = scroller.scrollWidth - scroller.clientWidth;
  }, [groups]);

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-heatmap-title">
      <SectionTitle id="token-heatmap-title">Token 活动</SectionTitle>
      <Card className="min-w-0 gap-0 rounded-xl py-0 shadow-none">
        <CardHeader className="px-4 pt-4 pb-0 sm:px-5">
          <div className="flex items-center justify-between gap-3">
            <CardDescription className="text-xs">最近一年按天显示 Token 活跃度。</CardDescription>
            <span className="shrink-0 text-xs font-medium text-foreground">每日</span>
          </div>
        </CardHeader>
        <CardContent className="min-w-0 px-4 pt-5 pb-5 sm:px-5">
          <div ref={scrollerRef} className="overflow-x-auto pb-1">
            <div
              className="min-w-[760px]"
              role="img"
              aria-label={`最近一年每日 Token 活动热力图，共 ${activeDays} 个活跃日`}
            >
              <div
                className="grid gap-1"
                style={{ gridTemplateColumns: "repeat(53, minmax(10px, 1fr))" }}
                aria-hidden="true"
              >
                {weeks.flatMap((week, weekIndex) =>
                  week.map((day, dayIndex) => {
                    const level = day.value
                      ? Math.max(1, Math.ceil(Math.sqrt(day.value / max) * 4))
                      : 0;
                    const cell = (
                      <span
                        key={day.key}
                        className={`aspect-square min-w-0 rounded-[3px] ${
                          day.future ? "opacity-0" : HEATMAP_LEVEL_CLASS[level]
                        }`}
                        style={{ gridColumn: weekIndex + 1, gridRow: dayIndex + 1 }}
                        aria-label={`${formatHeatmapDate(day.date)}使用了 ${formatTokenExact(day.value)} 个 Token`}
                      />
                    );

                    if (day.future) return cell;

                    return (
                      <Tooltip key={day.key} disableHoverableContent>
                        <TooltipTrigger asChild>{cell}</TooltipTrigger>
                        <TooltipContent
                          side="top"
                          sideOffset={7}
                          className="pointer-events-none rounded-lg bg-foreground px-2.5 py-1.5 text-xs leading-4 text-background shadow-md"
                        >
                          {formatHeatmapDate(day.date)} 使用了 {formatTokenCompact(day.value)} 个 Token
                          {day.records > 0 ? ` · ${exact.format(day.records)} 次调用` : ""}
                        </TooltipContent>
                      </Tooltip>
                    );
                  }),
                )}
              </div>
              <div
                className="mt-3 grid gap-1 text-[11px] text-muted-foreground"
                style={{ gridTemplateColumns: "repeat(53, minmax(10px, 1fr))" }}
                aria-hidden="true"
              >
                {monthLabels.map((label, index) => (
                  <span key={`${index}-${label ?? "empty"}`} className="whitespace-nowrap">
                    {label}
                  </span>
                ))}
              </div>
            </div>
          </div>
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
  const rows = groups.slice(0, RANKING_LIMIT);

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
                <span
                  className="shrink-0 tabular-nums"
                  title={`${formatTokenExact(amount)} Token`}
                  aria-label={`${formatTokenExact(amount)} Token`}
                >
                  {formatTokenCompact(amount)}
                </span>
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
  const rows = groups.slice(0, RANKING_LIMIT);

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
                  <span
                    className="mt-0.5 shrink-0 tabular-nums"
                    title={`${formatTokenExact(amount)} Token`}
                    aria-label={`${formatTokenExact(amount)} Token`}
                  >
                    {formatTokenCompact(amount)}
                  </span>
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
              variant="ghost"
              size="sm"
              className={`h-7 px-2.5 text-xs ${
                distribution === "projects"
                  ? "bg-background font-medium text-foreground shadow-sm hover:bg-background"
                  : "text-muted-foreground"
              }`}
              aria-pressed={distribution === "projects"}
              onClick={() => setDistribution("projects")}
            >
              按项目
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className={`h-7 px-2.5 text-xs ${
                distribution === "models"
                  ? "bg-background font-medium text-foreground shadow-sm hover:bg-background"
                  : "text-muted-foreground"
              }`}
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
      <Heatmap groups={source.daily} />
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

function TokenStatsLoadingSkeleton() {
  return (
    <div
      className="min-w-0 space-y-12"
      role="status"
      aria-label="正在扫描本地会话日志…"
    >
      <span className="sr-only">正在扫描本地会话日志…</span>
      <p className="flex items-center gap-2 text-sm text-muted-foreground" aria-hidden="true">
        <span className="size-1.5 rounded-full bg-primary/70" />
        正在扫描本地会话日志…
      </p>

      <section className="min-w-0 space-y-2.5" aria-hidden="true">
        <Skeleton className="h-4 w-20" />
        <Card className="min-w-0 gap-0 overflow-hidden rounded-2xl bg-card/70 py-0 shadow-none">
          <CardContent className="grid min-w-0 grid-cols-1 divide-y divide-border/60 p-0 sm:grid-cols-4 sm:divide-y-0 sm:py-5">
            {Array.from({ length: 4 }, (_, index) => (
              <div
                key={index}
                className={`flex min-w-0 flex-col items-center justify-center px-4 py-5 sm:py-3 ${
                  index > 0 ? "sm:border-l sm:border-border/60" : ""
                }`}
              >
                <Skeleton className="h-4 w-24" />
                <Skeleton className="mt-3 h-8 w-28" />
              </div>
            ))}
          </CardContent>
        </Card>
      </section>

      <section className="min-w-0 space-y-2.5" aria-hidden="true">
        <Skeleton className="h-4 w-36" />
        <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
          <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
            <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
              <Skeleton className="h-4 w-64 max-w-full" />
              <Skeleton className="h-8 w-40 rounded-lg" />
            </div>
          </CardHeader>
          <CardContent className="min-w-0 px-4 pt-3 pb-4 sm:px-5">
            <div className="mb-3 flex flex-wrap items-center gap-4">
              {Array.from({ length: 5 }, (_, index) => (
                <Skeleton key={index} className="h-4 w-16" />
              ))}
            </div>
            <Skeleton className="h-64 w-full rounded-lg sm:h-72" />
          </CardContent>
        </Card>
      </section>

      <section className="min-w-0 space-y-2.5" aria-hidden="true">
        <Skeleton className="h-4 w-24" />
        <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
          <CardHeader className="px-4 pt-4 pb-0 sm:px-5">
            <Skeleton className="h-4 w-56 max-w-full" />
          </CardHeader>
          <CardContent className="px-4 pt-5 pb-5 sm:px-5">
            <Skeleton className="h-44 w-full rounded-lg" />
          </CardContent>
        </Card>
      </section>

      <section className="min-w-0 space-y-2.5" aria-hidden="true">
        <Skeleton className="h-4 w-24" />
        <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
          <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
            <div className="flex items-center justify-between gap-3">
              <Skeleton className="h-4 w-56 max-w-full" />
              <Skeleton className="h-8 w-32 rounded-lg" />
            </div>
          </CardHeader>
          <CardContent className="space-y-3 px-4 pt-3 pb-5 sm:px-5">
            {Array.from({ length: RANKING_LIMIT }, (_, index) => (
              <div key={index} className="space-y-1.5">
                <div className="flex items-center gap-3">
                  <Skeleton className="h-4 w-5" />
                  <Skeleton className="h-4 flex-1" />
                  <Skeleton className="h-4 w-16" />
                </div>
                <Skeleton className="ml-8 h-1.5 w-[70%] rounded-full" />
              </div>
            ))}
          </CardContent>
        </Card>
      </section>

      <section className="min-w-0 space-y-2.5" aria-hidden="true">
        <Skeleton className="h-4 w-36" />
        <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
          <CardHeader className="px-4 pt-3 pb-0 sm:px-5">
            <Skeleton className="h-4 w-56 max-w-full" />
          </CardHeader>
          <CardContent className="space-y-3 px-4 pt-3 pb-5 sm:px-5">
            {Array.from({ length: RANKING_LIMIT }, (_, index) => (
              <div key={index} className="space-y-1.5">
                <div className="flex items-start gap-3">
                  <Skeleton className="mt-0.5 h-4 w-5" />
                  <Skeleton className="h-8 flex-1" />
                  <Skeleton className="h-4 w-16" />
                </div>
                <Skeleton className="ml-8 h-1.5 w-[70%] rounded-full" />
              </div>
            ))}
          </CardContent>
        </Card>
      </section>
    </div>
  );
}

export default function TokenStatsPage() {
  const [stats, setStats] = useState<TokenStatistics | null>(null);
  const [active, setActive] = useState<SourceKey>(readPreferredTokenSource);
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

  useEffect(() => {
    if (!stats || stats.sources.length === 0) return;
    const available = stats.sources.map((item) => item.source);
    const next = available.includes(active)
      ? active
      : available.includes("workbuddy")
        ? "workbuddy"
        : available[0];
    if (!next) return;
    if (next !== active) setActive(next);
    persistPreferredTokenSource(next);
  }, [active, stats]);

  const source = stats?.sources.find((item) => item.source === active);

  return (
    <div className="mx-auto w-full max-w-[1180px] min-w-0 px-4 py-6 sm:px-8 sm:py-9">
      <header className="mb-10 flex min-w-0 flex-wrap items-start justify-between gap-4 sm:mb-12">
        <div className="min-w-0">
          {loading && !stats ? (
            <div aria-hidden="true">
              <Skeleton className="h-8 w-40" />
              <Skeleton className="mt-2 h-5 w-64 max-w-full" />
            </div>
          ) : (
            <>
              <h1 className="text-[28px] font-semibold tracking-tight">Token 统计</h1>
              <p className="mt-2 max-w-2xl text-sm leading-6 text-muted-foreground">
                当前数据更新于 {stats ? formatDateTime(stats.generatedAt) : "—"}
              </p>
            </>
          )}
        </div>
        <div className="flex max-w-full flex-wrap items-center justify-end gap-2">
          {loading && !stats ? (
            <Skeleton className="h-8 w-44 rounded-lg" aria-hidden="true" />
          ) : (
            <Tabs
              className="min-w-0 gap-0"
              value={active}
              onValueChange={(value) => {
                if (!isSourceKey(value)) return;
                if (stats && !stats.sources.some((item) => item.source === value)) return;
                setActive(value);
              }}
            >
              <TabsList className="h-auto max-w-full flex-wrap" aria-label="Token 数据来源">
                <TabsTrigger
                  className="max-w-full whitespace-normal"
                  value="workbuddy"
                  disabled={Boolean(stats && !stats.sources.some((item) => item.source === "workbuddy"))}
                >
                  WorkBuddy
                </TabsTrigger>
                <TabsTrigger
                  className="max-w-full whitespace-normal"
                  value="codebuddy-cli"
                  disabled={Boolean(stats && !stats.sources.some((item) => item.source === "codebuddy-cli"))}
                >
                  CodeBuddy CLI
                </TabsTrigger>
              </TabsList>
            </Tabs>
          )}
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

      {loading && !stats ? <TokenStatsLoadingSkeleton /> : source ? (
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
