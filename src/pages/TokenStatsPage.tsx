import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ComponentProps, ReactNode } from "react";
import {
  CircleAlert,
  ArrowDownToLine,
  ArrowUpFromLine,
  Check,
  ChevronLeft,
  ChevronRight,
  Gauge,
  ListTree,
  Loader2,
  MessagesSquare,
  RefreshCw,
  SlidersHorizontal,
  Zap,
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
import { AccountFilterMenu } from "@/components/account-filter-menu";
import { buildAccountTokenTrend, tokenAccountKey, tokenTotal, type AccountTokenSeries, type TokenTrendPoint } from "@/lib/token-account-stats";
import { DemoAction } from "@/components/demo-action";
import { CodeBuddyCnIdeMark, CodeBuddyMark, WorkBuddyAiMark, WorkBuddyMark } from "@/components/product-marks";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Skeleton } from "@/components/ui/skeleton";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import * as api from "@/lib/api";
import { getStackedSegmentVisualLayout } from "@/lib/stacked-bar-visuals";
import type {
  TokenStatistics,
  TokenStatsGroup,
  TokenStatsRequestRow,
  TokenStatsSource,
  TokenStatsTotals,
} from "@/lib/types";

type SourceKey = TokenStatsSource["source"];
type RangeKey = "30d" | "today" | "7d" | "month";
type OverviewRangeKey = "today" | "7d" | "30d" | "total";
type DistributionKey = "projects" | "models";

const TOKEN_SOURCE_STORAGE_KEY = "wb-switch:token-stats:source";
const RANKING_LIMIT = 10;
const REQUEST_PAGE_SIZE = 50;

function isSourceKey(value: unknown): value is SourceKey {
  return (
    value === "workbuddy" ||
    value === "workbuddy-ai" ||
    value === "codebuddy-cli" ||
    value === "codebuddy-ide"
  );
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

const percentage = (value: number, sum: number) =>
  sum > 0 ? `${((value / sum) * 100).toFixed(1)}%` : "—";

const pad2 = (value: number) => String(value).padStart(2, "0");

function dateKey(date: Date): string {
  return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())}`;
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

/** 请求明细的精确时间：本地时区 `YYYY-MM-DD HH:mm:ss`。 */
function formatRequestTime(timestamp: number): string {
  const date = new Date(timestamp);
  return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())} ${pad2(date.getHours())}:${pad2(date.getMinutes())}:${pad2(date.getSeconds())}`;
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
          <StatMetric icon={ArrowUpFromLine} label="输入 Token" value={formatTokenCompact(summary.input)} divided />
          <StatMetric
            icon={ArrowDownToLine}
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

type TrendChartPoint = TokenTrendPoint;
type TokenSeriesKey = "cacheRead" | "uncachedInput" | "output" | "cacheWrite";
type TokenBarShapeProps = ComponentProps<typeof Rectangle> & {
  segmentKey: string;
  seriesKeys?: string[];
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
 * 不修改 data 值，所有分段（含柱顶）统一使用直角矩形。
 */
function TokenBarShape({
  segmentKey,
  seriesKeys = TOKEN_SERIES,
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
  const segmentIndex = seriesKeys.indexOf(segmentKey);
  const stackStart = Array.isArray(value) ? Number(value[0]) : 0;
  const layout = payload
    ? getStackedSegmentVisualLayout({
        values: seriesKeys.map((key) => payload.accountTokens?.[key] ?? payload[key as TokenSeriesKey] ?? 0),
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
      radius={0}
      stroke={stroke ?? "var(--background)"}
      strokeWidth={strokeWidth ?? 1}
    />
  );
}

function TrendLegend({ series, total = 0 }: { series?: AccountTokenSeries[]; total?: number }) {
  const items = series ? [
    ...series.map((item) => ({ ...item, label: `${item.label} ${formatTokenCompact(item.total)} Token (${percentage(item.total, total)})`, kind: "area" })),
    { key: "records", label: "调用次数", color: "var(--data-series-indigo)", kind: "line" },
  ] : [
    { key: "cacheRead", label: "缓存读取", color: "var(--data-series-emerald)", kind: "area" },
    { key: "uncachedInput", label: "新增输入", color: "var(--data-series-teal)", kind: "area" },
    { key: "output", label: "输出", color: "var(--data-series-violet)", kind: "area" },
    { key: "cacheWrite", label: "缓存写入", color: "var(--data-series-amber)", kind: "area" },
    { key: "records", label: "调用次数", color: "var(--data-series-indigo)", kind: "line" },
  ];

  return (
    <div
      className="flex max-h-24 min-w-0 flex-wrap items-center gap-x-4 gap-y-1.5 overflow-y-auto text-xs text-muted-foreground"
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
  series,
}: {
  active?: boolean;
  payload?: Array<{ payload?: TrendChartPoint }>;
  series?: AccountTokenSeries[];
}) {
  if (!active || !payload?.length) return null;
  const point = payload[0]?.payload;
  if (!point) return null;
  const rows = series ? [
    ...series.map((item) => ({ ...item, value: point.accountTokens?.[item.key] ?? 0 })).filter((row) => row.value > 0),
    { key: "records", label: "调用次数", value: point.records, color: "var(--data-series-indigo)" },
  ] : [
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
      <div className="grid max-h-72 gap-1.5 overflow-y-auto">
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

function TrendPlot({ points, series }: { points: TrendChartPoint[]; series?: AccountTokenSeries[] }) {
  const seriesKeys = series?.map((item) => item.key) ?? TOKEN_SERIES;
  const config: ChartConfig = series
    ? { ...Object.fromEntries(series.map((item) => [item.key, { label: item.label, color: item.color }])), records: chartConfig.records }
    : chartConfig;
  return (
    <>
      <div className="mb-1 flex items-center justify-end px-1 text-[11px] font-medium text-muted-foreground">
        <span className="font-normal">左轴：Token · 右轴：调用次数</span>
      </div>
      <ChartContainer config={config} className="h-64 w-full sm:h-72">
        <ComposedChart
          data={points}
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
            content={<TrendTooltipContent series={series} />}
          />
          {seriesKeys.map((key) => (
            <Bar
              key={key}
              yAxisId="tokens"
              dataKey={series ? `accountTokens.${key}` : key}
              stackId="token"
              fill={`var(--color-${key})`}
              stroke="var(--background)"
              strokeWidth={2}
              maxBarSize={28}
              shape={<TokenBarShape segmentKey={key} seriesKeys={seriesKeys} />}
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
      <p className="sr-only">
        {points
          .map(
            (point) =>
              `${point.date} 使用 ${formatTokenCompact(tokenTotal(point))} Token，${exact.format(point.records)} 次调用`,
          )
          .join("；")}
      </p>
    </>
  );
}

function TrendControls({ range, setRange, modelFilter, setModelFilter, modelOptions }: {
  range: RangeKey;
  setRange: (range: RangeKey) => void;
  modelFilter: string;
  setModelFilter: (model: string) => void;
  modelOptions: string[];
}) {
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            className="h-8 max-w-[190px] gap-1.5 px-2.5 text-xs text-muted-foreground hover:text-foreground"
            aria-label="按模型筛选"
          >
            <SlidersHorizontal className="size-3.5 shrink-0" />
            <span className="truncate">{modelFilter === "all" ? "所有模型" : modelFilter}</span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="max-h-80 w-56 overflow-y-auto">
          <DropdownMenuItem onSelect={() => setModelFilter("all")}>
            <SlidersHorizontal className="size-3.5 shrink-0" />
            所有模型
            {modelFilter === "all" && <Check className="ml-auto size-3.5 shrink-0" />}
          </DropdownMenuItem>
          {modelOptions.length > 0 && <DropdownMenuSeparator />}
          {modelOptions.map((model) => (
            <DropdownMenuItem key={model} onSelect={() => setModelFilter(model)}>
              <span className="min-w-0 flex-1 truncate">{model}</span>
              {modelFilter === model && <Check className="ml-auto size-3.5 shrink-0" />}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
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
    </>
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
              <TrendControls range={range} setRange={setRange} modelFilter={modelFilter} setModelFilter={setModelFilter} modelOptions={modelOptions} />
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
              </div>
              <TrendPlot points={chartData} />
              <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
                <span>
                  {rangeLabel(range)}合计 {formatTokenCompact(tokenTotal(totals))} Token · {exact.format(totals.records)} 次调用
                </span>
                <span>数据覆盖至 {formatDateTime(source.coverageEndAt)}</span>
              </div>

            </>
          )}
        </CardContent>
      </Card>
    </section>
  );
}

function AccountTrendChart({ source }: { source: TokenStatsSource }) {
  const [accountFilter, setAccountFilter] = useState<string | null>(null);
  const [range, setRange] = useState<RangeKey>("30d");
  const [modelFilter, setModelFilter] = useState("all");
  const accounts = source.accounts;
  const filterAccounts = useMemo(() => (accounts ?? []).map((account) => ({
    accountId: tokenAccountKey(account.accountId), accountName: account.accountName,
  })), [accounts]);
  const selectedAccount = accounts?.find((account) => tokenAccountKey(account.accountId) === accountFilter);
  const modelOptions = useMemo(() => source.models.map((model) => model.key).filter(Boolean), [source.models]);
  useEffect(() => {
    if (accountFilter !== null && !selectedAccount) setAccountFilter(null);
  }, [accountFilter, selectedAccount]);
  useEffect(() => {
    if (modelFilter !== "all" && !modelOptions.includes(modelFilter)) setModelFilter("all");
  }, [modelFilter, modelOptions]);
  const { start, end } = rangeBounds(range);
  const overview = useMemo(() => buildAccountTokenTrend(accounts ?? [], modelFilter, start, end), [accounts, modelFilter, start, end]);
  const daily = selectedAccount
    ? modelFilter === "all" ? selectedAccount.daily : selectedAccount.dailyByModel[modelFilter] ?? []
    : overview.points;
  const points: TrendChartPoint[] = fillRangePoints(rangePoints(daily, range), range)
    .map((point) => ({ ...point, date: point.key }));
  const totals = rangeTotals(points);
  const series = selectedAccount ? undefined : overview.series;

  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby="token-account-trend-title">
      <SectionTitle id="token-account-trend-title">账号 Token 与调用情况</SectionTitle>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
        <CardHeader className="gap-0 px-4 pt-3 pb-0 sm:px-5">
          <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <CardDescription className="min-w-0 text-xs">
              {selectedAccount ? "彩色堆叠柱表示该账号每日 Token 构成，虚线表示调用次数。" : "每日 Token 按账号分色堆叠，虚线表示总调用次数。"}
            </CardDescription>
            {accounts && (
              <div className="ml-auto flex max-w-full flex-wrap items-center justify-end gap-2">
                <AccountFilterMenu accounts={filterAccounts} accountFilter={selectedAccount ? accountFilter : null} onAccountFilterChange={setAccountFilter} ariaLabel="按账号筛选 Token 趋势" />
                <TrendControls range={range} setRange={setRange} modelFilter={modelFilter} setModelFilter={setModelFilter} modelOptions={modelOptions} />
              </div>
            )}
          </div>
        </CardHeader>
        <CardContent className="min-w-0 px-4 pt-3 pb-4 sm:px-5">
          {accounts === undefined ? (
            <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
              账号统计暂不可用，当前后端尚未提供账号维度数据。
            </div>
          ) : points.length === 0 ? (
            <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
              当前范围暂无可展示的 Token 数据。
            </div>
          ) : (
            <>
              <div className="mb-2"><TrendLegend series={series} total={tokenTotal(totals)} /></div>
              <TrendPlot points={points} series={series} />
              <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
                <span>{rangeLabel(range)}合计 {formatTokenCompact(tokenTotal(totals))} Token · {exact.format(totals.records)} 次调用</span>
                <span>数据覆盖至 {formatDateTime(source.coverageEndAt)}</span>
              </div>
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

/**
 * 详情卡的一行：分类色块 + 数值。`depth = 1` 表示父项（输入 / 输出）的拆分项，
 * 只缩进、不再画色块，保持「父项 → 子项」的视觉层次。
 */
function UsageDetailLine({
  label,
  value,
  color,
  depth = 0,
}: {
  label: string;
  value: number;
  color?: string;
  depth?: 0 | 1;
}) {
  return (
    <div className={`flex items-center justify-between gap-3 ${depth === 1 ? "pl-4" : ""}`}>
      <span className="inline-flex items-center gap-1.5 text-muted-foreground">
        {color ? (
          <span
            className="size-2 shrink-0 rounded-[2px]"
            style={{ backgroundColor: color }}
            aria-hidden="true"
          />
        ) : null}
        {label}
      </span>
      <span className="tabular-nums">{formatTokenExact(value)}</span>
    </div>
  );
}

/**
 * 「用量」单元格：默认只显示总计，副行是 输入 / 输出 / 命中率；悬停或键盘聚焦
 * 弹出「Token 消耗明细」分类卡（Radix HoverCard 自带 focus 触发，不需要本地状态）。
 *
 * 口径（与后端明细行一致，见 design §10.2）：总计 = 输入 + 输出 + 缓存写入；
 * 输入 = 缓存命中 + 缓存未命中；输出 = 思考过程 + 回复内容（饱和减）。
 * 缓存写入与输入 / 输出同级：本项目 `input` 不含 cacheWrite，嵌进输入父子加不上。
 */
function RequestUsageCell({ row }: { row: TokenStatsRequestRow }) {
  // 旧后端可能没有 thinking 键，按 0 兜底。
  const thinking = row.thinking ?? 0;
  const reply = Math.max(0, row.output - thinking);
  const segments = [
    { label: "命中", value: row.cacheRead, color: "var(--data-series-emerald)" },
    { label: "未命中", value: row.uncachedInput, color: "var(--data-series-rose)" },
    { label: "写入", value: row.cacheWrite, color: "var(--data-series-amber)" },
  ];
  const legend = [
    { label: "命中", color: "var(--data-series-emerald)" },
    { label: "写入", color: "var(--data-series-amber)" },
    { label: "未命中", color: "var(--data-series-rose)" },
  ];

  return (
    <HoverCard openDelay={150} closeDelay={80}>
      <HoverCardTrigger asChild>
        <TableCell
          tabIndex={0}
          className="w-[150px] rounded-md text-right outline-hidden focus-visible:ring-2 focus-visible:ring-ring/50"
        >
          <span className="block text-[15px] font-medium tabular-nums">
            {formatTokenCompact(row.total)}
          </span>
          <span className="mt-0.5 flex items-center justify-end gap-2 text-[11px] text-muted-foreground tabular-nums">
            <span className="inline-flex items-center gap-0.5" title="输入">
              <ArrowUpFromLine className="size-3" aria-hidden="true" />
              {formatTokenCompact(row.input)}
            </span>
            <span className="inline-flex items-center gap-0.5" title="输出">
              <ArrowDownToLine className="size-3" aria-hidden="true" />
              {formatTokenCompact(row.output)}
            </span>
            <span title="缓存命中率：缓存命中 / 输入">{percentage(row.cacheRead, row.input)}</span>
          </span>
        </TableCell>
      </HoverCardTrigger>
      {/* 贴在单元格左侧：这张卡比「用量」列高得多，若开在下方会盖住后面几行的
          用量数字（正在纵向对比时最碍事）。靠左后在垂直方向仍由碰撞检测兜底。 */}
      <HoverCardContent
        side="left"
        align="center"
        sideOffset={6}
        collisionPadding={8}
        className="w-[264px] space-y-1.5 text-xs"
      >
        <div className="flex items-center justify-between gap-3">
          <span className="font-medium">Token 消耗明细</span>
          <span className="text-muted-foreground tabular-nums">
            总计 {formatTokenExact(row.total)}
          </span>
        </div>
        <div className="space-y-1 border-t pt-1.5">
          <UsageDetailLine label="输入" value={row.input} color="var(--data-series-sky)" />
          <UsageDetailLine
            label="缓存命中"
            value={row.cacheRead}
            color="var(--data-series-emerald)"
            depth={1}
          />
          <UsageDetailLine
            label="缓存未命中"
            value={row.uncachedInput}
            color="var(--data-series-rose)"
            depth={1}
          />
          <UsageDetailLine label="输出" value={row.output} color="var(--data-series-violet)" />
          <UsageDetailLine label="思考过程" value={thinking} depth={1} />
          <UsageDetailLine label="回复内容" value={reply} depth={1} />
          <UsageDetailLine
            label="缓存写入"
            value={row.cacheWrite}
            color="var(--data-series-amber)"
          />
        </div>
        <div className="flex items-center justify-between gap-3 border-t pt-1.5">
          <span className="inline-flex items-center gap-1 text-muted-foreground">
            <Zap className="size-3" aria-hidden="true" />
            缓存命中率
          </span>
          <span className="font-medium tabular-nums" style={{ color: "var(--data-series-emerald)" }}>
            {percentage(row.cacheRead, row.input)}
          </span>
        </div>
        <div className="space-y-1.5 border-t pt-1.5">
          {/* 轨道用前景色透明度：暗色主题下 `--muted` 与卡片同色，背景色会看不见。 */}
          <div className="flex h-2 w-full overflow-hidden rounded-full bg-foreground/10">
            {segments.map((segment) =>
              segment.value > 0 ? (
                <div
                  key={segment.label}
                  className="h-full shrink"
                  style={{
                    flexBasis: 0,
                    flexGrow: segment.value,
                    // 占比极小的段也要看得见，但不改变其余段的比例关系。
                    minWidth: 4,
                    backgroundColor: segment.color,
                  }}
                />
              ) : null,
            )}
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
            {legend.map((item) => (
              <span key={item.label} className="inline-flex items-center gap-1">
                <span
                  className="size-2 shrink-0 rounded-[2px]"
                  style={{ backgroundColor: item.color }}
                  aria-hidden="true"
                />
                {item.label}
              </span>
            ))}
          </div>
        </div>
      </HoverCardContent>
    </HoverCard>
  );
}

/**
 * 明细表格与分页。页状态挂在 `DialogContent` 的子组件里：Radix 关闭即卸载，
 * 因此重新打开弹框会自动回到第 1 页，不需要额外的重置逻辑。
 */
function RequestDetailRows({ rows, records }: { rows: TokenStatsRequestRow[]; records: number }) {
  const [page, setPage] = useState(0);
  const pageCount = Math.max(1, Math.ceil(rows.length / REQUEST_PAGE_SIZE));
  const start = page * REQUEST_PAGE_SIZE;
  const pageRows = rows.slice(start, start + REQUEST_PAGE_SIZE);
  const shownEnd = start + pageRows.length;

  return (
    <>
      {/* 两个方向都由本容器滚动，表头才能 sticky：Table 自带的横向滚动 wrapper
          会让 overflow-y 的计算值变成 auto，吃掉 sticky 的参照系（见 table.tsx）。 */}
      <div className="min-h-0 flex-1 overflow-auto rounded-lg border">
        <Table containerClassName="overflow-visible" className="min-w-[720px]">
          <TableHeader className="sticky top-0 z-10 bg-background [&_th]:bg-background">
            <TableRow className="hover:bg-transparent">
              <TableHead>时间</TableHead>
              <TableHead>模型</TableHead>
              <TableHead>会话</TableHead>
              <TableHead>项目</TableHead>
              <TableHead className="w-[150px] text-right">用量</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {pageRows.map((row, index) => {
              const sessionTitle = row.title?.trim();
              const sessionLabel = sessionTitle || row.sessionId.slice(0, 8);
              return (
                <TableRow key={`${row.timestamp}-${row.sessionId}-${start + index}`}>
                  <TableCell className="tabular-nums text-muted-foreground">
                    {formatRequestTime(row.timestamp)}
                  </TableCell>
                  <TableCell className="max-w-[200px] truncate" title={row.model}>
                    {row.model}
                  </TableCell>
                  <TableCell
                    className="max-w-[220px] truncate"
                    title={sessionTitle || row.sessionId}
                  >
                    {sessionLabel}
                  </TableCell>
                  <TableCell className="max-w-[160px] truncate" title={row.project}>
                    {row.project}
                  </TableCell>
                  <RequestUsageCell row={row} />
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-[11px] text-muted-foreground">
          {rows.length < records
            ? `仅展示最近 ${exact.format(rows.length)} 条（共 ${exact.format(records)} 次调用）`
            : `共 ${exact.format(rows.length)} 次调用`}
        </span>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setPage((value) => Math.max(0, value - 1))}
            disabled={page === 0}
          >
            <ChevronLeft />
            上一页
          </Button>
          <span className="text-[11px] text-muted-foreground tabular-nums">
            第 {exact.format(start + 1)}–{exact.format(shownEnd)} 条 · 共 {exact.format(rows.length)} 条
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setPage((value) => Math.min(pageCount - 1, value + 1))}
            disabled={page >= pageCount - 1}
          >
            下一页
            <ChevronRight />
          </Button>
        </div>
      </div>
    </>
  );
}

function RequestDetailDialog({
  open,
  onOpenChange,
  source,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  source: TokenStatsSource;
}) {
  const rows = source.requests ?? [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[85vh] min-w-0 flex-col gap-3 sm:max-w-5xl">
        <DialogHeader>
          <DialogTitle>请求明细</DialogTitle>
          <DialogDescription>
            每次模型调用一行，按时间倒序展示本地 CodeBuddy CLI 日志记录。
          </DialogDescription>
        </DialogHeader>
        {rows.length === 0 ? (
          <div className="rounded-lg border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
            该来源暂无可展示的请求明细。
          </div>
        ) : (
          <RequestDetailRows rows={rows} records={source.summary.records} />
        )}
      </DialogContent>
    </Dialog>
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
        {source.source === "workbuddy-ai" && (
          <div className="mt-2 text-xs leading-5">
            国际版数据源为空：本机可能未安装 WorkBuddy 国际版客户端，或尚未产生本地会话日志；
            国际版数据与国内版分开统计，不参与国内版用量。
          </div>
        )}
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
      <AccountTrendChart key={source.source} source={source} />
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
  const [detailOpen, setDetailOpen] = useState(false);

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
      <header className="mb-6 flex min-w-0 flex-wrap items-start justify-between gap-4">
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
          {active === "codebuddy-cli" && (
            <Button
              className="shrink-0"
              variant="outline"
              size="sm"
              onClick={() => setDetailOpen(true)}
            >
              <ListTree />
              查看请求明细
            </Button>
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

      {/* 数据源 Tab 独占一行：数据源变多时不与右上角操作挤在同一排 */}
      {loading && !stats ? (
        <Skeleton className="mb-8 h-9 w-full max-w-md rounded-lg" aria-hidden="true" />
      ) : (
        <Tabs
          className="mb-8 min-w-0 gap-0"
          value={active}
          onValueChange={(value) => {
            if (!isSourceKey(value)) return;
            if (stats && !stats.sources.some((item) => item.source === value)) return;
            setActive(value);
          }}
        >
          <TabsList className="h-auto max-w-full flex-wrap" aria-label="Token 数据来源">
            <TabsTrigger
              className="max-w-full gap-2 whitespace-normal"
              value="workbuddy"
              disabled={Boolean(stats && !stats.sources.some((item) => item.source === "workbuddy"))}
            >
              <WorkBuddyMark size={16} />
              WorkBuddy
            </TabsTrigger>
            <TabsTrigger
              className="max-w-full gap-2 whitespace-normal"
              value="workbuddy-ai"
              disabled={Boolean(stats && !stats.sources.some((item) => item.source === "workbuddy-ai"))}
            >
              <WorkBuddyAiMark size={16} />
              WorkBuddy 国际版
            </TabsTrigger>
            <TabsTrigger
              className="max-w-full gap-2 whitespace-normal"
              value="codebuddy-cli"
              disabled={Boolean(stats && !stats.sources.some((item) => item.source === "codebuddy-cli"))}
            >
              <CodeBuddyMark size={16} />
              CodeBuddy CLI
            </TabsTrigger>
            <TabsTrigger
              className="max-w-full gap-2 whitespace-normal"
              value="codebuddy-ide"
              disabled={Boolean(stats && !stats.sources.some((item) => item.source === "codebuddy-ide"))}
            >
              <CodeBuddyCnIdeMark size={16} />
              CodeBuddy IDE / VS Code
            </TabsTrigger>
          </TabsList>
        </Tabs>
      )}

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

      {source && (
        <RequestDetailDialog
          open={detailOpen}
          onOpenChange={setDetailOpen}
          source={source}
        />
      )}
    </div>
  );
}
