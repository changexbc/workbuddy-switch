import { ArrowRight, CalendarCheck2, History, CalendarDays, Check, CircleCheck, Clock3, Coins, Ellipsis, Gauge, Loader2, PackageOpen, PlaneTakeoff, RefreshCw, Sparkles, Star, Trash2 } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DemoAction } from "@/components/demo-action";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { CodeBuddyCnIdeMark, CodeBuddyMark, WorkBuddyMark } from "@/components/product-marks";
import { cn } from "@/lib/utils";
import { creditResourceName } from "@/lib/credit-package-names";
import { demoModeEnabled } from "@/lib/demo-mode";
import type { AccountMeta, CreditExpiry, CreditResource, RateLimitEntry, TravelStatus } from "@/lib/types";

const AVATAR_TONES = [
  "bg-emerald-100 text-emerald-800",
  "bg-violet-100 text-violet-800",
  "bg-sky-100 text-sky-800",
  "bg-amber-100 text-amber-800",
  "bg-rose-100 text-rose-800",
  "bg-teal-100 text-teal-800",
] as const;

function avatarTone(name: string) {
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  return AVATAR_TONES[hash % AVATAR_TONES.length];
}

function formatCredits(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(value);
}

function formatCreditExpiry(ts: number | null): string {
  if (!ts) return "长期有效";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "长期有效";
  return `${String(date.getMonth() + 1).padStart(2, "0")}/${String(date.getDate()).padStart(2, "0")} 到期`;
}

function formatFullDate(ts: number | null): string {
  if (!ts) return "—";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleDateString("zh-CN", { year: "numeric", month: "2-digit", day: "2-digit" });
}

function formatCreditUpdatedAt(ts: number | undefined): string {
  if (!ts) return "—";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "—";
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function expiryClass(expired: boolean, expiringSoon: boolean): string {
  if (expired) return "text-destructive";
  if (expiringSoon) return "text-orange-600";
  return "text-muted-foreground";
}

function creditResources(credit?: CreditExpiry): CreditResource[] {
  return (credit?.resources ?? [])
    .filter((resource) => resource.remaining > 0)
    .map((resource, index) => ({ resource, index }))
    .sort((left, right) => {
      const leftExpiry = left.resource.expireAt ?? Number.POSITIVE_INFINITY;
      const rightExpiry = right.resource.expireAt ?? Number.POSITIVE_INFINITY;
      return leftExpiry === rightExpiry ? left.index - right.index : leftExpiry - rightExpiry;
    })
    .map(({ resource }) => resource);
}

function accountIdentity(account: AccountMeta): string {
  if (account.email) {
    const [local, domain] = account.email.split("@");
    if (!domain) return account.email;
    return `${local.slice(0, 1)}${"*".repeat(Math.max(3, local.length - 1))}@${domain}`;
  }
  return account.uid ? `UID · ${account.uid}` : `ID · ${account.id}`;
}

const chipClass = "rounded-md px-1.5 py-0 text-[11px] font-medium";

/**
 * 纯图标状态 chip：状态由图标 + 色调 + tooltip 共同表达，不再占文案宽度。
 * 签到、旅行与模型限额共用这一份实现（角标样式、`aria-label`、tooltip 位置统一）。
 */
function statusIconChip({
  icon,
  label,
  tooltip,
  variant,
  count,
}: {
  icon: ReactNode;
  label: string;
  tooltip: ReactNode;
  variant: "secondary" | "success" | "warning";
  /** 数量角标；≤1 时不显示（单个受限模型不需要角标）。 */
  count?: number;
}) {
  const badge = count != null && count > 1;
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Badge variant={variant} className={cn(chipClass, "px-1", badge && "gap-0.5")} aria-label={label}>
          {icon}
          {badge ? (
            <span
              aria-hidden="true"
              className="flex h-3 min-w-3 items-center justify-center rounded-full bg-amber-600 px-0.5 text-[9px] font-semibold leading-none tabular-nums text-white"
            >
              {count}
            </span>
          ) : null}
        </Badge>
      </TooltipTrigger>
      <TooltipContent side="top">{tooltip}</TooltipContent>
    </Tooltip>
  );
}

function travelIconChip({
  label,
  tooltip,
  variant,
}: {
  label: string;
  tooltip: string;
  variant: "secondary" | "success";
}) {
  return statusIconChip({ icon: <PlaneTakeoff className="size-3.5" />, label, tooltip, variant });
}

function formatTravelRemaining(arriveAt: number | null | undefined): string | null {
  if (!arriveAt || arriveAt <= 0) return null;
  const arriveMs = arriveAt > 1e12 ? arriveAt : arriveAt * 1000;
  const remainingMs = arriveMs - Date.now();
  if (remainingMs <= 0) return "即将到达";
  const totalMinutes = Math.max(1, Math.ceil(remainingMs / 60_000));
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours > 0 && minutes > 0) return `剩余 ${hours} 小时 ${minutes} 分钟`;
  if (hours > 0) return `剩余 ${hours} 小时`;
  return `剩余 ${minutes} 分钟`;
}

function travelTooltip(status: TravelStatus): string {
  const place = status.locationName?.trim();
  const credit = status.rewardCredit;
  const points = credit != null ? `+${credit}` : null;
  const remaining = formatTravelRemaining(status.arriveAt);
  if (status.label === "traveling") {
    const parts = [place, points ? `预计 ${points}` : "旅行中", remaining].filter(Boolean);
    return parts.length > 0 ? parts.join(" · ") : "旅行中";
  }
  if (status.label === "finished") {
    if (place && points) return `${place} · ${points}`;
    if (place) return `${place} · 已结束`;
    if (points) return `已结束 · ${points}`;
    return "已结束";
  }
  if (status.label === "no-buddy") return "无 Buddy";
  return "未旅行";
}

/** 按旅行状态渲染标签：无 Buddy / 未旅行 / 旅行中 / 已结束。 */
function travelChip(status: TravelStatus | undefined) {
  if (!status) return null;
  switch (status.label) {
    case "no-buddy":
      return <Badge variant="secondary" className={cn(chipClass, "text-muted-foreground")}>无 Buddy</Badge>;
    case "traveling":
      return travelIconChip({ label: travelTooltip(status), tooltip: travelTooltip(status), variant: "secondary" });
    case "finished":
      return travelIconChip({ label: travelTooltip(status), tooltip: travelTooltip(status), variant: "success" });
    case "untraveled":
    default:
      return <Badge variant="secondary" className={cn(chipClass, "text-muted-foreground")}>未旅行</Badge>;
  }
}

/** 倒计时：`2h14m 后恢复`；不足 1 分钟按「即将恢复」，已过期由调用方过滤。 */
function formatRateLimitRemaining(resetAt: number, now: number): string {
  const remainingMs = resetAt - now;
  if (remainingMs <= 0) return "即将恢复";
  const totalMinutes = Math.max(1, Math.ceil(remainingMs / 60_000));
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours > 0 && minutes > 0) return `${hours}h${minutes}m 后恢复`;
  if (hours > 0) return `${hours}h 后恢复`;
  return `${minutes}m 后恢复`;
}

/** 恢复时刻：今天 `17:59`、明天 `明天 09:00`、更远 `9/18 09:00`。 */
function formatRateLimitClock(resetAt: number, now: number): string {
  const reset = new Date(resetAt);
  const time = `${String(reset.getHours()).padStart(2, "0")}:${String(reset.getMinutes()).padStart(2, "0")}`;
  const midnight = (date: Date) => new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  const days = Math.round((midnight(reset) - midnight(new Date(now))) / 86_400_000);
  if (days <= 0) return time;
  if (days === 1) return `明天 ${time}`;
  return `${reset.getMonth() + 1}/${reset.getDate()} ${time}`;
}

/**
 * 模型限额 chip：放在旅行图标旁。
 *
 * - 该账号当前没有受限模型 → 不渲染（AC1）；
 * - 悬停按恢复时间**升序**列出全部受限模型（最早的解锁时刻最有行动价值），
 *   每行是「模型 · 倒计时（恢复时刻）」——倒计时看还剩多久，括号里的时刻看具体什么时候；
 * - 受限模型数 >1 → 图标带数量角标（AC2.1）；
 * - `resetAt` 已过本地时钟的条目每秒被过滤掉，全部过期后图标自动消失，不依赖后端刷新；
 * - 归因失败的模型显示「未知模型」，不猜测。
 */
function rateLimitChip(limits: RateLimitEntry[] | undefined, now: number) {
  const active = (limits ?? [])
    .filter((limit) => Number.isFinite(limit.resetAt) && limit.resetAt > now)
    .sort((left, right) => left.resetAt - right.resetAt);
  if (active.length === 0) return null;
  const lines = active.map(
    (limit) =>
      `${limit.model ?? "未知模型"} · ${formatRateLimitRemaining(limit.resetAt, now)}（${formatRateLimitClock(limit.resetAt, now)}）`,
  );
  return statusIconChip({
    icon: <Gauge className="size-3.5" />,
    label: `模型限额：${lines.join("；")}`,
    // 多模型时会有多行，字号比全局 TooltipContent（text-xs）再小一号。
    tooltip: (
      <span className="flex flex-col gap-0.5 text-[11px] leading-4">
        {lines.map((line) => (
          <span key={line}>{line}</span>
        ))}
      </span>
    ),
    variant: "warning",
    count: active.length,
  });
}

interface Props {
  account: AccountMeta;
  onDelete: (a: AccountMeta) => void;
  onCheckin?: (a: AccountMeta) => void;
  onRefresh?: (a: AccountMeta) => void;
  onSwitch?: (a: AccountMeta) => void;
  onCleanupSessions?: (a: AccountMeta) => void;
  todayCheckedIn?: boolean;
  /** 今日旅行状态（undefined=查询中/未知，不渲染标签） */
  travelStatus?: TravelStatus;
  /** 该账号当前受限的模型（来自本机日志台账）；空/缺失=无受限，不渲染图标。 */
  rateLimits?: RateLimitEntry[];
  credit?: CreditExpiry;
  creditLoading?: boolean;
  /** 该账号积分最近一次查询完成时间（时间戳） */
  creditUpdatedAt?: number;
  creditPriority?: boolean;
  workbuddyActive?: boolean;
  codebuddyCliConfigured?: boolean;
  codebuddyCliActive?: boolean;
  /** 任一 CodeBuddy CLI 账号切换正在进行，用于阻止并发切换。 */
  codebuddyCliBusy?: boolean;
  onSwitchCodebuddyCli?: (a: AccountMeta) => void;
  /** 当前卡片是否为正在切换的目标账号。 */
  codebuddyCliLoading?: boolean;
  /** CodeBuddy CN IDE 是否已安装（可切换）。 */
  codebuddyCnIdeAvailable?: boolean;
  codebuddyCnIdeActive?: boolean;
  codebuddyCnIdeBusy?: boolean;
  codebuddyCnIdeLoading?: boolean;
  onSwitchCodebuddyCnIde?: (a: AccountMeta) => void;
  featuresDisabled?: boolean;
  /** 紧凑模式：头部缩成一条、按钮图标化、无 footer */
  compact?: boolean;
}

function ProductCurrentState({ product, compact = false }: { product: "workbuddy" | "codebuddy" | "codebuddy-cn"; compact?: boolean }) {
  const title =
    product === "workbuddy"
      ? "WorkBuddy 当前账号"
      : product === "codebuddy-cn"
        ? "CodeBuddy IDE 当前账号"
        : "CodeBuddy CLI 当前账号";
  return (
    <span
      role="status"
      aria-label={title}
      title={title}
      className={cn(
        "inline-flex items-center gap-2 rounded-full border border-primary/25 bg-primary/10 px-2.5 text-primary shadow-[inset_0_1px_0_rgba(255,255,255,.8)]",
        compact ? "h-7 text-xs" : "h-9",
      )}
    >
      {product === "workbuddy" ? (
        <WorkBuddyMark size={compact ? 18 : 22} />
      ) : product === "codebuddy-cn" ? (
        <CodeBuddyCnIdeMark size={compact ? 18 : 22} />
      ) : (
        <CodeBuddyMark size={compact ? 18 : 22} />
      )}
      <Check className={compact ? "size-3.5" : "size-4"} strokeWidth={2.25} />
    </span>
  );
}

/** 单行积分。`resource` 缺省时渲染为**占位行**，把列表撑满固定槽位数（2 个）：
 *  结构与真实行完全相同，因此与真实行等高，**不使用任何写死的高度值**。
 *  - 传 `placeholderLabel`：图标 + 文案贴列表左缘（与真实行的徽标同起点），其余列不可见；
 *  - 不传：整行纯占位（"一条积分都没有"时用它，避免与空态文案重复）。 */
function CreditResourceRow({ resource, compact, placeholderLabel }: { resource?: CreditResource; compact: boolean; placeholderLabel?: string }) {
  const placeholder = !resource;

  if (placeholder && placeholderLabel) {
    return (
      <div className="min-w-0">
        <div className={cn("grid min-w-0 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-3", compact ? "text-[11px]" : "text-xs")}>
          {/* 放在第一列：与真实行的徽标同起点，即贴列表左缘 */}
          <span className={cn("flex items-center gap-1.5 text-muted-foreground", compact ? "py-0.5" : "py-1")}>
            <PackageOpen className="size-3.5 shrink-0" aria-hidden="true" />
            {placeholderLabel}
          </span>
          <span className="invisible truncate">{"\u00a0"}</span>
          <span className="invisible">{"\u00a0"}</span>
        </div>
        {/* 与真实行的进度条等高，但不画轨道 —— 占位不该看起来像"剩余为 0" */}
        <div className={cn("h-1", compact ? "mt-1" : "mt-1.5")} aria-hidden="true" />
      </div>
    );
  }

  const name = resource ? creditResourceName(resource, "积分包") : "\u00a0";
  const remainingText = resource ? `${formatCredits(resource.remaining)} 积分` : "\u00a0";
  const expiryText = resource ? formatCreditExpiry(resource.expireAt) : "\u00a0";
  const ratio = resource && resource.total > 0 ? Math.min(100, Math.max(0, (resource.remaining / resource.total) * 100)) : 0;
  const barTone = resource && (resource.expiringSoon || resource.expired) ? "bg-orange-500" : "bg-primary";
  const title = resource ? `${name} · 剩余 ${formatCredits(resource.remaining)} / ${formatCredits(resource.total)} · ${expiryText}` : undefined;
  return (
    <div className={cn("min-w-0", placeholder && "invisible")} aria-hidden={placeholder || undefined} title={title}>
      <div className={cn("grid min-w-0 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-3", compact ? "text-[11px]" : "text-xs")}>
        <span className={cn("rounded-lg bg-muted/80 font-medium tabular-nums text-foreground", compact ? "px-1.5 py-0.5" : "px-2 py-1")}>{remainingText}</span>
        <span className="truncate text-muted-foreground">{name}</span>
        <span className={cn("whitespace-nowrap tabular-nums", resource && expiryClass(resource.expired, resource.expiringSoon))}>{expiryText}</span>
      </div>
      <div className={cn("h-1 overflow-hidden rounded-full bg-muted", compact ? "mt-1" : "mt-1.5")} aria-hidden="true">
        <div className={cn("h-full rounded-full", barTone)} style={{ width: `${ratio}%` }} />
      </div>
    </div>
  );
}

export function AccountCard({ account, onDelete, onCheckin, onRefresh, onSwitch, onCleanupSessions, todayCheckedIn, travelStatus, rateLimits, credit, creditLoading, creditUpdatedAt, creditPriority, workbuddyActive, codebuddyCliConfigured, codebuddyCliActive, codebuddyCliBusy, onSwitchCodebuddyCli, codebuddyCliLoading, codebuddyCnIdeAvailable, codebuddyCnIdeActive, codebuddyCnIdeBusy, codebuddyCnIdeLoading, onSwitchCodebuddyCnIde, featuresDisabled = true, compact = false }: Props) {
  const [resourcesOpen, setResourcesOpen] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  /**
   * 限额图标要随官方恢复时刻自动消失（AC3），所以本地每秒走一次时钟。
   * 只在确实有受限模型时才开定时器，普通卡片不引入额外开销。
   */
  useEffect(() => {
    if (!rateLimits?.length) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [rateLimits]);
  const name = account.nickname || account.uid || "未命名账号";
  const expired = typeof account.expiresAt === "number" && account.expiresAt < Date.now();
  const avatarClass = avatarTone(name);
  const resources = creditResources(credit);
  const visibleResources = resources.slice(0, 2);
  const expiringAmount = credit?.ok ? credit.expiringSoonRemaining ?? 0 : 0;
  /** 弹窗内展示还有剩余的资源包（已用完的隐藏），按到期时间升序 */
  const allResources = (credit?.resources ?? [])
    .filter((resource) => resource.remaining > 0)
    .map((resource, index) => ({ resource, index }))
    .sort((left, right) => {
      const leftExpiry = left.resource.expireAt ?? Number.POSITIVE_INFINITY;
      const rightExpiry = right.resource.expireAt ?? Number.POSITIVE_INFINITY;
      return leftExpiry === rightExpiry ? left.index - right.index : leftExpiry - rightExpiry;
    })
    .map(({ resource }) => resource);

  const activeProductCount = [workbuddyActive, codebuddyCliActive, codebuddyCnIdeActive].filter(Boolean).length;

  const statusChips = (
    <>
      {todayCheckedIn !== undefined &&
        statusIconChip({
          icon: todayCheckedIn ? (
            <CalendarCheck2 className="size-3.5" />
          ) : (
            <CalendarDays className="size-3.5" />
          ),
          label: todayCheckedIn ? "今日已签到" : "今日未签到",
          tooltip: todayCheckedIn ? "今日已签到" : "今日未签到",
          variant: todayCheckedIn ? "success" : "secondary",
        })}
      {travelChip(travelStatus)}
      {rateLimitChip(rateLimits, now)}
      {(account.needsRelogin || expired) && <Badge variant="warning" className={chipClass}>{account.needsRelogin ? "需重新登录" : "Token 已过期"}</Badge>}
      {creditPriority && (
        <Tooltip>
          <TooltipTrigger asChild>
            <Badge variant="warning" className={cn(chipClass, "px-1")} aria-label="建议优先">
              <Star className="size-3.5" />
            </Badge>
          </TooltipTrigger>
          <TooltipContent side="top">建议优先使用</TooltipContent>
        </Tooltip>
      )}
      {!compact && activeProductCount >= 2 && <Badge variant="secondary" className={cn(chipClass, "text-muted-foreground")}>{activeProductCount} 个工具正在使用</Badge>}
    </>
  );

  return (
    <TooltipProvider>
      <article className="flex min-w-0 flex-col overflow-hidden rounded-2xl border border-border bg-card shadow-[0_1px_2px_rgba(15,23,42,.025),0_10px_28px_rgba(15,23,42,.035)] transition-shadow hover:shadow-[0_2px_4px_rgba(15,23,42,.04),0_14px_34px_rgba(15,23,42,.055)]">
      <header
        className={cn(
          "relative flex items-center border-b border-border",
          compact ? "min-h-[52px] px-3.5 py-1.5" : "min-h-[104px] px-5 py-3",
          /* 选中态染色，优先级：WorkBuddy（品牌绿）> CodeBuddy IDE（淡紫）> CodeBuddy CLI（中性灰）> 默认。
             多个产品同时选中时取优先级最高者；具体哪几个产品在使用由 header 的标记+勾选角标表达。
             CodeBuddy IDE 的紫是产品专属色：主题里没有对应语义 token，故用 Tailwind 的 violet-500
             （本文件 AVATAR_TONES 已在用同一调色板），透明度与 WorkBuddy 的 /5、/15 保持同一强度。 */
          workbuddyActive ? "bg-primary/5" : codebuddyCnIdeActive ? "bg-violet-500/5" : codebuddyCliActive ? "bg-muted/60" : "bg-muted/30",
        )}
      >
        <div className="pointer-events-none absolute inset-0 overflow-hidden">
          <div
            className={cn(
              "absolute -right-10 -top-16 rounded-full blur-2xl",
              compact ? "size-20" : "size-24",
              workbuddyActive ? "bg-primary/15" : codebuddyCnIdeActive ? "bg-violet-500/15" : codebuddyCliActive ? "bg-muted/50" : "bg-muted/30",
            )}
          />
          {workbuddyActive && (
            <div className={cn("absolute top-[64%] -translate-y-1/2 opacity-[0.075] saturate-50 grayscale-[10%]", codebuddyCliActive ? "right-[68px] rotate-[8deg]" : "right-5 rotate-[7deg]")}>
              <WorkBuddyMark size={compact ? 40 : 56} />
            </div>
          )}
          {codebuddyCliActive && (
            <div className={cn("absolute top-[63%] -translate-y-1/2 opacity-[0.065] saturate-50 grayscale-[18%]", workbuddyActive ? "right-1 -rotate-[8deg]" : "right-5 -rotate-[7deg]")}>
              <CodeBuddyMark size={compact ? 38 : 54} />
            </div>
          )}
          {codebuddyCnIdeActive && (
            /* 复用 WorkBuddy 的 SVG：两个产品的标记同形；CodeBuddyCnIdeMark 是位图 app 图标，
               放大到水印尺寸会是一块模糊方块。位置与旋转与 WorkBuddy 水印相同 —— 两者同时选中时
               完全重合，因此无需再引入第三套偏移规则。 */
            <div className={cn("absolute top-[64%] -translate-y-1/2 opacity-[0.075] saturate-50 grayscale-[10%]", codebuddyCliActive ? "right-[68px] rotate-[8deg]" : "right-5 rotate-[7deg]")}>
              <WorkBuddyMark size={compact ? 40 : 56} />
            </div>
          )}
        </div>

        <div className={cn("absolute z-20", compact ? "right-2.5 top-1/2 -translate-y-1/2" : "right-3.5 top-3.5")}>
          {demoModeEnabled ? (
            <DemoAction>
              <Button variant="ghost" size="icon" className={cn("rounded-lg text-muted-foreground hover:text-foreground", compact ? "size-7" : "size-8")} aria-label={`管理账号 ${name}`} title="更多账号操作">
                <Ellipsis />
              </Button>
            </DemoAction>
          ) : (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className={cn("rounded-lg text-muted-foreground hover:text-foreground", compact ? "size-7" : "size-8")} aria-label={`管理账号 ${name}`} title="更多账号操作">
                  <Ellipsis />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-40">
                <DropdownMenuItem disabled={featuresDisabled || !onRefresh} onSelect={() => onRefresh?.(account)}>
                  <RefreshCw />刷新 Token
                </DropdownMenuItem>
                {todayCheckedIn === false && (
                  <DropdownMenuItem disabled={featuresDisabled || !onCheckin} onSelect={() => onCheckin?.(account)}>
                    <CircleCheck />手动签到
                  </DropdownMenuItem>
                )}
                <DropdownMenuItem disabled={featuresDisabled || !onCleanupSessions} onSelect={() => onCleanupSessions?.(account)}>
                  <History />清理旧会话
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem className="text-destructive focus:bg-destructive/5 focus:text-destructive" onSelect={() => onDelete(account)}>
                  <Trash2 />删除账号
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>

        {compact ? (
          <div className="relative z-10 flex w-full min-w-0 items-center gap-2 pr-10">
            <h3 className="min-w-0 flex-1 truncate text-[13px] font-semibold leading-5" title={name}>{name}</h3>
            <div className="hidden shrink-0 items-center gap-1 min-[420px]:flex">{statusChips}</div>
            <div className="ml-auto flex shrink-0 items-center gap-1">
              {workbuddyActive ? (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="relative inline-flex size-7 items-center justify-center rounded-lg border border-primary/25 bg-primary/10 text-primary">
                      <WorkBuddyMark size={15} />
                      <span className="absolute -right-1 -top-1 flex size-3.5 items-center justify-center rounded-full bg-primary text-primary-foreground">
                        <Check className="size-2.5" strokeWidth={3} />
                      </span>
                    </span>
                  </TooltipTrigger>
                  <TooltipContent side="top">WorkBuddy 当前账号</TooltipContent>
                </Tooltip>
              ) : demoModeEnabled ? (
                <DemoAction>
                  <Button variant="outline" size="icon" className="size-7 rounded-lg" aria-label="设为 WorkBuddy 当前账号">
                    <WorkBuddyMark size={15} />
                  </Button>
                </DemoAction>
              ) : (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button variant="outline" size="icon" className="size-7 rounded-lg" disabled={featuresDisabled || !onSwitch} onClick={() => onSwitch?.(account)} aria-label="设为 WorkBuddy 当前账号">
                      <WorkBuddyMark size={15} />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="top">设为 WorkBuddy 当前账号（会重启 WorkBuddy）</TooltipContent>
                </Tooltip>
              )}
              {codebuddyCnIdeActive ? (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="relative inline-flex size-7 items-center justify-center rounded-lg border border-primary/25 bg-primary/10 text-primary">
                      <CodeBuddyCnIdeMark size={15} />
                      <span className="absolute -right-1 -top-1 flex size-3.5 items-center justify-center rounded-full bg-primary text-primary-foreground">
                        <Check className="size-2.5" strokeWidth={3} />
                      </span>
                    </span>
                  </TooltipTrigger>
                  <TooltipContent side="top">CodeBuddy IDE 当前账号</TooltipContent>
                </Tooltip>
              ) : (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button variant="outline" size="icon" className="relative size-7 rounded-lg" disabled={featuresDisabled || !codebuddyCnIdeAvailable || !onSwitchCodebuddyCnIde || codebuddyCnIdeBusy} onClick={() => onSwitchCodebuddyCnIde?.(account)} aria-label={codebuddyCnIdeLoading ? "正在切换 CodeBuddy IDE" : "切换到 CodeBuddy IDE"} aria-busy={codebuddyCnIdeLoading}>
                      {codebuddyCnIdeLoading ? <Loader2 className="size-3.5 animate-spin" /> : <CodeBuddyCnIdeMark size={15} />}
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="top">{codebuddyCnIdeAvailable ? "切换到 CodeBuddy IDE（会重启 IDE）" : "未检测到 CodeBuddy IDE"}</TooltipContent>
                </Tooltip>
              )}
              {codebuddyCliActive ? (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="relative inline-flex size-7 items-center justify-center rounded-lg border border-primary/25 bg-primary/10 text-primary">
                      <CodeBuddyMark size={15} />
                      <span className="absolute -right-1 -top-1 flex size-3.5 items-center justify-center rounded-full bg-primary text-primary-foreground">
                        <Check className="size-2.5" strokeWidth={3} />
                      </span>
                    </span>
                  </TooltipTrigger>
                  <TooltipContent side="top">CodeBuddy CLI 当前账号</TooltipContent>
                </Tooltip>
              ) : (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button variant="outline" size="icon" className="size-7 rounded-lg" disabled={featuresDisabled || !codebuddyCliConfigured || !onSwitchCodebuddyCli || codebuddyCliBusy} onClick={() => onSwitchCodebuddyCli?.(account)} aria-label={codebuddyCliLoading ? "正在切换 CodeBuddy CLI 当前账号" : "设为 CodeBuddy CLI 当前账号"} aria-busy={codebuddyCliLoading}>
                      {codebuddyCliLoading ? <Loader2 className="size-3.5 animate-spin" /> : <CodeBuddyMark size={15} />}
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="top">{codebuddyCliConfigured ? "设为 CodeBuddy CLI 当前账号" : "请先接入 CodeBuddy CLI"}</TooltipContent>
                </Tooltip>
              )}
            </div>
          </div>
        ) : (
          <div className={cn("relative z-10 flex w-full min-w-0 items-center gap-3", workbuddyActive || codebuddyCliActive ? "pr-[112px]" : "pr-10")}>
            <div className={cn("flex size-12 shrink-0 items-center justify-center rounded-full text-base font-semibold ring-4 ring-white/65", avatarClass)}>{name.charAt(0).toUpperCase()}</div>
            <div className="min-w-0 flex-1">
              <h3 className="truncate text-sm font-semibold leading-5" title={name}>{name}</h3>
              <p className="mt-0.5 truncate text-xs leading-5 text-muted-foreground" title={account.email || account.uid || account.id}>{accountIdentity(account)}</p>
              <div className="mt-1.5 flex min-w-0 flex-wrap items-center gap-1.5">{statusChips}</div>
            </div>
          </div>
        )}
      </header>

      {/* 下内边距比上内边距大一档（紧凑 16 vs 12、宽松 20 vs 16）：顶部那条是卡内部分隔线，
          底部是卡片外缘 —— 同值留白在边缘处看起来更紧，需要补偿才与顶部视觉一致。 */}
      <section className={cn("flex min-w-0 flex-1 flex-col", compact ? "px-3.5 pb-4 pt-3" : "px-5 pb-5 pt-4")}>
        {creditLoading ? (
          <div className="flex items-center gap-2 py-3 text-sm text-muted-foreground"><Loader2 className="size-4 animate-spin" />积分查询中…</div>
        ) : !credit ? (
          <div className="py-3 text-sm text-muted-foreground">等待积分数据…</div>
        ) : !credit.ok ? (
          <div className="flex min-w-0 items-center gap-2 py-3 text-sm text-destructive" title={credit.error}>
            <Coins className="size-4 shrink-0" />
            <span className="min-w-0 truncate">{credit.error || "积分查询失败"}</span>
          </div>
        ) : (
          <>
            <div className="flex items-baseline gap-x-3 gap-y-1">
              <span className="flex items-center gap-1.5">
                <Sparkles className="size-4 shrink-0 stroke-[1.75] text-muted-foreground" aria-hidden="true" />
                <strong className={cn("font-semibold leading-none tabular-nums tracking-[-0.025em]", compact ? "text-[20px]" : "text-[22px]")} style={{ fontFamily: '"Bricolage Grotesque Variable", "SF Pro Display", ui-sans-serif, sans-serif' }}>{formatCredits(credit.totalRemaining ?? 0)}</strong>
              </span>
              <span className={cn("text-muted-foreground", compact ? "text-[11px]" : "text-xs")}>{resources.length} 个积分包</span>
              <div className={cn("ml-auto flex items-center gap-1.5 text-muted-foreground", compact ? "text-[11px]" : "text-xs")} title={expiringAmount > 0 ? `${formatCredits(expiringAmount)} 积分将在 7 天内到期` : resources[0]?.expireAt ? `最近到期 ${formatCreditExpiry(resources[0].expireAt).replace(" 到期", "")}` : "当前积分长期有效"}>
                <Clock3 className="size-3.5 shrink-0" />
                <span className="whitespace-nowrap tabular-nums">{creditUpdatedAt ? `${formatCreditUpdatedAt(creditUpdatedAt)} 更新` : "—"}</span>
              </div>
            </div>

            <div className={cn("flex items-center justify-between gap-2", compact ? "mt-3" : "mt-4")}>
              <span className="text-[11px] font-medium text-muted-foreground">近期到期</span>
              {resources.length > 2 && (
                <button
                  type="button"
                  className="inline-flex shrink-0 items-center gap-1 text-[11px] font-medium text-primary transition-colors hover:text-primary/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
                  onClick={() => setResourcesOpen(true)}
                >
                  查看全部积分包
                  <ArrowRight className="size-3.5" />
                </button>
              )}
            </div>
            {/* 上下间距统一：列表上方与内容区底部 padding 同值（紧凑 12px / 宽松 16px）。
                否则「标签→首条」比「末条→卡片底」窄，垂直节奏不对称。 */}
            <div className={cn(compact ? "mt-3 space-y-2" : "mt-4 space-y-2.5")}>
              {[0, 1].map((index) => {
                const resource = visibleResources[index];
                // 一条积分都没有时，第一槽位显示空态提示，第二槽位仍是纯占位（不再重复"暂无其他积分包"）
                if (!resource && index === 0 && visibleResources.length === 0) {
                  return <div key="credit-empty" className="py-1 text-[11px] text-muted-foreground">暂无可用积分</div>;
                }
                return (
                  <CreditResourceRow
                    key={resource ? `${resource.packageCode ?? "resource"}-${resource.expireAt ?? "none"}-${index}` : `credit-slot-${index}`}
                    resource={resource}
                    compact={compact}
                    placeholderLabel={visibleResources.length > 0 ? "暂无其他积分包" : undefined}
                  />
                );
              })}
            </div>
          </>
        )}
      </section>

      {!compact && (
        <footer className="flex flex-wrap items-center gap-2.5 border-t px-5 py-2.5">
          {workbuddyActive ? <ProductCurrentState product="workbuddy" compact /> : demoModeEnabled ? (
            <DemoAction>
              <Button variant="outline" size="sm" className="h-7 rounded-full px-2.5 pr-3.5 text-xs" aria-label="设为 WorkBuddy 当前账号">
                <WorkBuddyMark size={18} /><span>设为当前</span>
              </Button>
            </DemoAction>
          ) : (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="outline" size="sm" className="h-7 rounded-full px-2.5 pr-3.5 text-xs" disabled={featuresDisabled || !onSwitch} onClick={() => onSwitch?.(account)} aria-label="设为 WorkBuddy 当前账号">
                  <WorkBuddyMark size={18} /><span>设为当前</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent side="top">设为 WorkBuddy 当前账号（会重启 WorkBuddy）</TooltipContent>
            </Tooltip>
          )}
          {codebuddyCnIdeActive ? <ProductCurrentState product="codebuddy-cn" compact /> : (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="outline" size="sm" className="h-7 rounded-full px-2.5 pr-3.5 text-xs" disabled={featuresDisabled || !codebuddyCnIdeAvailable || !onSwitchCodebuddyCnIde || codebuddyCnIdeBusy} onClick={() => onSwitchCodebuddyCnIde?.(account)} aria-label={codebuddyCnIdeLoading ? "正在切换 CodeBuddy IDE" : "切换到 CodeBuddy IDE"} aria-busy={codebuddyCnIdeLoading}>
                  {codebuddyCnIdeLoading ? <Loader2 className="size-4 animate-spin" /> : <CodeBuddyCnIdeMark size={18} />}<span>{codebuddyCnIdeLoading ? "切换中…" : "IDE"}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent side="top">{codebuddyCnIdeAvailable ? "切换到 CodeBuddy IDE（会重启 IDE）" : "未检测到 CodeBuddy IDE"}</TooltipContent>
            </Tooltip>
          )}
          {codebuddyCliActive ? <ProductCurrentState product="codebuddy" compact /> : (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="outline" size="sm" className="h-7 rounded-full px-2.5 pr-3.5 text-xs" disabled={featuresDisabled || !codebuddyCliConfigured || !onSwitchCodebuddyCli || codebuddyCliBusy} onClick={() => onSwitchCodebuddyCli?.(account)} aria-label={codebuddyCliLoading ? "正在切换 CodeBuddy CLI 当前账号" : "设为 CodeBuddy CLI 当前账号"} aria-busy={codebuddyCliLoading}>
                  {codebuddyCliLoading ? <Loader2 className="size-4 animate-spin" /> : <CodeBuddyMark size={18} />}<span>{codebuddyCliLoading ? "切换中…" : "CLI 当前"}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent side="top">{codebuddyCliConfigured ? "设为 CodeBuddy CLI 当前账号" : "请先接入 CodeBuddy CLI"}</TooltipContent>
            </Tooltip>
          )}
        </footer>
      )}
      </article>

      <Dialog open={resourcesOpen} onOpenChange={setResourcesOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>全部积分包</DialogTitle>
            <DialogDescription>{name} · 共 {allResources.length} 个积分包</DialogDescription>
          </DialogHeader>
          {allResources.length === 0 ? (
            <div className="px-1 py-6 text-center text-sm text-muted-foreground">当前没有可展示的资源包。</div>
          ) : (
            <div className="max-h-[60vh] min-w-0 overflow-y-auto divide-y divide-border/60">
              {allResources.map((resource, index) => {
                const ratio = resource.total > 0 ? Math.min(100, Math.max(0, (resource.remaining / resource.total) * 100)) : 0;
                return (
                  <div key={`${resource.packageCode || resource.packageName || "resource"}-${index}`} className="min-w-0 py-3 first:pt-0 last:pb-0">
                    <div className="flex min-w-0 items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="truncate text-sm font-medium">{creditResourceName(resource, "未命名资源包")}</div>
                        <div className="mt-1 text-[11px] text-muted-foreground">
                          {resource.expired ? "已到期" : resource.expireAt ? `到期 ${formatFullDate(resource.expireAt)}` : "长期有效"}
                        </div>
                      </div>
                      <div className="shrink-0 text-right text-xs">
                        <div className="font-medium">{formatCredits(resource.remaining)} / {formatCredits(resource.total)}</div>
                        <div className="mt-1 text-[11px] text-muted-foreground">已用 {formatCredits(resource.used)}</div>
                      </div>
                    </div>
                    <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
                      <div className={cn("h-full rounded-full", resource.expired ? "bg-destructive/60" : resource.expiringSoon ? "bg-orange-500/80" : "bg-primary/75")} style={{ width: `${ratio}%` }} />
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </DialogContent>
      </Dialog>
    </TooltipProvider>
  );
}
