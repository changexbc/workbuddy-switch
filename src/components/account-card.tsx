import { LogIn, RefreshCw, Trash2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import type { AccountMeta } from "@/lib/types";

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

interface Props {
  account: AccountMeta;
  onDelete: (a: AccountMeta) => void;
  onCheckin?: (a: AccountMeta) => void;
  onRefresh?: (a: AccountMeta) => void;
  onSwitch?: (a: AccountMeta) => void;
  /** 今日签到状态：true=已签到，false=未签到，undefined=未知/查询中 */
  todayCheckedIn?: boolean;
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
  featuresDisabled = true,
}: Props) {
  const name = account.nickname || account.email || account.uid || "未命名账号";
  const expired =
    account.expiresAt != null && typeof account.expiresAt === "number" && account.expiresAt < Date.now();

  return (
    <Card className="gap-0 py-4">
      <CardContent className="flex items-center gap-4 px-5">
        <div className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary/10 text-base font-semibold text-primary">
          {name.charAt(0).toUpperCase()}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium">{name}</span>
            {account.email && account.email !== name && (
              <span className="truncate text-sm text-muted-foreground">{account.email}</span>
            )}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <span className="font-mono">{account.uid ? `uid: ${account.uid.slice(0, 8)}…` : ""}</span>
            {account.enterpriseName && <span>{account.enterpriseName}</span>}
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-2">
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
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          <Button
            variant="outline"
            size="sm"
            disabled={featuresDisabled || !onSwitch}
            onClick={() => onSwitch?.(account)}
          >
            <LogIn />
            切换
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={featuresDisabled || !onCheckin}
            onClick={() => onCheckin?.(account)}
          >
            签到
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={featuresDisabled || !onRefresh}
            onClick={() => onRefresh?.(account)}
          >
            <RefreshCw />
            刷新
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="text-muted-foreground hover:text-destructive"
            onClick={() => onDelete(account)}
          >
            <Trash2 />
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
