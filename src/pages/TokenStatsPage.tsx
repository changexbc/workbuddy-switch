import { useEffect, useMemo, useState } from "react";
import { Activity, Database, Gauge, MessagesSquare, RefreshCw } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { Bar, CartesianGrid, ComposedChart, Line, XAxis, YAxis } from "recharts";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { ChartContainer, ChartTooltip, ChartTooltipContent, type ChartConfig } from "@/components/ui/chart";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import * as api from "@/lib/api";
import type { TokenStatsGroup, TokenStatsSource, TokenStatsTotals } from "@/lib/types";

type SourceKey = TokenStatsSource["source"];
type RangeKey = "7d" | "30d" | "90d" | "all";
type DistributionKey = "projects" | "models";

const ranges: { key: RangeKey; label: string; days?: number }[] = [
  { key: "7d", label: "近 7 天", days: 7 },
  { key: "30d", label: "近 30 天", days: 30 },
  { key: "90d", label: "近 90 天", days: 90 },
  { key: "all", label: "全部" },
];

const chartConfig = {
  cacheRead: { label: "缓存读取", color: "var(--primary)" },
  uncachedInput: { label: "新增输入", color: "#55a9d8" },
  output: { label: "输出", color: "#9a80d8" },
  records: { label: "调用", color: "#d6973d" },
} satisfies ChartConfig;

const compact = new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 1, notation: "compact" });
const exact = new Intl.NumberFormat("zh-CN");

/** 展示总量：input 已包含 cacheRead，因此不能再次加上 cacheRead。 */
const tokenTotal = (value: TokenStatsTotals) => value.input + value.output + value.cacheWrite;
const percentage = (value: number, sum: number) => sum > 0 ? `${(value / sum * 100).toFixed(1)}%` : "—";

function formatDateTime(timestamp?: number | null): string {
  if (!timestamp) return "—";
  return new Date(timestamp).toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

function rangeLabel(range: RangeKey): string {
  return ranges.find((item) => item.key === range)?.label ?? "全部";
}

function Metric({ icon: Icon, label, value, divided = false }: { icon: LucideIcon; label: string; value: string; divided?: boolean }) {
  return <div className={`flex min-w-0 flex-col items-center justify-center px-4 py-4 text-center ${divided ? "sm:border-l sm:border-border/60" : ""}`}><div className="flex max-w-full items-center justify-center gap-2 text-[13px] font-medium leading-5 text-muted-foreground"><Icon className="size-4 shrink-0 stroke-[1.75]" aria-hidden="true" /><span className="truncate">{label}</span></div><div className="mt-2 max-w-full truncate text-[25px] font-semibold tracking-[-0.025em] text-foreground tabular-nums" style={{ fontFamily: '"Bricolage Grotesque Variable", "SF Pro Display", ui-sans-serif, sans-serif' }}>{value}</div></div>;
}

function Composition({ value }: { value: TokenStatsTotals }) {
  const sum = tokenTotal(value);
  const rows = [{ label: "缓存读取", value: value.cacheRead, color: "bg-primary" }, { label: "新增输入", value: value.uncachedInput, color: "bg-sky-500" }, { label: "输出", value: value.output, color: "bg-violet-500" }, { label: "缓存写入", value: value.cacheWrite, color: "bg-amber-500" }];
  return <Card className="gap-0 rounded-xl py-0 shadow-none"><CardHeader className="px-4 pb-2 pt-4 sm:px-5"><CardTitle className="text-[13px]">Token 构成</CardTitle><CardDescription className="text-xs">输入拆分为缓存读取与真正新增的上下文。</CardDescription></CardHeader><CardContent className="space-y-4 px-4 pb-5 sm:px-5">{rows.map((row) => { const share = percentage(row.value, sum); return <div key={row.label}><div className="mb-1.5 flex items-center justify-between gap-2 text-xs"><span className="flex items-center gap-2"><span className={`size-2.5 rounded-[3px] ${row.color}`} aria-hidden="true" />{row.label}</span><span className="shrink-0 tabular-nums">{compact.format(row.value)} <span className="ml-1 text-muted-foreground">{share}</span></span></div><div className="h-1.5 overflow-hidden rounded-full bg-muted"><div className={`h-full rounded-full ${row.color}`} style={{ width: share === "—" ? "0%" : share }} /></div></div>; })}</CardContent></Card>;
}

function Heatmap({ groups }: { groups: TokenStatsGroup[] }) {
  const map = new Map(groups.map((group) => [`${group.key}`, tokenTotal(group)]));
  const max = Math.max(1, ...map.values());
  const days = ["一", "二", "三", "四", "五", "六", "日"];
  return <Card className="min-w-0 gap-0 rounded-xl py-0 shadow-none"><CardHeader className="px-4 pb-2 pt-4 sm:px-5"><CardTitle className="text-[13px]">使用热力图</CardTitle><CardDescription className="text-xs">按本地星期与小时汇总 Token 活跃度。</CardDescription></CardHeader><CardContent className="min-w-0 px-4 pb-5 sm:px-5">{map.size === 0 ? <div className="py-8 text-center text-sm text-muted-foreground">当前范围暂无小时数据</div> : <div role="img" aria-label="按星期和小时显示的 Token 使用热力图"><div className="mb-1 ml-6 flex justify-between text-[10px] text-muted-foreground"><span>0</span><span>6</span><span>12</span><span>18</span><span>23</span></div><div className="grid grid-cols-[12px_repeat(24,minmax(3px,1fr))] gap-0.5">{days.map((day, dayIndex) => <div className="contents" key={day}><span className="flex items-center text-[10px] text-muted-foreground">{day}</span>{Array.from({ length: 24 }, (_, hour) => { const value = map.get(`${dayIndex}-${hour}`) ?? 0; const opacity = value ? 0.12 + 0.88 * Math.sqrt(value / max) : 0.04; return <span key={hour} className="aspect-square min-w-0 rounded-[2px] bg-foreground" style={{ opacity }} title={`周${day} ${String(hour).padStart(2, "0")}:00 · ${exact.format(value)} Token`} aria-label={`周${day} ${hour} 时 ${exact.format(value)} Token`} />; })}</div>)}</div></div>}</CardContent></Card>;
}

function Ranking({ title, groups, denominator }: { title: string; groups: TokenStatsGroup[]; denominator: number }) {
  const rows = groups.slice(0, 8);
  return <Card className="gap-0 rounded-xl py-0 shadow-none"><CardHeader className="px-4 pb-2 pt-4 sm:px-5"><CardTitle className="text-[13px]">{title}</CardTitle><CardDescription className="text-xs">按本地聚合 Token 从高到低排列。</CardDescription></CardHeader><CardContent className="space-y-3 px-4 pb-5 sm:px-5">{rows.map((row, index) => { const amount = tokenTotal(row); const share = percentage(amount, denominator); return <div key={row.key}><div className="mb-1.5 flex min-w-0 items-center gap-3 text-xs"><span className="w-5 shrink-0 font-mono text-muted-foreground">{String(index + 1).padStart(2, "0")}</span><span className="min-w-0 flex-1 truncate font-medium" title={row.key}>{row.key}</span><span className="shrink-0 tabular-nums">{compact.format(amount)}</span><span className="w-12 shrink-0 text-right text-muted-foreground tabular-nums">{share}</span></div><div className="ml-8 h-1.5 overflow-hidden rounded-full bg-muted"><div className="h-full rounded-full bg-primary" style={{ width: share === "—" ? "0%" : share }} /></div></div>; })}{rows.length === 0 && <div className="py-8 text-center text-sm text-muted-foreground">暂无统计数据</div>}</CardContent></Card>;
}

function Dashboard({ source, range }: { source: TokenStatsSource; range: RangeKey }) {
  const [distribution, setDistribution] = useState<DistributionKey>("projects");
  const summary = source.summary;
  const trend = useMemo(() => [...source.daily].sort((left, right) => left.key.localeCompare(right.key)).map((point) => ({ ...point, date: point.key.slice(5) })), [source.daily]);
  const distributionGroups = source[distribution];
  const denominator = tokenTotal(summary);
  const cacheRate = summary.cacheHitRate;
  const coverage = source.coverageStartAt && source.coverageEndAt ? `${formatDateTime(source.coverageStartAt)} – ${formatDateTime(source.coverageEndAt)}` : "暂无时间戳覆盖";
  if (summary.records === 0) {
    return <Card className="rounded-xl p-8 text-center shadow-none"><div className="text-sm font-medium">当前范围暂无 Token 记录</div><p className="mt-2 text-xs text-muted-foreground">{source.filesScanned > 0 ? `已扫描 ${exact.format(source.filesScanned)} 个会话文件，但没有可用于 ${rangeLabel(range)} 的 usage。` : "尚未发现该来源的本地会话日志。"}</p>{source.parseErrors > 0 && <p className="mt-2 text-xs text-amber-600">已跳过 {exact.format(source.parseErrors)} 条无法解析的记录。</p>}</Card>;
  }
  return <div className="space-y-4"><Card className="grid min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none md:grid-cols-[1.15fr_.85fr]"><div className="min-w-0 px-5 py-5"><div className="text-xs font-medium tracking-[.08em] text-muted-foreground">总 TOKEN · {rangeLabel(range)}</div><div className="mt-2 text-[42px] font-semibold leading-none tracking-[-.04em] tabular-nums" style={{ fontFamily: '"Bricolage Grotesque Variable", "SF Pro Display", ui-sans-serif, sans-serif' }}>{compact.format(denominator)}</div><p className="mt-4 text-xs leading-5 text-muted-foreground">{exact.format(source.filesScanned)} 个会话文件 · {exact.format(summary.records)} 条有效 usage · 覆盖 {coverage}</p></div><div className="flex min-w-0 flex-wrap items-center gap-5 border-t px-5 py-4 md:border-l md:border-t-0"><div className="relative size-28 shrink-0 rounded-full" style={{ background: cacheRate == null ? "var(--muted)" : `conic-gradient(var(--primary) ${cacheRate * 360}deg, var(--muted) 0)` }}><div className="absolute inset-2.5 flex flex-col items-center justify-center rounded-full bg-card"><strong className="text-2xl tabular-nums">{cacheRate == null ? "—" : `${(cacheRate * 100).toFixed(0)}%`}</strong><span className="text-[10px] text-muted-foreground">缓存命中</span></div></div><div className="min-w-0 flex-1"><div className="text-sm font-medium">上下文复用效率</div><p className="mt-1 text-xs leading-5 text-muted-foreground">{compact.format(summary.cacheRead)} Token 由缓存承担，真正新增输入 {compact.format(summary.uncachedInput)}。</p></div></div></Card><Card className="grid gap-0 rounded-xl py-0 shadow-none" aria-label="Token 总览"><Metric icon={Database} label="输入 Token" value={compact.format(summary.input)} /><Metric icon={MessagesSquare} label="输出 Token" value={compact.format(summary.output)} divided /><Metric icon={Gauge} label="缓存命中率" value={cacheRate == null ? "—" : `${(cacheRate * 100).toFixed(1)}%`} divided /></Card><Card className="gap-0 rounded-xl py-0 shadow-none"><CardHeader className="flex-row items-center justify-between px-4 pb-0 pt-4 sm:px-5"><div><CardTitle className="text-[13px]">流量趋势</CardTitle><CardDescription className="text-xs">新增输入、缓存读取与输出按日展示；折线表示调用次数。</CardDescription></div><Activity className="size-4 text-muted-foreground" aria-hidden="true" /></CardHeader><CardContent className="px-4 pb-4 pt-3 sm:px-5">{trend.length ? <ChartContainer config={chartConfig} className="h-64 w-full"><ComposedChart data={trend} margin={{ top: 8, right: 6, left: 0, bottom: 0 }}><CartesianGrid vertical={false} strokeDasharray="3 3" /><XAxis dataKey="date" tickLine={false} axisLine={false} tickMargin={8} /><YAxis yAxisId="tokens" tickLine={false} axisLine={false} width={46} tickFormatter={(value) => compact.format(Number(value))} /><YAxis yAxisId="calls" orientation="right" tickLine={false} axisLine={false} width={34} /><ChartTooltip content={<ChartTooltipContent />} /><Bar yAxisId="tokens" dataKey="cacheRead" stackId="token" fill="var(--color-cacheRead)" /><Bar yAxisId="tokens" dataKey="uncachedInput" stackId="token" fill="var(--color-uncachedInput)" /><Bar yAxisId="tokens" dataKey="output" stackId="token" fill="var(--color-output)" radius={[3, 3, 0, 0]} /><Line yAxisId="calls" dataKey="records" stroke="var(--color-records)" strokeWidth={2} dot={false} /></ComposedChart></ChartContainer> : <div className="py-16 text-center text-sm text-muted-foreground">当前范围暂无数据</div>}</CardContent></Card><div className="grid gap-4 lg:grid-cols-2"><Composition value={summary} /><Heatmap groups={source.hours} /></div><div><div className="mb-2 flex flex-wrap items-center justify-between gap-2 px-1"><h2 className="text-[13px] font-medium">用量分布</h2><div className="flex rounded-lg bg-muted p-1" role="group" aria-label="用量分布维度"><Button type="button" variant={distribution === "projects" ? "secondary" : "ghost"} size="sm" className="h-7 px-2.5 text-xs" aria-pressed={distribution === "projects"} onClick={() => setDistribution("projects")}>按项目</Button><Button type="button" variant={distribution === "models" ? "secondary" : "ghost"} size="sm" className="h-7 px-2.5 text-xs" aria-pressed={distribution === "models"} onClick={() => setDistribution("models")}>按模型</Button></div></div><Ranking title={distribution === "projects" ? "项目 Token 排行" : "模型 Token 排行"} groups={distributionGroups} denominator={denominator} /></div><Ranking title="消耗最高的会话" groups={source.sessions} denominator={denominator} />{source.parseErrors > 0 && <p className="flex items-center gap-1.5 px-1 text-xs text-amber-600"><RefreshCw className="size-3.5" aria-hidden="true" />已跳过 {exact.format(source.parseErrors)} 条无法解析的本地记录。</p>}</div>;
}

export default function TokenStatsPage() {
  const [stats, setStats] = useState<TokenStatsSource[]>([]);
  const [active, setActive] = useState<SourceKey>("workbuddy");
  const [range, setRange] = useState<RangeKey>("30d");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [reload, setReload] = useState(0);
  const days = ranges.find((item) => item.key === range)?.days;
  useEffect(() => {
    let disposed = false;
    setLoading(true); setError(undefined);
    api.getTokenStatistics(days).then((result) => { if (!disposed) setStats(result.sources); }).catch((value) => { if (!disposed) setError(String(value)); }).finally(() => { if (!disposed) setLoading(false); });
    return () => { disposed = true; };
  }, [days, reload]);
  const source = stats.find((item) => item.source === active);
  return <div className="mx-auto min-w-0 max-w-6xl space-y-5 p-5 sm:p-6"><div className="flex min-w-0 flex-wrap items-start justify-between gap-3"><div className="min-w-0"><h1 className="text-xl font-semibold tracking-tight">Token 统计</h1><p className="mt-1 text-sm text-muted-foreground">读取本地会话日志，观察上下文缓存与模型用量。</p><div className="mt-2 inline-flex max-w-full rounded-full border bg-muted/40 px-2 py-0.5 text-[11px] text-muted-foreground">{api.isDemoMode() ? "演示模式 · 示例数据" : "本地日志 · 实时扫描"}</div></div><Tabs className="min-w-0 max-w-full" value={active} onValueChange={(value) => setActive(value as SourceKey)}><TabsList className="h-auto max-w-full flex-wrap" aria-label="Token 数据来源"><TabsTrigger className="max-w-full whitespace-normal" value="workbuddy">WorkBuddy</TabsTrigger><TabsTrigger className="max-w-full whitespace-normal" value="codebuddy-cli">CodeBuddy CLI</TabsTrigger></TabsList></Tabs></div><div className="flex w-fit max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1" role="group" aria-label="统计时间范围">{ranges.map((item) => <Button key={item.key} type="button" variant={range === item.key ? "secondary" : "ghost"} size="sm" className="h-7 px-2.5 text-xs" aria-pressed={range === item.key} onClick={() => setRange(item.key)}>{item.label}</Button>)}</div>{error ? <Card className="p-6 text-sm text-destructive"><div>读取失败：{error}</div><Button type="button" variant="outline" size="sm" className="mt-3" onClick={() => setReload((value) => value + 1)}>重试</Button></Card> : loading ? <Card className="p-10 text-center text-sm text-muted-foreground">正在扫描本地会话日志…</Card> : source ? <Dashboard source={source} range={range} /> : <Card className="p-10 text-center text-sm text-muted-foreground">该来源暂无可用统计数据</Card>}</div>;
}
