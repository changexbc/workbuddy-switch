import {
  CalendarCheck,
  Clock3,
  Coins,
  Loader2,
  LogIn,
  RefreshCw,
  Terminal,
  Trash2,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import type { AccountMeta, CreditExpiry, CreditResource } from "@/lib/types";

function formatTime(ts: number | null): string {
  if (!ts) return "—";
  try {
    return new Date(ts).toLocaleString("zh-CN", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return String(ts);
  }
}

function formatCredits(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(value);
}

function formatCreditExpiry(ts: number | null): string {
  if (!ts) return "未提供到期时间";
  const dateText = new Date(ts).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
  const days = Math.ceil((ts - Date.now()) / (24 * 3600 * 1000));
  if (days <= 0) return `已到期（${dateText}）`;
  if (days <= 7) return `${days} 天后到期（${dateText}）`;
  return `到期 ${dateText}`;
}

const creditTextClass = (expired: boolean, expiringSoon: boolean) =>
  expired ? "font-medium text-destructive" : expiringSoon ? "font-medium text-amber-700" : "text-foreground";

function CreditSummary({ credit, loading }: { credit?: CreditExpiry; loading?: boolean }) {
  if (loading) {
    return (
      <div className="mt-3 flex items-center gap-1.5 text-xs text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" />
        积分查询中…
      </div>
    );
  }
  if (!credit) return null;

  if (!credit.ok) {
    return (
      <div
        className="mt-3 flex items-center gap-1.5 text-xs text-destructive"
        title={credit.error}
      >
        <Coins className="size-3.5 shrink-0" />
        积分查询失败
      </div>
    );
  }

  const MAX_VISIBLE_RESOURCES = 3;
  const resources = (credit.resources ?? [])
    .filter((resource) => resource.remaining > 0)
    .map((resource, index) => ({ resource, index }))
    .sort((left, right) => {
      const leftExpiry = left.resource.expireAt ?? Number.POSITIVE_INFINITY;
      const rightExpiry = right.resource.expireAt ?? Number.POSITIVE_INFINITY;
      if (leftExpiry !== rightExpiry) return leftExpiry - rightExpiry;
      return left.index - right.index;
    })
    .map(({ resource }) => resource);
  const visibleResources = resources.slice(0, MAX_VISIBLE_RESOURCES);
  const hiddenResources = resources.slice(MAX_VISIBLE_RESOURCES);
  const hiddenRemaining = hiddenResources.reduce(
    (sum, resource) => sum + (resource.remaining ?? 0),
    0,
  );

  return (
    <div className="mt-3 rounded-md border bg-muted/40 px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
        <span className={creditTextClass(credit.expired ?? false, credit.expiringSoon ?? false)}>
          <Coins className="mr-1 inline size-3.5" />
          剩余 {formatCredits(credit.totalRemaining ?? 0)} 积分
        </span>
        {credit.soonestExpireAt != null && (
          <span className={creditTextClass(credit.expired ?? false, credit.expiringSoon ?? false)}>
            <Clock3 className="mr-1 inline size-3.5" />
            {formatCreditExpiry(credit.soonestExpireAt)}
          </span>
        )}
        {(credit.expiringSoonRemaining ?? 0) > 0 && (
          <span className="font-medium text-amber-700">
            其中 {formatCredits(credit.expiringSoonRemaining ?? 0)} 积分 7 天内到期
          </span>
        )}
      </div>
      {visibleResources.length > 0 ? (
        <div className="mt-1.5 flex flex-col gap-1 border-t pt-1.5 text-xs text-muted-foreground">
          {visibleResources.map((resource: CreditResource, index) => (
            <div
              key={`${resource.packageCode ?? "resource"}-${resource.expireAt ?? "none"}-${index}`}
              className={
                resource.expired
                  ? "font-medium text-destructive"
                  : resource.expiringSoon
                    ? "font-medium text-amber-700"
                    : undefined
              }
            >
              {resource.packageName || resource.packageCode || "积分包"}：{formatCredits(resource.remaining)} 积分 ·{" "}
              {formatCreditExpiry(resource.expireAt)}
            </div>
          ))}
          {hiddenResources.length > 0 && (
            <div className="text-muted-foreground">
              另有 {hiddenResources.length} 个积分包（合计 {formatCredits(hiddenRemaining)} 积分）
            </div>
          )}
        </div>
      ) : (
        <div className="mt-1.5 border-t pt-1.5 text-xs text-muted-foreground">暂无可用积分</div>
      )}
    </div>
  );
}

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
  creditPriority,
  workbuddyActive,
  codebuddyCliConfigured,
  codebuddyCliActive,
  onSwitchCodebuddyCli,
  codebuddyCliLoading,
  featuresDisabled = true,
}: Props) {
  const name = account.nickname || account.email || account.uid || "未命名账号";
  const expired =
    account.expiresAt != null && typeof account.expiresAt === "number" && account.expiresAt < Date.now();

  return (
    <Card className={creditPriority ? "border-amber-300" : undefined}>
      <CardContent className="px-5 py-4">
        {/* 身份信息 */}
        <div className="flex items-start gap-3">
          <div className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary/10 text-base font-semibold text-primary">
            {name.charAt(0).toUpperCase()}
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-baseline gap-x-2">
              <span className="truncate font-medium">{name}</span>
              {account.email && account.email !== name && (
                <span className="truncate text-sm text-muted-foreground">{account.email}</span>
              )}
            </div>
            <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
              {account.uid && <span className="font-mono">uid: {account.uid.slice(0, 8)}…</span>}
              {account.enterpriseName && <span>{account.enterpriseName}</span>}
            </div>
            <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
              <Badge variant={account.needsRelogin ? "warning" : expired ? "destructive" : "success"}>
                {account.needsRelogin
                  ? "需重新登录"
                  : expired
                    ? "Token 已过期"
                    : `Token 有效至 ${formatTime(account.expiresAt)}`}
              </Badge>
              {todayCheckedIn !== undefined && (
                <Badge variant={todayCheckedIn ? "success" : "outline"}>
                  {todayCheckedIn ? "今日已签到" : "今日未签到"}
                </Badge>
              )}
              {creditPriority && <Badge variant="warning">建议优先使用</Badge>}
              {workbuddyActive && <Badge variant="outline">WorkBuddy 当前</Badge>}
              {codebuddyCliConfigured && codebuddyCliActive && (
                <Badge variant="outline">CodeBuddy CLI 当前</Badge>
              )}
            </div>
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="-mr-1.5 -mt-1.5 shrink-0 text-muted-foreground hover:text-destructive"
            onClick={() => onDelete(account)}
          >
            <Trash2 />
          </Button>
        </div>

        {/* 积分摘要 */}
        <CreditSummary credit={credit} loading={creditLoading} />

        {/* 操作栏 */}
        <div className="mt-3 flex flex-wrap items-center gap-2 border-t pt-3">
          <Button
            variant={creditPriority && !workbuddyActive ? "default" : "outline"}
            size="sm"
            disabled={featuresDisabled || !onSwitch || workbuddyActive}
            onClick={() => onSwitch?.(account)}
            title="切换 WorkBuddy 当前登录账号（会重启 WorkBuddy）"
          >
            <LogIn />
            切换 WorkBuddy
          </Button>
          {codebuddyCliConfigured && onSwitchCodebuddyCli && (
            <Button
              variant={creditPriority && !codebuddyCliActive ? "default" : "outline"}
              size="sm"
              disabled={featuresDisabled || codebuddyCliActive || codebuddyCliLoading}
              onClick={() => onSwitchCodebuddyCli(account)}
              title="只切换 CodeBuddy CLI 的当前账号，不重启 WorkBuddy"
            >
              {codebuddyCliLoading ? <Loader2 className="animate-spin" /> : <Terminal />}
              切换 CodeBuddy CLI
            </Button>
          )}
          <div className="ml-auto flex items-center gap-1">
            {todayCheckedIn !== true && (
              <Button
                variant="ghost"
                size="sm"
                disabled={featuresDisabled || !onCheckin || todayCheckedIn !== false}
                onClick={() => onCheckin?.(account)}
              >
                <CalendarCheck />
                签到
              </Button>
            )}
            <Button
              variant="ghost"
              size="sm"
              disabled={featuresDisabled || !onRefresh}
              onClick={() => onRefresh?.(account)}
            >
              <RefreshCw />
              刷新
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}
