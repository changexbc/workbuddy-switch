import { useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, Loader2 } from "lucide-react";
import { toast } from "sonner";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import * as api from "@/lib/api";
import type { AccountMeta, VscodeExtStatus, VscodeSession, VscodeSessionRef } from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 目标账号 */
  account: AccountMeta | null;
  /** VS Code 扩展状态（用于渲染空态与运行中提示）。 */
  vscodeExtStatus?: VscodeExtStatus | null;
  /** 切换完成后刷新列表 */
  onDone?: () => void;
}

/** VS Code 扩展会话切换弹窗：可勾选「当前扩展账号」的会话复制到目标账号。 */
export function VscodeSwitchAccountDialog({ open, onOpenChange, account, vscodeExtStatus, onDone }: Props) {
  const [sessions, setSessions] = useState<VscodeSession[]>([]);
  const [sourceUid, setSourceUid] = useState<string | null>(null);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [copyEnabled, setCopyEnabled] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  /** 展开的工作区分组（默认全部展开，会话较少）。 */
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // 打开时加载「当前扩展账号」可复制的会话。
  useEffect(() => {
    if (!open || !account) return;
    setCopyEnabled(false);
    setSelected(new Set());
    setCollapsed(new Set());
    setError("");
    setLoadingSessions(true);
    api
      .listVscodeSessions()
      .then((res) => {
        setSessions(res.sessions);
        setSourceUid(res.sourceUid);
      })
      .catch(() => {
        setSessions([]);
        setSourceUid(null);
      })
      .finally(() => setLoadingSessions(false));
  }, [open, account]);

  const groups = useMemo(() => buildGroups(sessions), [sessions]);
  const sessionById = useMemo(() => {
    const map = new Map<string, VscodeSession>();
    for (const session of sessions) map.set(session.id, session);
    return map;
  }, [sessions]);

  function toggleSession(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleGroup(ids: string[]) {
    setSelected((prev) => {
      const next = new Set(prev);
      const allOn = ids.length > 0 && ids.every((id) => next.has(id));
      if (allOn) ids.forEach((id) => next.delete(id));
      else ids.forEach((id) => next.add(id));
      return next;
    });
  }

  function toggleCollapsed(key: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  async function doSwitch() {
    if (!account) return;
    setBusy(true);
    setError("");
    try {
      const refs: VscodeSessionRef[] | undefined = copyEnabled
        ? [...selected]
            .map((id) => {
              const session = sessionById.get(id);
              return session
                ? { workspaceHash: session.workspaceHash, conversationId: session.id }
                : null;
            })
            .filter((ref): ref is VscodeSessionRef => ref !== null)
        : undefined;

      const res = await api.switchVscodeExtAccount(account.id, false, refs);
      const nickname = account.nickname || account.email || account.uid || "该账号";
      const copied = res.sessionCopy?.copied.length ?? 0;
      const errors = res.sessionCopy?.errors ?? [];

      if (errors.length > 0) {
        toast.warning(`已切换至「${nickname}」，但部分会话未复制`, {
          description: [
            `已复制 ${copied} 个会话`,
            `${errors.length} 个失败：${errors.map((e) => e.error).join("；")}`,
            "请重载 VS Code 窗口生效",
          ].join("；"),
        });
      } else {
        toast.success(`已切换至「${nickname}」`, {
          description: [
            copied > 0 ? `已复制 ${copied} 个会话` : null,
            res.message || "请重载 VS Code 窗口生效",
          ]
            .filter(Boolean)
            .join("；"),
        });
      }
      onOpenChange(false);
      onDone?.();
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
    }
  }

  const copyCount = copyEnabled ? selected.size : 0;
  const hasCopyable = groups.length > 0;
  const running = vscodeExtStatus?.running === true;
  const emptyHint = emptyStateHint(vscodeExtStatus, loadingSessions, sourceUid, hasCopyable);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        showCloseButton={!busy}
        className="flex max-h-[min(90vh,calc(100vh-2rem))] min-w-0 flex-col overflow-hidden"
      >
        <DialogHeader className="shrink-0">
          <DialogTitle>切换到「{account?.nickname || account?.email || account?.uid || "该账号"}」</DialogTitle>
          <DialogDescription>
            将把所选账号写入 VS Code CodeBuddy 扩展；可选把当前账号的会话一并复制过去。
          </DialogDescription>
        </DialogHeader>

        {busy && (
          <div className="absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg bg-background/85 backdrop-blur-sm">
            <Loader2 className="size-8 animate-spin text-primary" />
            <p className="text-sm font-medium">正在切换并复制会话…</p>
            <p className="max-w-xs text-center text-xs text-muted-foreground">
              正在处理中，请勿关闭窗口
            </p>
          </div>
        )}

        <div className="min-h-0 space-y-3 overflow-x-hidden overflow-y-auto">
          <Alert variant={running ? "destructive" : "warning"} className="min-w-0">
            <AlertTitle>请先完全退出 VS Code</AlertTitle>
            <AlertDescription className="min-w-0 break-words">
              {running
                ? "检测到 VS Code 正在运行。运行中写入会被覆盖且不会生效，请完全退出后重试。"
                : "复制会话与写入凭证都必须在 VS Code 完全退出后进行，否则会被运行中的编辑器覆盖。"}
            </AlertDescription>
          </Alert>

          <div className="flex items-center justify-between gap-3 rounded-md border px-3 py-2.5">
            <div className="min-w-0 flex-1">
              <div className="text-sm font-medium">复制会话到目标账号</div>
              <div
                className={
                  !loadingSessions && !hasCopyable
                    ? "text-xs text-amber-700 dark:text-amber-400"
                    : "text-xs text-muted-foreground"
                }
              >
                {emptyHint}
              </div>
            </div>
            <Switch
              checked={copyEnabled}
              onCheckedChange={setCopyEnabled}
              disabled={loadingSessions || !hasCopyable}
            />
          </div>

          {copyEnabled && (
            <>
              <Separator />
              <div className="max-h-[min(20rem,45vh)] overflow-y-auto pr-1">
                {loadingSessions ? (
                  <div className="space-y-2 py-1">
                    <Skeleton className="h-9 w-full" />
                    <Skeleton className="h-9 w-full" />
                    <Skeleton className="h-9 w-full" />
                  </div>
                ) : (
                  groups.map((group) => {
                    const open_ = !collapsed.has(group.key);
                    const ids = group.sessions.map((s) => s.id);
                    const state = selectionState(ids, selected);
                    return (
                      <div key={group.key} className="mb-0.5">
                        <div className="sticky top-0 z-10 flex items-center gap-1.5 rounded-md bg-background px-1.5 py-1">
                          <TreeCheckbox
                            allOn={state.allOn}
                            someOn={state.someOn}
                            onChange={() => toggleGroup(ids)}
                            ariaLabel={`选择${group.label}`}
                          />
                          <button
                            type="button"
                            className="flex min-w-0 flex-1 items-center gap-1 rounded px-1 py-0.5 text-left hover:bg-accent/50"
                            onClick={() => toggleCollapsed(group.key)}
                            aria-expanded={open_}
                            aria-label={`${open_ ? "折叠" : "展开"}${group.label}`}
                          >
                            <span className="min-w-0 flex-1 truncate text-sm font-medium">
                              {group.label}
                              <span className="ml-1 font-normal text-muted-foreground">
                                #{group.hash.slice(0, 8)} · {group.sessions.length}
                              </span>
                            </span>
                            {open_ ? (
                              <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
                            ) : (
                              <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
                            )}
                          </button>
                        </div>
                        {open_ &&
                          group.sessions.map((session) => (
                            <SessionRow
                              key={session.id}
                              session={session}
                              checked={selected.has(session.id)}
                              onToggle={() => toggleSession(session.id)}
                            />
                          ))}
                      </div>
                    );
                  })
                )}
              </div>
            </>
          )}

          {error && (
            <Alert variant="destructive" className="min-w-0 break-all">
              <AlertDescription className="min-w-0 break-all">{error}</AlertDescription>
            </Alert>
          )}
        </div>

        <DialogFooter className="shrink-0">
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            取消
          </Button>
          <Button onClick={doSwitch} disabled={busy || (copyEnabled && copyCount === 0)}>
            {busy ? "切换中…" : "确认切换"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

interface WorkspaceGroup {
  key: string;
  label: string;
  hash: string;
  sessions: VscodeSession[];
}

/** 按工作区 hash 分组，仅保留含正文的会话；分组按最近活动时间降序。 */
function buildGroups(sessions: VscodeSession[]): WorkspaceGroup[] {
  const byHash = new Map<string, VscodeSession[]>();
  for (const session of sessions) {
    if (!session.hasHistory) continue;
    const list = byHash.get(session.workspaceHash);
    if (list) list.push(session);
    else byHash.set(session.workspaceHash, [session]);
  }
  const entries = [...byHash.entries()];
  entries.sort((left, right) => maxUpdatedAt(right[1]) - maxUpdatedAt(left[1]));
  return entries.map(([hash, list], index) => ({
    key: hash,
    hash,
    label: `工作区 #${index + 1}`,
    sessions: [...list].sort((left, right) => right.updatedAt - left.updatedAt),
  }));
}

function maxUpdatedAt(sessions: VscodeSession[]): number {
  return sessions.reduce((max, session) => Math.max(max, session.updatedAt || 0), 0);
}

function selectionState(ids: string[], selected: Set<string>) {
  const count = ids.filter((id) => selected.has(id)).length;
  return { allOn: ids.length > 0 && count === ids.length, someOn: count > 0 && count < ids.length };
}

/** 统一空态文案：区分未装 VS Code / 未装扩展 / 未登录 / 无会话。 */
function emptyStateHint(
  status: VscodeExtStatus | null | undefined,
  loading: boolean,
  sourceUid: string | null,
  hasCopyable: boolean,
): string {
  if (loading) return "正在加载会话…";
  if (status && !status.installed) return "未检测到 VS Code，请先安装并登录 CodeBuddy 扩展";
  if (status && !status.extensionInstalled) return "未安装 CodeBuddy 扩展，请先在 VS Code 中安装并登录";
  if (!sourceUid) return "未检测到 VS Code 扩展当前登录账号，请先在 VS Code 中登录";
  if (!hasCopyable) return "当前账号暂无可复制的会话（无含正文的历史）";
  return "将当前账号勾选的会话以新 id 复制给目标账号（加法，不影响源账号）";
}

function TreeCheckbox({
  allOn,
  someOn,
  onChange,
  ariaLabel,
}: {
  allOn: boolean;
  someOn: boolean;
  onChange: () => void;
  ariaLabel: string;
}) {
  return (
    <input
      type="checkbox"
      className="size-3.5 shrink-0 accent-primary"
      checked={allOn}
      ref={(el) => {
        if (el) el.indeterminate = someOn;
      }}
      onChange={onChange}
      aria-label={ariaLabel}
    />
  );
}

function SessionRow({
  session,
  checked,
  onToggle,
}: {
  session: VscodeSession;
  checked: boolean;
  onToggle: () => void;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-2.5 rounded-md py-1.5 pl-7 pr-2 hover:bg-accent/50">
      <input
        type="checkbox"
        className="size-3.5 shrink-0 accent-primary"
        checked={checked}
        onChange={onToggle}
      />
      <span className="min-w-0 flex-1 truncate text-sm" title={session.title}>
        {session.title}
      </span>
      {session.updatedAt > 0 && (
        <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
          {formatUpdatedAt(session.updatedAt)}
        </span>
      )}
      <Badge variant="outline" className="shrink-0 text-[10px]">
        有正文
      </Badge>
    </label>
  );
}

/** 会话时间：MM/DD HH:mm（本地时区）。 */
function formatUpdatedAt(ts: number): string {
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "";
  return `${String(date.getMonth() + 1).padStart(2, "0")}/${String(date.getDate()).padStart(2, "0")} ${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}
