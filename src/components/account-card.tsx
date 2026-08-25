import { ArrowRight, Check, ChevronUp, CircleCheck, Clock3, Coins, Ellipsis, Loader2, RefreshCw, Trash2 } from "lucide-react";
import { useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { CodeBuddyMark, WorkBuddyMark } from "@/components/product-marks";
import { cn } from "@/lib/utils";
import type { AccountMeta, CreditExpiry, CreditResource } from "@/lib/types";

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

interface Props {
  account: AccountMeta;
  onDelete: (a: AccountMeta) => void;
  onCheckin?: (a: AccountMeta) => void;
  onRefresh?: (a: AccountMeta) => void;
  onSwitch?: (a: AccountMeta) => void;
  todayCheckedIn?: boolean;
  credit?: CreditExpiry;
  creditLoading?: boolean;
  creditPriority?: boolean;
  workbuddyActive?: boolean;
  codebuddyCliConfigured?: boolean;
  codebuddyCliActive?: boolean;
  onSwitchCodebuddyCli?: (a: AccountMeta) => void;
  codebuddyCliLoading?: boolean;
  featuresDisabled?: boolean;
}

function ProductCurrentState({ product }: { product: "workbuddy" | "codebuddy" }) {
  const isWorkBuddy = product === "workbuddy";
  const title = isWorkBuddy ? "WorkBuddy 当前账号" : "CodeBuddy CLI 当前账号";
  return (
    <span role="status" aria-label={title} title={title} className="inline-flex h-9 items-center gap-2 rounded-full border border-emerald-200/80 bg-emerald-50/90 px-2.5 text-emerald-700 shadow-[inset_0_1px_0_rgba(255,255,255,.8)]">
      {isWorkBuddy ? <WorkBuddyMark size={22} /> : <CodeBuddyMark size={22} />}
      <Check className="size-4" strokeWidth={2.25} />
    </span>
  );
}

export function AccountCard({ account, onDelete, onCheckin, onRefresh, onSwitch, todayCheckedIn, credit, creditLoading, creditPriority, workbuddyActive, codebuddyCliConfigured, codebuddyCliActive, onSwitchCodebuddyCli, codebuddyCliLoading, featuresDisabled = true }: Props) {
  const [resourcesExpanded, setResourcesExpanded] = useState(false);
  const name = account.nickname || account.uid || "未命名账号";
  const expired = typeof account.expiresAt === "number" && account.expiresAt < Date.now();
  const avatarClass = avatarTone(name);
  const resources = creditResources(credit);
  const visibleResources = resourcesExpanded ? resources : resources.slice(0, 2);
  const expiringAmount = credit?.ok ? credit.expiringSoonRemaining ?? 0 : 0;
  const resourceRegionId = `credit-resources-${account.id.replace(/[^a-zA-Z0-9_-]/g, "-")}`;

  return (
    <article className="flex min-w-0 flex-col overflow-hidden rounded-2xl border border-slate-200/90 bg-card shadow-[0_1px_2px_rgba(15,23,42,.025),0_10px_28px_rgba(15,23,42,.035)] transition-shadow hover:shadow-[0_2px_4px_rgba(15,23,42,.04),0_14px_34px_rgba(15,23,42,.055)]">
      <header
        className={cn(
          "relative flex min-h-[120px] items-center border-b border-slate-200/80 bg-gradient-to-br px-6 py-4",
          workbuddyActive && codebuddyCliActive
            ? "from-emerald-50/95 via-emerald-50/55 to-slate-100/85"
            : workbuddyActive
              ? "from-emerald-50/95 via-white to-teal-50/75"
              : codebuddyCliActive
                ? "from-slate-100 via-zinc-50 to-slate-200/80"
                : "from-slate-50 via-white to-zinc-100/75",
        )}
      >
        <div className="pointer-events-none absolute inset-0 overflow-hidden">
          <div
            className={cn(
              "absolute -right-10 -top-16 size-44 rounded-full blur-2xl",
              workbuddyActive ? "bg-emerald-200/30" : codebuddyCliActive ? "bg-slate-300/35" : "bg-slate-200/25",
            )}
          />
          <div className="absolute inset-0 opacity-[0.22] [background-image:radial-gradient(circle_at_1px_1px,rgba(100,116,139,.22)_1px,transparent_0)] [background-size:18px_18px]" />
          {workbuddyActive && (
            <div className={cn("absolute top-[64%] -translate-y-1/2 opacity-[0.075] saturate-50 grayscale-[10%]", codebuddyCliActive ? "right-[68px] rotate-[8deg]" : "right-5 rotate-[7deg]")}>
              <WorkBuddyMark size={92} />
            </div>
          )}
          {codebuddyCliActive && (
            <div className={cn("absolute top-[63%] -translate-y-1/2 opacity-[0.065] saturate-50 grayscale-[18%]", workbuddyActive ? "right-1 -rotate-[8deg]" : "right-5 -rotate-[7deg]")}>
              <CodeBuddyMark size={90} />
            </div>
          )}
        </div>

        <div className="absolute right-4 top-4 z-20">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="icon" className="size-9 rounded-xl border-black/5 bg-white/80 text-muted-foreground shadow-sm backdrop-blur hover:bg-white hover:text-foreground" aria-label={`管理账号 ${name}`} title="更多账号操作">
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
              <DropdownMenuSeparator />
              <DropdownMenuItem className="text-destructive focus:bg-destructive/5 focus:text-destructive" onSelect={() => onDelete(account)}>
                <Trash2 />删除账号
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        <div className={cn("relative z-10 flex w-full min-w-0 items-center gap-4", workbuddyActive || codebuddyCliActive ? "pr-[138px]" : "pr-12")}>
          <div className={cn("flex size-14 shrink-0 items-center justify-center rounded-full text-lg font-semibold ring-4 ring-white/65", avatarClass)}>{name.charAt(0).toUpperCase()}</div>
          <div className="min-w-0 flex-1">
            <h3 className="truncate text-[15px] font-semibold leading-5" title={name}>{name}</h3>
            <p className="mt-1 truncate text-xs leading-5 text-muted-foreground" title={account.email || account.uid || account.id}>{accountIdentity(account)}</p>
            <div className="mt-2 flex min-w-0 flex-wrap items-center gap-1.5">
              {todayCheckedIn !== undefined && (
                <Badge variant={todayCheckedIn ? "success" : "secondary"} className={cn(chipClass, !todayCheckedIn && "text-muted-foreground")}><CircleCheck /> {todayCheckedIn ? "已签到" : "未签到"}</Badge>
              )}
              {(account.needsRelogin || expired) && <Badge variant="warning" className={chipClass}>{account.needsRelogin ? "需重新登录" : "Token 已过期"}</Badge>}
              {creditPriority && <Badge variant="warning" className={chipClass}>建议优先</Badge>}
              {workbuddyActive && codebuddyCliActive && <Badge variant="secondary" className={cn(chipClass, "text-muted-foreground")}>2 个工具正在使用</Badge>}
            </div>
          </div>
        </div>
      </header>

      <section className="flex flex-1 flex-col px-6 pb-5 pt-5">
        {creditLoading ? (
          <div className="flex items-center gap-2 py-3 text-sm text-muted-foreground"><Loader2 className="size-4 animate-spin" />积分查询中…</div>
        ) : !credit ? (
          <div className="py-3 text-sm text-muted-foreground">等待积分数据…</div>
        ) : !credit.ok ? (
          <div className="flex items-center gap-2 py-3 text-sm text-destructive" title={credit.error}><Coins className="size-4" />积分查询失败</div>
        ) : (
          <>
            <div>
              <div className="text-xs text-muted-foreground">可用积分</div>
              <div className="mt-2 flex flex-wrap items-baseline gap-x-3 gap-y-1">
                <strong className="text-[32px] font-semibold leading-none tabular-nums tracking-[-0.025em]">{formatCredits(credit.totalRemaining ?? 0)}</strong>
                <span className="text-xs text-muted-foreground">{resources.length} 个积分包</span>
              </div>
              <div className={cn("mt-4 flex items-center gap-2 text-xs", expiringAmount > 0 ? "text-orange-600" : "text-muted-foreground")}>
                <Clock3 className="size-4 shrink-0" />
                {expiringAmount > 0 ? `${formatCredits(expiringAmount)} 积分将在 7 天内到期` : resources[0]?.expireAt ? `最近到期 ${formatCreditExpiry(resources[0].expireAt).replace(" 到期", "")}` : "当前积分长期有效"}
              </div>
            </div>

            <div className="mt-6 text-xs font-medium text-muted-foreground">近期到期</div>
            <div id={resourceRegionId} className="mt-3 space-y-2.5">
              {visibleResources.length > 0 ? visibleResources.map((resource, index) => {
                const resourceName = resource.packageName || resource.packageCode || "积分包";
                return (
                  <div key={`${resource.packageCode ?? "resource"}-${resource.expireAt ?? "none"}-${index}`} className="grid min-w-0 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-3 text-xs" title={`${resourceName} · ${formatCredits(resource.remaining)} 积分 · ${formatCreditExpiry(resource.expireAt)}`}>
                    <span className="rounded-lg bg-muted/80 px-2.5 py-1.5 font-medium tabular-nums text-foreground">{formatCredits(resource.remaining)} 积分</span>
                    <span className="truncate text-muted-foreground">{resourceName}</span>
                    <span className={cn("whitespace-nowrap tabular-nums", expiryClass(resource.expired, resource.expiringSoon))}>{formatCreditExpiry(resource.expireAt)}</span>
                  </div>
                );
              }) : <div className="py-1 text-xs text-muted-foreground">暂无可用积分</div>}
            </div>

            {resources.length > 2 && (
              <button type="button" className="mt-4 inline-flex w-fit items-center gap-1.5 text-xs font-medium text-emerald-700 transition-colors hover:text-emerald-800 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500/30" aria-expanded={resourcesExpanded} aria-controls={resourceRegionId} onClick={() => setResourcesExpanded((value) => !value)}>
                {resourcesExpanded ? "收起积分包" : "查看全部积分包"}
                {resourcesExpanded ? <ChevronUp className="size-3.5" /> : <ArrowRight className="size-3.5" />}
              </button>
            )}
          </>
        )}
      </section>

      <footer className="flex min-h-16 flex-wrap items-center gap-2.5 border-t px-6 py-3.5">
        {workbuddyActive ? <ProductCurrentState product="workbuddy" /> : (
          <Button variant="outline" size="sm" className="h-9 rounded-full px-2.5 pr-3.5" disabled={featuresDisabled || !onSwitch} onClick={() => onSwitch?.(account)} aria-label="设为 WorkBuddy 当前账号" title="设为 WorkBuddy 当前账号（会重启 WorkBuddy）">
            <WorkBuddyMark size={22} /><span>设为当前</span>
          </Button>
        )}
        {codebuddyCliActive ? <ProductCurrentState product="codebuddy" /> : (
          <Button variant="outline" size="sm" className="h-9 rounded-full px-2.5 pr-3.5" disabled={featuresDisabled || !codebuddyCliConfigured || !onSwitchCodebuddyCli || codebuddyCliLoading} onClick={() => onSwitchCodebuddyCli?.(account)} aria-label="设为 CodeBuddy CLI 当前账号" title={codebuddyCliConfigured ? "设为 CodeBuddy CLI 当前账号" : "请先接入 CodeBuddy CLI"}>
            {codebuddyCliLoading ? <Loader2 className="size-4 animate-spin" /> : <CodeBuddyMark size={22} />}<span>设为当前</span>
          </Button>
        )}
      </footer>
    </article>
  );
}
