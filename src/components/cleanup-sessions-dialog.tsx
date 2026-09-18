import { useState } from "react";
import { Loader2, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import * as api from "@/lib/api";
import type { AccountMeta } from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  account: AccountMeta | null;
  /** 清理完成后回调，用于刷新会话相关数据。 */
  onCleaned?: () => void;
}

/** 从宽松报告里取数字，取不到按 0 处理。 */
function numOf(report: api.CleanupReport | null, path: string[]): number {
  let cur: unknown = report;
  for (const key of path) {
    if (typeof cur !== "object" || cur === null) return 0;
    cur = (cur as Record<string, unknown>)[key];
  }
  return typeof cur === "number" ? cur : 0;
}

function accountLabel(a: AccountMeta): string {
  return a.nickname || a.email || a.uid || a.id;
}

/** 会话数描述。 */
function countLine(report: api.CleanupReport | null): string {
  const planned = numOf(report, ["planned"]);
  const deleted = numOf(report, ["deleted"]);
  const cloudRemoved = numOf(report, ["cloud", "removed"]);
  const localPart = numOf(report, ["dryRun"]) ? `本地将清理 ${planned} 条` : `本地已清理 ${deleted} 条`;
  const cloudPart = numOf(report, ["cloud", "tokenReady"])
    ? numOf(report, ["dryRun"])
      ? `，云端将删除 ${cloudRemoved} 条`
      : `，云端已删除 ${cloudRemoved} 条`
    : "";
  return `${localPart}${cloudPart}。`;
}

/**
 * 清理旧会话对话框：先预览（dry_run）再执行，两步走，避免手一抖删多了。
 */
export function CleanupSessionsDialog({ open, onOpenChange, account, onCleaned }: Props) {
  const [keep, setKeep] = useState("3");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [preview, setPreview] = useState<api.CleanupReport | null>(null);
  const [done, setDone] = useState<api.CleanupReport | null>(null);

  const reset = () => {
    setKeep("3");
    setBusy(false);
    setError("");
    setPreview(null);
    setDone(null);
  };

  const run = async (dryRun: boolean) => {
    if (!account) return;
    const n = Math.max(0, Number.parseInt(keep, 10) || 0);
    setBusy(true);
    setError("");
    try {
      const report = await api.cleanupSessions({ accountId: account.id, keep: n, dryRun });
      if (dryRun) {
        setPreview(report);
      } else {
        setDone(report);
        onCleaned?.();
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) reset();
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>清理旧会话</DialogTitle>
          <DialogDescription>
            {account
              ? `帮 ${accountLabel(account)} 整理会话：每个项目只保留最新的几条，其余的连同云端副本一起清掉。先预览看看范围，满意了再动手。`
              : ""}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <label className="text-sm font-medium" htmlFor="cleanup-keep">
            每个项目保留
          </label>
          <div className="flex items-center gap-2">
            <Input
              id="cleanup-keep"
              type="number"
              min={1}
              value={keep}
              onChange={(e) => setKeep(e.target.value)}
              className="w-24"
              disabled={busy}
            />
            <span className="text-sm text-muted-foreground">条会话（至少 1 条）</span>
          </div>

          {preview && (
            <div className="rounded-md bg-muted px-3 py-2 text-sm">
              预览结果：{countLine(preview)}
            </div>
          )}
          {done && (
            <div className="rounded-md bg-muted px-3 py-2 text-sm">
              搞定：{countLine(done)}删掉的会话已备份，出问题可以从备份目录找回。
            </div>
          )}
          {error && <div className="text-sm text-destructive">{error}</div>}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => run(true)} disabled={busy || !!done}>
            {busy && !done ? <Loader2 className="size-4 animate-spin" /> : null}
            预览
          </Button>
          <Button
            variant="destructive"
            onClick={() => run(false)}
            disabled={busy || !preview || !!done}
          >
            {busy ? <Loader2 className="size-4 animate-spin" /> : <Trash2 className="size-4" />}
            确认清理
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
