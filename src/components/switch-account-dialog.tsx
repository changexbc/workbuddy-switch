import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, ExternalLink, Folder, Loader2 } from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { Alert, AlertDescription } from "@/components/ui/alert";
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
import { Switch } from "@/components/ui/switch";
import * as api from "@/lib/api";
import type { AccountMeta, Session } from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 目标账号 */
  account: AccountMeta | null;
  /** 切换完成后刷新列表 */
  onDone?: () => void;
}

/** 切换账号弹窗：可勾选当前账号的会话复制到目标账号（路径 B）。 */
export function SwitchAccountDialog({ open, onOpenChange, account, onDone }: Props) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [copySessions, setCopySessions] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  /** 展开的节点：任务 / 空间 / 文件夹。默认全部收起。 */
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [currentUid, setCurrentUid] = useState<string | null>(null);
  const [progress, setProgress] = useState("");

  // 监听后端切换进度：桌面端走 Tauri 事件，webui 走 HTTP 轮询
  useEffect(() => {
    if (api.isWebui()) {
      const timer = setInterval(() => {
        void api.switchProgress().then((p) => {
          if (p.progress) setProgress(p.progress);
        });
      }, 600);
      return () => clearInterval(timer);
    }
    let unlisten: (() => void) | undefined;
    listen<{ message: string }>("switch-progress", (e) => {
      setProgress(e.payload.message);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  // 打开时加载当前账号会话
  useEffect(() => {
    if (open && account) {
      setCopySessions(false);
      setSelected(new Set());
      setExpanded(new Set());
      setError("");
      setLoadingSessions(true);
      api
        .listSessions()
        .then((res) => {
          setSessions(res.sessions);
          setCurrentUid(res.current);
        })
        .catch((e) => setError(api.asError(e)))
        .finally(() => setLoadingSessions(false));
    }
  }, [open, account]);

  function toggleSession(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleFolder(ids: string[]) {
    setSelected((prev) => {
      const next = new Set(prev);
      const allOn = ids.length > 0 && ids.every((id) => next.has(id));
      if (allOn) ids.forEach((id) => next.delete(id));
      else ids.forEach((id) => next.add(id));
      return next;
    });
  }

  function toggleExpanded(key: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  async function doSwitch() {
    if (!account) return;
    setBusy(true);
    setProgress("正在切换账号…");
    setError("");
    try {
      const res = await api.switchAccount({
        accountId: account.id,
        copySessionIds: copySessions ? [...selected] : undefined,
      });
      const nickname = account.nickname || account.email || account.uid || "该账号";
      const parts: string[] = [];
      if (res.sessionCopy?.copied.length) {
        parts.push(`已复制 ${res.sessionCopy.copied.length} 个会话`);
      }
      if (res.backup) parts.push(`备份: ${res.backup}`);
      toast.success(`已切换至「${nickname}」`, {
        description: parts.length ? parts.join("；") : "WorkBuddy 已重启为目标账号。",
      });
      onOpenChange(false);
      onDone?.();
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
      setProgress("");
    }
  }

  /** 打开系统设置授权面板（默认完全磁盘访问），供小白一键跳转。 */
  async function openPermissionSettings() {
    try {
      await api.openPermissionSettings("all_files");
    } catch (e) {
      // 打开失败时退化为提示
      setError(api.asError(e));
    }
  }

  /** 权限自检：确认完全磁盘访问是否生效。 */
  const [permCheck, setPermCheck] = useState<string | null>(null);
  async function runPermissionCheck() {
    setPermCheck("检测中…");
    try {
      const res = await api.checkAuthPermission();
      setPermCheck(res.ok ? `✓ ${res.message}` : `✗ ${res.error}（${res.dir}）`);
    } catch (e) {
      setPermCheck(`✗ ${api.asError(e)}`);
    }
  }

  // 出现「无权限」错误时，自动每 2s 轮询一次授权状态；用户拖入 app 授权成功后自动恢复
  useEffect(() => {
    if (!error.includes("无权限")) return;
    let cancelled = false;
    let timer: number | undefined;
    const check = async () => {
      try {
        const res = await api.checkAuthPermission();
        if (res.ok) {
          if (!cancelled) {
            setPermCheck("✓ 授权成功，可以重新切换了");
            setError("");
          }
          return;
        }
      } catch {
        /* 忽略中间态 */
      }
      if (!cancelled) timer = window.setTimeout(check, 2000);
    };
    check();
    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, [error]);

  const copyCount = copySessions ? selected.size : 0;
  const needsPermission = error.includes("无权限");
  const sessionsEmpty = !loadingSessions && sessions.length === 0;
  const copyHint = loadingSessions
    ? "正在加载会话…"
    : error && sessionsEmpty
      ? "无法加载会话列表，暂不能复制"
      : sessionsEmpty
        ? currentUid
          ? "当前账号暂无会话，无法复制"
          : "未检测到当前登录账号，无法列出会话"
        : "将当前账号勾选的会话以新 id 复制给目标账号（云端归属目标）";

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        showCloseButton={!busy}
        className="flex max-h-[min(90vh,calc(100vh-2rem))] min-w-0 flex-col overflow-hidden"
      >
        <DialogHeader className="shrink-0">
          <DialogTitle>切换到「{account?.nickname || account?.email || account?.uid || "该账号"}」</DialogTitle>
          <DialogDescription>
            切换会关闭并重启 WorkBuddy，认证文件将写入目标账号。
          </DialogDescription>
        </DialogHeader>

        {busy && (
          <div className="absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg bg-background/85 backdrop-blur-sm">
            <Loader2 className="size-8 animate-spin text-primary" />
            <p className="text-sm font-medium">{progress || "正在切换账号…"}</p>
            <p className="max-w-xs text-center text-xs text-muted-foreground">
              正在处理中，请勿关闭窗口
            </p>
          </div>
        )}

        <div className="min-h-0 space-y-3 overflow-x-hidden overflow-y-auto">
          <div className="flex items-center justify-between gap-3 rounded-md border px-3 py-2.5">
            <div className="min-w-0 flex-1">
              <div className="text-sm font-medium">复制会话到目标账号</div>
              <div
                className={
                  sessionsEmpty
                    ? "text-xs text-amber-700 dark:text-amber-400"
                    : "text-xs text-muted-foreground"
                }
              >
                {copyHint}
              </div>
            </div>
            <Switch
              checked={copySessions}
              onCheckedChange={setCopySessions}
              disabled={loadingSessions || sessions.length === 0}
            />
          </div>

          {copySessions && (
            <>
              <Separator />
              <div className="max-h-[min(20rem,45vh)] overflow-y-auto pr-1">
                {loadingSessions ? (
                  <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
                    <Loader2 className="animate-spin" /> 加载会话…
                  </div>
                ) : sessions.length === 0 ? (
                  <p className="py-4 text-center text-sm text-muted-foreground">
                    {currentUid ? "当前账号暂无会话" : "未检测到当前登录账号，无法列出会话"}
                  </p>
                ) : (
                  buildSessionTree(sessions).map((kind) => {
                    const kindOpen = expanded.has(kind.key);
                    const kindSel = selectionState(kind.sessions, selected);
                    return (
                      <div key={kind.key} className="mb-0.5">
                        <div className="sticky top-0 z-10 flex items-center gap-1.5 rounded-md bg-background px-1.5 py-1">
                          <TreeCheckbox
                            allOn={kindSel.allOn}
                            someOn={kindSel.someOn}
                            onChange={() => toggleFolder(kind.sessions.map((s) => s.id))}
                            ariaLabel={`选择${kind.label}`}
                          />
                          <button
                            type="button"
                            className="flex min-w-0 flex-1 items-center gap-1 rounded px-1 py-0.5 text-left hover:bg-accent/50"
                            onClick={() => toggleExpanded(kind.key)}
                            aria-expanded={kindOpen}
                            aria-label={`${kindOpen ? "折叠" : "展开"}${kind.label}`}
                          >
                            <span className="min-w-0 flex-1 truncate text-sm font-medium">
                              {kind.label}
                              <span className="ml-1 font-normal text-muted-foreground">
                                ({kind.count})
                              </span>
                            </span>
                            {kindOpen ? (
                              <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
                            ) : (
                              <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
                            )}
                          </button>
                        </div>
                        {kindOpen && kind.key === "task" &&
                          kind.sessions.map((s) => (
                            <SessionPickRow
                              key={s.id}
                              session={s}
                              checked={selected.has(s.id)}
                              indentClass="pl-7"
                              onToggle={() => toggleSession(s.id)}
                            />
                          ))}
                        {kindOpen &&
                          kind.folders?.map((folder) => {
                            const folderOpen = expanded.has(folder.key);
                            const folderSel = selectionState(folder.sessions, selected);
                            return (
                              <div key={folder.key}>
                                <div className="flex items-center gap-1.5 px-1.5 py-0.5 pl-7">
                                  <TreeCheckbox
                                    allOn={folderSel.allOn}
                                    someOn={folderSel.someOn}
                                    onChange={() => toggleFolder(folder.sessions.map((s) => s.id))}
                                    ariaLabel={`选择文件夹 ${folder.label}`}
                                  />
                                  <button
                                    type="button"
                                    className="flex min-w-0 flex-1 items-center gap-1.5 rounded px-1 py-0.5 text-left hover:bg-accent/50"
                                    onClick={() => toggleExpanded(folder.key)}
                                    aria-expanded={folderOpen}
                                    aria-label={`${folderOpen ? "折叠" : "展开"}文件夹 ${folder.label}`}
                                  >
                                    <Folder className="size-3.5 shrink-0 text-muted-foreground" />
                                    <span className="min-w-0 flex-1 truncate text-sm">
                                      {folder.label}
                                    </span>
                                    {folderOpen ? (
                                      <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
                                    ) : (
                                      <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
                                    )}
                                  </button>
                                </div>
                                {folderOpen &&
                                  folder.sessions.map((s) => (
                                    <SessionPickRow
                                      key={s.id}
                                      session={s}
                                      checked={selected.has(s.id)}
                                      indentClass="pl-12"
                                      onToggle={() => toggleSession(s.id)}
                                    />
                                  ))}
                              </div>
                            );
                          })}
                      </div>
                    );
                  })
                )}
              </div>
            </>
          )}

          {error && (
            <Alert variant={needsPermission ? "warning" : "destructive"} className="min-w-0 break-all">
              <AlertDescription className="min-w-0 break-all">
                <div className="min-w-0 break-all">{error}</div>
                {needsPermission && (
                  <div className="mt-2 space-y-2">
                    <div className="rounded-md border bg-muted/60 p-3 text-xs text-muted-foreground">
                      <p className="mb-1 font-medium text-foreground">如何授权（只需 3 步）：</p>
                      <ol className="list-decimal space-y-1 pl-4">
                        <li>点击下方「打开完全磁盘访问」</li>
                        <li>
                          把 <b>workbuddy-switch.app</b> 从 Finder 拖进面板列表（即使没提示框也直接拖），
                          打开它的开关
                        </li>
                        <li>授权后这里会自动检测到，无需其他操作</li>
                      </ol>
                    </div>
                    <div className="flex flex-wrap gap-2">
                      <Button variant="outline" size="sm" onClick={openPermissionSettings}>
                        <ExternalLink />
                        打开完全磁盘访问
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void api.revealAppInFinder()}
                      >
                        在 Finder 中显示
                      </Button>
                      <Button variant="secondary" size="sm" onClick={runPermissionCheck}>
                        立即检测
                      </Button>
                    </div>
                  </div>
                )}
                {permCheck && <div className="mt-2 text-xs">{permCheck}</div>}
              </AlertDescription>
            </Alert>
          )}
        </div>

        <DialogFooter className="shrink-0">
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            取消
          </Button>
          <Button onClick={doSwitch} disabled={busy || (copySessions && copyCount === 0)}>
            {busy ? "切换中…" : "确认切换"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

type FolderGroup = { key: string; label: string; sessions: Session[] };
type KindGroup = {
  key: "task" | "space";
  label: string;
  count: number;
  sessions: Session[];
  folders?: FolderGroup[];
};

function selectionState(sessions: Session[], selected: Set<string>) {
  const ids = sessions.map((s) => s.id);
  const n = ids.filter((id) => selected.has(id)).length;
  return { allOn: n === ids.length && ids.length > 0, someOn: n > 0 && n < ids.length };
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

function SessionPickRow({
  session,
  checked,
  indentClass,
  onToggle,
}: {
  session: Session;
  checked: boolean;
  indentClass: string;
  onToggle: () => void;
}) {
  return (
    <label
      className={`flex cursor-pointer items-center gap-2.5 rounded-md py-1.5 pr-2 hover:bg-accent/50 ${indentClass}`}
    >
      <input
        type="checkbox"
        className="size-3.5 shrink-0 accent-primary"
        checked={checked}
        onChange={onToggle}
      />
      <span className="min-w-0 flex-1 truncate text-sm" title={session.title}>
        {session.title}
      </span>
      {session.hasHistory && (
        <Badge variant="outline" className="shrink-0 text-[10px]">
          有正文
        </Badge>
      )}
    </label>
  );
}

/** WorkBuddy 侧栏文件夹名：cwd 最后一段。 */
function sessionFolderLabel(cwd: string): string {
  const normalized = cwd.trim().replace(/[\\/]+$/, "");
  if (!normalized) return "未分组";
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || normalized;
}

/** 按工作目录分组，文件夹顺序跟会话一样按最近活动排。 */
function groupSessionsByFolder(sessions: Session[]): FolderGroup[] {
  const groups = new Map<string, Session[]>();
  const order: string[] = [];
  for (const session of sessions) {
    const key = session.cwd.trim() || "__none__";
    let list = groups.get(key);
    if (!list) {
      list = [];
      groups.set(key, list);
      order.push(key);
    }
    list.push(session);
  }
  return order.map((key) => ({
    key,
    label: key === "__none__" ? "未分组" : sessionFolderLabel(key),
    sessions: groups.get(key) ?? [],
  }));
}

/** 对齐 WorkBuddy 侧栏：任务（playground）平铺，空间按文件夹分组。 */
function buildSessionTree(sessions: Session[]): KindGroup[] {
  const tasks = sessions.filter((s) => s.isPlayground);
  const spaces = sessions.filter((s) => !s.isPlayground);
  const groups: KindGroup[] = [];
  if (tasks.length > 0) {
    groups.push({ key: "task", label: "任务", count: tasks.length, sessions: tasks });
  }
  if (spaces.length > 0) {
    const folders = groupSessionsByFolder(spaces);
    groups.push({
      key: "space",
      label: "空间",
      count: folders.length,
      sessions: spaces,
      folders,
    });
  }
  return groups;
}
