import { CalendarCheck, CircleDot, Coins, Loader2, RefreshCw, Trash2 } from "lucide-react";
import type { ReactNode } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { CodeBuddyMark, WorkBuddyMark } from "@/components/product-marks";
import { cn } from "@/lib/utils";
import type { AccountMeta, CreditExpiry, CreditResource } from "@/lib/types";

const AVATAR_TONES = [
  { bg: "bg-emerald-100", fg: "text-emerald-800" },
  { bg: "bg-violet-100", fg: "text-violet-800" },
  { bg: "bg-sky-100", fg: "text-sky-800" },
  { bg: "bg-amber-100", fg: "text-amber-800" },
  { bg: "bg-rose-100", fg: "text-rose-800" },
  { bg: "bg-teal-100", fg: "text-teal-800" },
] as const;

function avatarTone(name: string) {
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) {
    hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  }
  return AVATAR_TONES[hash % AVATAR_TONES.length];
}

function formatCredits(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(value);
}

function formatCreditExpiry(ts: number | null): string {
  if (!ts) return "长期有效";
  try {
    const date = new Date(ts);
    const month = String(date.getMonth() + 1).padStart(2, "0");
    const day = String(date.getDate()).padStart(2, "0");
    return `${month}/${day} 到期`;
  } catch {
    return "长期有效";
  }
}

function expiryClass(expired: boolean, expiringSoon: boolean): string {
  if (expired) return "text-destructive";
  if (expiringSoon) return "text-orange-500";
  return "text-muted-foreground";
}

function creditResources(credit: CreditExpiry): CreditResource[] {
  return (credit.resources ?? [])
    .filter((resource) => resource.remaining > 0)
    .map((resource, index) => ({ resource, index }))
    .sort((left, right) => {
      const leftExpiry = left.resource.expireAt ?? Number.POSITIVE_INFINITY;
      const rightExpiry = right.resource.expireAt ?? Number.POSITIVE_INFINITY;
      if (leftExpiry !== rightExpiry) return leftExpiry - rightExpiry;
      return left.index - right.index;
    })
    .map(({ resource }) => resource);
}

function CreditSummary({
  credit,
  loading,
  actions,
}: {
  credit?: CreditExpiry;
  loading?: boolean;
  /** 右侧头部操作（刷新/删除），各状态均渲染 */
  actions?: ReactNode;
}) {
  if (loading) {
    return (
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="flex items-center gap-1.5 text-sm text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" />
          积分查询中…
        </div>
        {actions}
      </div>
    );
  }
  if (!credit) return <div className="flex min-w-0 items-start justify-between gap-3">{actions}</div>;

  if (!credit.ok) {
    return (
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="flex items-center gap-1.5 text-sm text-destructive" title={credit.error}>
          <Coins className="size-3.5 shrink-0" />
          积分查询失败
        </div>
        {actions}
      </div>
    );
  }

  const resources = creditResources(credit);

  return (
    <div className="min-w-0">
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-xs text-muted-foreground">总积分</div>
          <div className="mt-1 flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1">
            <span className="text-[28px] font-semibold leading-none tabular-nums tracking-tight">
              {formatCredits(credit.totalRemaining ?? 0)}
            </span>
            <span className="text-sm text-muted-foreground">积分 · {resources.length} 项</span>
          </div>
        </div>
        {actions}
      </div>

      {resources.length > 0 ? (
        <div className="mt-4 max-h-20 space-y-2.5 overflow-y-auto">
          {resources.map((resource, index) => {
            const name = resource.packageName || resource.packageCode || "积分包";
            return (
              <div
                key={`${resource.packageCode ?? "resource"}-${resource.expireAt ?? "none"}-${index}`}
                className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-baseline gap-x-4 text-[13px]"
                title={`${name} · ${formatCredits(resource.remaining)} 积分 · ${formatCreditExpiry(resource.expireAt)}`}
              >
                <span className="min-w-0 truncate text-muted-foreground">{name}</span>
                <span className="shrink-0 whitespace-nowrap tabular-nums text-foreground">
                  {formatCredits(resource.remaining)} 积分
                  <span className={cn("ml-2", expiryClass(resource.expired, resource.expiringSoon))}>
                    · {formatCreditExpiry(resource.expireAt)}
                  </span>
                </span>
              </div>
            );
          })}
        </div>
      ) : (
        <div className="mt-4 text-[13px] text-muted-foreground">暂无可用积分</div>
      )}
    </div>
  );
}

const chipClass = "rounded-md px-1.5 py-0 text-[11px] font-medium";

function CurrentChip() {
  return (
    <span className="ml-auto inline-flex items-center gap-0.5 rounded-full bg-background/70 px-1.5 py-px text-[11px] font-medium text-muted-foreground">
      <CircleDot className="size-3" />
      当前
    </span>
  );
}

const switchBtnClass = "h-9 w-full justify-start rounded-full px-3";
const switchCurrentClass =
  "disabled:opacity-100 cursor-not-allowed bg-muted text-muted-foreground shadow-none hover:bg-muted hover:text-muted-foreground";

interface Props {
  account: AccountMeta;
  onDelete: (a: AccountMeta) => void;
  onCheckin?: (a: AccountMeta) => void;
  onRefresh?: (a: AccountMeta) => void;
  onSwitch?: (a: AccountMeta) => void;
  /** 今日签到状态：true=已签到，false=未签到，undefined=未知/查询中 */
  todayCheckedIn?: boolean;
  /** 积分资源与到期状态 */
  credit?: CreditExpiry;
  /** 积分查询是否进行中 */
  creditLoading?: boolean;
  /** 当前积分排序中最需要优先使用的账号 */
  creditPriority?: boolean;
  /** WorkBuddy 当前登录账号 */
  workbuddyActive?: boolean;
  /** CodeBuddy CLI helper 是否已配置 */
  codebuddyCliConfigured?: boolean;
  /** CodeBuddy CLI 当前是否为此账号 */
  codebuddyCliActive?: boolean;
  /** 只切换 CodeBuddy CLI，不重启 WorkBuddy */
  onSwitchCodebuddyCli?: (a: AccountMeta) => void;
  codebuddyCliLoading?: boolean;
  /** 阶段 2/3 功能尚未启用时置灰 */
  featuresDisabled?: boolean;
}

export function AccountCard({
  account,
  onDelete,
  onCheckin,
  onRefresh,
  onSwitch,
  todayCheckedIn,
  credit,
  creditLoading,
  creditPriority: _creditPriority,
  workbuddyActive,
  codebuddyCliConfigured,
  codebuddyCliActive,
  onSwitchCodebuddyCli,
  codebuddyCliLoading,
  featuresDisabled = true,
}: Props) {
  const name = account.nickname || account.uid || "未命名账号";
  const expired =
    account.expiresAt != null && typeof account.expiresAt === "number" && account.expiresAt < Date.now();
  const tone = avatarTone(name);

  return (
    <article className="min-w-0 border-b last:border-b-0">
      <div className="grid min-w-0 gap-5 p-5 md:grid-cols-[236px_minmax(0,1fr)] md:gap-0 md:p-6">
        <section className="min-w-0 md:pr-6">
          <div className="flex min-w-0 items-start gap-3">
            <div
              className={cn(
                "flex size-12 shrink-0 items-center justify-center rounded-full text-base font-semibold",
                tone.bg,
                tone.fg,
              )}
            >
              {name.charAt(0).toUpperCase()}
            </div>
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium" title={name}>
                {name}
              </div>
              {(todayCheckedIn !== undefined || account.needsRelogin || expired) && (
                <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                  {todayCheckedIn !== undefined && (
                    <Badge variant="secondary" className={cn(chipClass, "text-muted-foreground")}>
                      <CalendarCheck />
                      {todayCheckedIn ? "已签到" : "未签到"}
                    </Badge>
                  )}
                  {(account.needsRelogin || expired) && (
                    <Badge variant="warning" className={chipClass}>
                      {account.needsRelogin ? "需重新登录" : "Token 已过期"}
                    </Badge>
                  )}
                </div>
              )}
            </div>
          </div>

          <div className="mt-4 flex min-w-0 flex-col gap-2">
            <Button
              variant="outline"
              size="sm"
              className={cn(switchBtnClass, workbuddyActive && switchCurrentClass)}
              disabled={featuresDisabled || !onSwitch || workbuddyActive}
              onClick={() => onSwitch?.(account)}
              title={
                workbuddyActive
                  ? "当前已是 WorkBuddy 登录账号"
                  : "切换 WorkBuddy 当前登录账号（会重启 WorkBuddy）"
              }
            >
              <WorkBuddyMark size={18} className={workbuddyActive ? "opacity-50" : undefined} />
              <span className="min-w-0 flex-1 truncate text-left">切换 WorkBuddy</span>
              {workbuddyActive && <CurrentChip />}
            </Button>
            <Button
              variant="outline"
              size="sm"
              className={cn(switchBtnClass, codebuddyCliActive && switchCurrentClass)}
              disabled={
                featuresDisabled ||
                !codebuddyCliConfigured ||
                !onSwitchCodebuddyCli ||
                codebuddyCliActive ||
                codebuddyCliLoading
              }
              onClick={() => onSwitchCodebuddyCli?.(account)}
              title={
                codebuddyCliActive
                  ? "当前已是 CodeBuddy CLI 账号"
                  : codebuddyCliConfigured
                    ? "只切换 CodeBuddy CLI 的当前账号，不重启 WorkBuddy"
                    : "请先接入 CodeBuddy CLI"
              }
            >
              {codebuddyCliLoading ? (
                <Loader2 className="animate-spin" />
              ) : (
                <CodeBuddyMark size={18} className={codebuddyCliActive ? "opacity-50" : undefined} />
              )}
              <span className="min-w-0 flex-1 truncate text-left">切换 CodeBuddy CLI</span>
              {codebuddyCliActive && <CurrentChip />}
            </Button>
            {todayCheckedIn === false && (
              <Button
                variant="ghost"
                size="sm"
                className="h-8 w-full justify-start px-2 text-xs text-muted-foreground"
                disabled={featuresDisabled || !onCheckin}
                onClick={() => onCheckin?.(account)}
              >
                <CalendarCheck />
                签到
              </Button>
            )}
          </div>
        </section>

        <section className="min-w-0 border-t pt-5 md:border-l md:border-t-0 md:pl-6 md:pt-0">
          <CreditSummary
            credit={credit}
            loading={creditLoading}
            actions={
              <div className="flex shrink-0 items-center gap-2">
                <Button
                  variant="outline"
                  size="icon"
                  className="size-9 rounded-lg text-muted-foreground hover:text-foreground"
                  disabled={featuresDisabled || !onRefresh}
                  onClick={() => onRefresh?.(account)}
                  aria-label="刷新账号"
                  title="刷新账号 token"
                >
                  <RefreshCw />
                </Button>
                <Button
                  variant="outline"
                  size="icon"
                  className="size-9 rounded-lg text-destructive hover:bg-destructive/5 hover:text-destructive"
                  onClick={() => onDelete(account)}
                  aria-label="删除账号"
                  title="删除账号"
                >
                  <Trash2 />
                </Button>
              </div>
            }
          />
        </section>
      </div>
    </article>
  );
}
