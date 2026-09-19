import { useEffect, useState } from "react";
import { toast } from "sonner";
import { History, Loader2, UserRoundPlus } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import * as api from "@/lib/api";
import type { DiscoveredAccount } from "@/lib/types";

function fmtTs(ts: number | null): string {
  if (!ts) return "";
  try {
    return new Date(ts).toLocaleString("zh-CN", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return "";
  }
}

/**
 * 「发现曾登录账号」提示条：扫描本机 auth 历史备份 + 数据残留，找出
 * 已登录过但不在账号库的账号，支持一键补录。无候选时整块不渲染。
 */
export function DiscoverAccountsBanner({
  onAdopted,
}: {
  onAdopted?: () => void;
}) {
  const [discovered, setDiscovered] = useState<DiscoveredAccount[] | null>(null);
  const [adopting, setAdopting] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .discoverKnownAccounts()
      .then(({ accounts }) => {
        if (!cancelled) setDiscovered(accounts);
      })
      .catch(() => {
        // 老版本 server 无该接口时静默隐藏，不打扰
        if (!cancelled) setDiscovered([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // 只展示「不在账号库」的账号；在册的已可见，无需提示
  const candidates = (discovered ?? []).filter((account) => !account.inAccountList);
  if (!discovered || candidates.length === 0) return null;

  async function onAdopt(account: DiscoveredAccount) {
    if (adopting !== null) return;
    setAdopting(account.uid);
    try {
      const res = await api.adoptAccount(account.uid);
      toast.success("已加回列表", {
        description: `${account.nickname || account.email || account.uid.slice(0, 8)}：${res.account.nickname || res.account.uid}`,
      });
      onAdopted?.();
    } catch (error) {
      toast.error("这次没加进去", {
        description: `登录信息可能已经过期了，稍后可以再试一次。${api.asError(error)}`,
      });
    } finally {
      setAdopting(null);
    }
  }

  return (
    <Alert className="mb-4 border-dashed bg-amber-500/[0.04]">
      <History className="mt-0.5 h-4 w-4" />
      <AlertTitle className="flex items-center gap-2">
        发现 {candidates.length} 个以前登录过的账号
        <span className="text-xs font-normal text-muted-foreground">
          本机还留着它们的记录，加回列表就能直接管理和切换
        </span>
      </AlertTitle>
      <AlertDescription>
        <ul className="mt-2 space-y-1.5">
          {candidates.map((account) => (
            <li key={account.uid} className="flex flex-wrap items-center gap-2.5">
              <span className="min-w-0 text-sm">
                {account.nickname || account.email || account.uid.slice(0, 8)}
                <span className="ml-2 text-xs tabular-nums text-muted-foreground">
                  {account.uid.slice(0, 8)}
                </span>
              </span>
              {account.backedUpAt ? (
                <Badge variant="secondary" className="text-[11px] font-normal text-muted-foreground">
                  上次登录 {fmtTs(account.backedUpAt)}
                </Badge>
              ) : (
                <Badge variant="secondary" className="text-[11px] font-normal text-muted-foreground">
                  只找到本地数据
                </Badge>
              )}
              {account.restorable ? (
                <Button
                  variant="outline"
                  size="sm"
                  className="h-7 gap-1 px-2.5 text-xs"
                  onClick={() => void onAdopt(account)}
                  disabled={adopting !== null}
                >
                  {adopting === account.uid ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : (
                    <UserRoundPlus className="size-3.5" />
                  )}
                  加回列表
                </Button>
              ) : (
                <span className="text-xs text-muted-foreground">
                  登录信息已过期，重新登录后可用「导入本机账号」
                </span>
              )}
            </li>
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  );
}
