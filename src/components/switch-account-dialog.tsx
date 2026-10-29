import { useEffect, useLayoutEffect, useState } from "react";
import { CircleAlert, ExternalLink, Loader2 } from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { DialogAutoHeight } from "@/components/ui/dialog-auto-height";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { SessionSyncSection, type SessionLinksMeta } from "@/components/session-sync-section";
import { SessionTreeList } from "@/components/session-tree";
import * as api from "@/lib/api";
import { displayName } from "@/lib/account-display";
import { accountVariant, variantAppName } from "@/lib/variant";
import type {
  AccountMeta,
  Session,
  SessionLinkPreviewGroup,
  SessionSyncSelection,
  TemporaryFileInfo,
} from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 目标账号 */
  account: AccountMeta | null;
  /** 切换完成后刷新列表 */
  onDone?: () => void;
}

/** 设计稿的 tab 样式：下划线指示 + 可选计数徽标。 */
const TAB_TRIGGER_CLASS =
  "-mb-px h-9 flex-none rounded-none border-b-2 border-transparent px-0.5 pb-2 text-sm font-medium text-muted-foreground hover:text-foreground data-[state=active]:border-primary data-[state=active]:bg-transparent data-[state=active]:text-foreground data-[state=active]:shadow-none";

/** tab 计数徽标：0 时不显示，避免出现空的「0」。 */
function tabCount(count: number) {
  return count > 0 ? (
    <span className="ml-1 text-xs text-muted-foreground tabular-nums">{count}</span>
  ) : null;
}

/**
 * 会话列表区最小高度：加载态、空态与列表共用同一下沿。
 *
 * 弹窗垂直居中，内容高度一变弹窗就上下撑开；
 * 打开时先渲染加载态、会话数据到达后换成列表，两端高度差越大跳得越明显。
 * 与「关联会话」tab 的下沿取同一数值，两个 tab 打开时的高度表现保持一致。
 */
const LIST_MIN_H = "min-h-[min(7.5rem,26vh)]";

/** 临时备份残留的可读描述：优先标题，其次会话 id，最后操作 id。 */
function describeTemporaryFile(item: TemporaryFileInfo): string {
  const label = item.title || item.sessionId || item.operationId;
  return `${label}：${item.reason}`;
}

/** 同一操作只保留一条：复制 / 同步 / 恢复三个报告可能重复上报同一残留。 */
function dedupeTemporaryFiles(items: TemporaryFileInfo[]): TemporaryFileInfo[] {
  const seen = new Map<string, TemporaryFileInfo>();
  for (const item of items) {
    const key = `${item.operationId}:${item.state}`;
    const existing = seen.get(key);
    if (!existing || (existing.state === "cleanupPending" && item.state === "needsRecovery")) {
      seen.set(key, item);
    }
  }
  return [...seen.values()];
}

/** 切换账号弹窗：可勾选当前账号的会话复制到目标账号（路径 B）。 */
export function SwitchAccountDialog({ open, onOpenChange, account, onDone }: Props) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  /** 展开的节点：任务 / 空间 / 文件夹。默认全部收起。 */
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [currentUid, setCurrentUid] = useState<string | null>(null);
  const [progress, setProgress] = useState("");
  /** 会话同步：勾选结果（默认值来自后端 defaultChecked）与预览组（结果反馈回显用）。 */
  const [syncSelections, setSyncSelections] = useState<SessionSyncSelection[]>([]);
  const [syncGroups, setSyncGroups] = useState<SessionLinkPreviewGroup[]>([]);
  /** 当前 tab：默认关联会话；该 tab 不可用时回落到复制会话。 */
  const [tab, setTab] = useState<"links" | "copy">("links");
  /** 关联会话区块上报的状态：tab 徽标与常驻提示用。 */
  const [linksMeta, setLinksMeta] = useState<SessionLinksMeta | null>(null);

  const variant = accountVariant(account);
  const accountId = account?.id;

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

  // 立即打开弹框；首次绘制前重置旧内容，两个 tab 各自加载数据。
  useLayoutEffect(() => {
    if (!open || !accountId) return;
    let cancelled = false;
    setSelected(new Set());
    setExpanded(new Set());
    setPermCheck(null);
    setError("");
    setSessions([]);
    setCurrentUid(null);
    setSyncSelections([]);
    setSyncGroups([]);
    setLinksMeta(null);
    setTab("links");
    setLoadingSessions(true);
    void api.listSessions(variant)
      .then((res) => {
        if (cancelled) return;
        setSessions(res.sessions);
        setCurrentUid(res.current);
      })
      .catch((e) => {
        if (!cancelled) setError(api.asError(e));
      })
      .finally(() => {
        if (!cancelled) setLoadingSessions(false);
      });
    // 关闭时只取消回写，保留内容到退出动画结束。
    return () => { cancelled = true; };
  }, [open, accountId, variant]);

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
    const requestedCopy = selected.size > 0;
    const requestedSync = syncSelections.length > 0;
    try {
      const res = await api.switchAccount({
        accountId: account.id,
        copySessionIds: requestedCopy ? [...selected] : undefined,
        // 勾选绑定预览凭据；执行前后端会重新校验，版本变化则跳过该项。
        syncSelections: requestedSync ? syncSelections : undefined,
      });
      const nickname = displayName(account);
      const parts: string[] = [];
      const copyReport = res.sessionCopy;
      const copiedCount = copyReport?.copied?.length ?? 0;
      const linkedCount = copyReport?.alreadyLinked?.length ?? 0;
      const copyErrors = copyReport?.errors ?? [];
      if (copiedCount > 0) parts.push(`已复制 ${copiedCount} 个会话`);
      if (linkedCount > 0) parts.push(`已关联 ${linkedCount} 个之前复制过的会话`);
      if (copyErrors.length > 0) parts.push(`会话复制失败 ${copyErrors.length} 个`);
      const syncReport = res.sessionSync;
      const syncedItems = syncReport?.synced ?? [];
      const skippedItems = syncReport?.skipped ?? [];
      const syncErrors = syncReport?.errors ?? [];
      if (syncedItems.length > 0) parts.push(`已同步 ${syncedItems.length} 个会话`);
      if (skippedItems.length > 0) parts.push(`跳过 ${skippedItems.length} 个会话`);
      if (syncErrors.length > 0) parts.push(`会话同步失败 ${syncErrors.length} 个`);
      if (res.backup) parts.push(`备份: ${res.backup}`);
      // 成功清理：临时备份已回收，不再展示可还原路径；待清理项单独提示，不写进成功文案。
      toast.success(`已切换至「${nickname}」`, {
        description: parts.length ? parts.join("；") : `${variantAppName(accountVariant(account))} 已重启为目标账号。`,
      });
      // 复制失败或被后端跳过时必须显式提示，不能静默当成成功。
      if (copyReport?.error) {
        toast.error("会话复制未执行", { description: copyReport.error });
      } else if (copyErrors.length > 0) {
        // 失败提示必须带会话标识（能取标题时优先标题），不是只有原因。
        const sessionLabel = (id: string) =>
          sessions.find((item) => item.id === id)?.title || id;
        toast.error("部分会话未复制", {
          description: copyErrors
            .map((item) => `${sessionLabel(item.id)}：${item.error}`)
            .join("；"),
        });
      } else if (requestedCopy && !copyReport) {
        toast.warning("会话未复制", {
          description: "没有收到复制结果：当前版本可能不支持复制会话；账号已切换，但会话没有复制。",
        });
      }
      // 同步结果：成功、跳过、失败与恢复信息都要能看到，不能只显示成功数。
      const groupLabel = (groupId: string) =>
        syncGroups.find((group) => group.groupId === groupId)?.title || groupId;
      // 失败与跳过合并成一条提示：两者可能同时出现，不能只报其中一类。
      const syncIssues = [
        ...syncErrors.map(
          (item) => `${item.groupId ? groupLabel(item.groupId) : "全部会话"}：${item.error}`,
        ),
        // 预览过期/前置条件变化一律跳过并给出原因，绝不显示成已同步。
        ...skippedItems.map((item) => `${groupLabel(item.groupId)}：${item.message}`),
      ];
      if (syncIssues.length > 0) {
        if (syncErrors.length > 0) {
          toast.error("部分会话没有同步（失败或已跳过）", { description: syncIssues.join("；") });
        } else {
          toast.warning("有会话没有同步（已跳过）", { description: syncIssues.join("；") });
        }
      } else if (requestedSync && !syncReport) {
        toast.warning("会话同步未执行", {
          description: "没有收到同步结果：当前版本可能不支持同步会话，或本次切换没有重启应用。",
        });
      }
      if (syncReport?.needsRecovery) {
        toast.error("有会话同步没有完成", {
          description: "已保留操作记录与备份，下次切换会先恢复；恢复完成前不会再改动目标账号的内容。",
        });
      }
      // 未完成的会话写入：可重试项只提示，阻断项由后端直接返回错误。
      const recoveryIssues = res.sessionRecovery?.needsRecovery ?? [];
      if (recoveryIssues.length > 0) {
        toast.error("有会话操作没有完成", {
          description: recoveryIssues.map((issue) => issue.reason).join("；"),
        });
      }
      // 临时备份残留：待清理说明重试入口；待恢复说明必须保留材料并给出下一步。
      const temporaryFiles = dedupeTemporaryFiles([
        ...(copyReport?.temporaryFiles ?? []),
        ...(syncReport?.temporaryFiles ?? []),
        ...(res.sessionRecovery?.temporaryFiles ?? []),
      ]);
      const pendingFiles = temporaryFiles.filter((item) => item.state === "cleanupPending");
      const recoveryFiles = temporaryFiles.filter((item) => item.state === "needsRecovery");
      // 报告级 temporaryFiles 是权威来源；仅当旧后端没给该字段时，才回退到成功项上的 pending。
      const pendingCleanupReasons: string[] = [];
      if (temporaryFiles.length === 0) {
        for (const item of copyReport?.copied ?? []) {
          if (item.cleanupState === "pending") {
            pendingCleanupReasons.push(item.cleanupError ?? "临时文件待清理");
          }
        }
        for (const item of syncedItems) {
          if (item.cleanupState === "pending") {
            pendingCleanupReasons.push(item.cleanupError ?? "临时文件待清理");
          }
        }
      }
      if (pendingCleanupReasons.length > 0 || pendingFiles.length > 0) {
        const details = [...pendingCleanupReasons, ...pendingFiles.map(describeTemporaryFile)];
        toast.warning("会话已完成，部分临时文件待清理", {
          description: `${details.join("；")}。下次切号时会自动重试清理。`,
        });
      }
      if (recoveryFiles.length > 0) {
        toast.error("有临时备份需要确认", {
          description: `${recoveryFiles.map(describeTemporaryFile).join("；")}。已保留现场，请按提示处理后重试。`,
        });
      }
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

  /** 权限自检：确认完全磁盘访问是否生效（探针按目标账号档位）。 */
  const [permCheck, setPermCheck] = useState<string | null>(null);
  async function runPermissionCheck() {
    setPermCheck("检测中…");
    try {
      const res = await api.checkAuthPermission(accountVariant(account));
      setPermCheck(res.ok ? `✓ ${res.message}` : `✗ ${res.error}（${res.dir}）`);
    } catch (e) {
      setPermCheck(`✗ ${api.asError(e)}`);
    }
  }

  // 出现「无权限」错误时，自动每 2s 轮询一次授权状态；用户拖入 app 授权成功后自动恢复
  useEffect(() => {
    if (!open || !error.includes("无权限")) return;
    let cancelled = false;
    let timer: number | undefined;
    const check = async () => {
      try {
        const res = await api.checkAuthPermission(accountVariant(account));
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
  }, [error, open, variant]);

  const copyCount = selected.size;
  const syncCount = syncSelections.length;
  /** 覆盖项数量：底部摘要据此提示风险。 */
  const overwriteCount = syncSelections.filter((item) => item.mode === "overwrite").length;
  const linksAvailable = linksMeta?.available ?? true;
  const summaryMain =
    copyCount > 0 && syncCount > 0
      ? `将复制 ${copyCount} 个、同步 ${syncCount} 个关联会话`
      : copyCount > 0
        ? `将复制 ${copyCount} 个会话`
        : syncCount > 0
          ? `将同步 ${syncCount} 个关联会话`
          : "本次仅切换账号";
  const summarySub =
    overwriteCount > 0
      ? `其中 ${overwriteCount} 个会替换目标账号的完整内容`
      : copyCount === 0 && syncCount === 0
        ? "未选择复制或同步会话"
        : copyCount === 0
          ? "未选择复制会话"
          : syncCount === 0
            ? "未选择同步会话"
            : null;
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
        : "把当前账号勾选的会话复制给目标账号（复制后归属目标账号）；已经复制过的不会重复复制；已置顶的会话会同步置顶到目标账号";

  // 关联 tab 不可用（国际版能力判定不通过）时回落到复制会话，避免停在空 tab。
  useEffect(() => {
    if (!linksAvailable && tab === "links") setTab("copy");
  }, [linksAvailable, tab]);

  // 「复制会话」tab 内容：勾选即意图，提交结果由底部摘要兜底确认。
  const copyTabContent = (
    <>
      {loadingSessions ? (
        // 两行高度对齐真实提示文案（有会话时折成两行）。
        <div className="px-1">
          <Skeleton className="h-4 w-full" />
          <Skeleton className="h-4 w-3/4" />
        </div>
      ) : (
        <p
          className={
            sessionsEmpty
              ? "px-1 text-xs text-amber-700 dark:text-amber-400"
              : "px-1 text-xs text-muted-foreground"
          }
        >
          {copyHint}
        </p>
      )}
      {loadingSessions ? (
        <div
          className={`flex items-center justify-center gap-2 text-sm text-muted-foreground ${LIST_MIN_H}`}
        >
          <Loader2 className="animate-spin" /> 加载会话…
        </div>
      ) : sessions.length === 0 ? (
        <p
          className={`flex items-center justify-center px-3 text-center text-sm text-muted-foreground ${LIST_MIN_H}`}
        >
          {currentUid ? "当前账号暂无会话" : "未检测到当前登录账号，无法列出会话"}
        </p>
      ) : (
        <SessionTreeList
          sessions={sessions}
          selected={selected}
          expanded={expanded}
          onToggleSession={toggleSession}
          onToggleGroup={toggleFolder}
          onToggleExpanded={toggleExpanded}
          className={`max-h-[min(22rem,45vh)] overflow-y-auto pr-1 ${LIST_MIN_H}`}
        />
      )}
    </>
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        showCloseButton={!busy}
        className="flex max-h-[min(90vh,calc(100vh-2rem))] min-w-0 flex-col gap-0 overflow-hidden p-0"
      >
        <DialogAutoHeight>
        <div className="flex max-h-[calc(min(90vh,100vh-2rem)-2px)] min-w-0 flex-col gap-4 p-6">
        <DialogHeader className="shrink-0">
          <DialogTitle>切换到「{account ? displayName(account) : "该账号"}」</DialogTitle>
          <DialogDescription>切换时将重启 {variantAppName(accountVariant(account))}。</DialogDescription>
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

        {/* min-h-0：矮窗口下压缩本区而不是裁切页脚。列表按自身 max-h 滚动；矮窗口或权限大卡时 tab 内容区可滚，避免裁切。仅当常驻提示超过剩余空间时本层滚动。 */}
        <div className="flex min-h-0 flex-col gap-3 overflow-x-hidden overflow-y-auto">
          {/* 常驻提示区：切换错误 / 权限 / 关联检查失败不随 tab 切换隐藏。 */}
          {error && (
            <Alert variant={needsPermission ? "warning" : "destructive"} className="min-w-0 shrink-0 break-all">
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
          {linksMeta?.error && (
            <Alert variant="destructive" className="min-w-0 shrink-0">
              <CircleAlert />
              <AlertTitle>无法检查会话</AlertTitle>
              <AlertDescription className="min-w-0 break-all">{linksMeta.error}</AlertDescription>
            </Alert>
          )}
          {linksMeta?.storeStatus === "unavailable" && (
            <Alert variant="warning" className="min-w-0 shrink-0">
              <CircleAlert />
              <AlertTitle>同步记录不可用</AlertTitle>
              <AlertDescription className="min-w-0 break-all">
                {`${linksMeta.storeError || "原因未知"}；本次不会同步任何会话，请处理后重试。`}
              </AlertDescription>
            </Alert>
          )}

          {linksAvailable ? (
            <Tabs
              value={tab}
              onValueChange={(value) => setTab(value as "links" | "copy")}
              className="flex min-h-0 flex-col gap-3 overflow-hidden"
            >
              <TabsList className="h-auto w-full shrink-0 justify-start gap-5 rounded-none border-b border-border bg-transparent p-0">
                <TabsTrigger value="links" className={TAB_TRIGGER_CLASS}>
                  关联会话
                  {tabCount(linksMeta?.groupCount ?? 0)}
                </TabsTrigger>
                <TabsTrigger value="copy" className={TAB_TRIGGER_CLASS}>
                  复制会话
                  {tabCount(copyCount)}
                </TabsTrigger>
              </TabsList>
              {/* forceMount：切换 tab 不得卸载另一侧，否则关联勾选会被预览重拉重置。 */}
              <TabsContent
                value="links"
                forceMount
                className="min-h-0 overflow-y-auto data-[state=inactive]:hidden data-[state=active]:animate-in data-[state=active]:fade-in-0 data-[state=active]:duration-150"
              >
                <SessionSyncSection
                  open={open}
                  account={account}
                  disabled={busy}
                  onChange={(state) => {
                    setSyncSelections(state.selections);
                    setSyncGroups(state.groups);
                  }}
                  onMetaChange={setLinksMeta}
                />
              </TabsContent>
              <TabsContent
                value="copy"
                forceMount
                className="min-h-0 space-y-2 overflow-y-auto data-[state=inactive]:hidden data-[state=active]:animate-in data-[state=active]:fade-in-0 data-[state=active]:duration-150"
              >
                {copyTabContent}
              </TabsContent>
            </Tabs>
          ) : (
            <div className="min-h-0 space-y-2 overflow-y-auto">{copyTabContent}</div>
          )}
        </div>

        <DialogFooter className="shrink-0 sm:justify-between">
          <div className="min-w-0 space-y-0.5">
            <div className="text-sm font-medium">{summaryMain}</div>
            {summarySub && <div className="text-xs text-muted-foreground">{summarySub}</div>}
          </div>
          <div className="flex shrink-0 gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
              取消
            </Button>
            <Button onClick={doSwitch} disabled={busy}>
              {busy ? "切换中…" : "确认切换"}
            </Button>
          </div>
        </DialogFooter>
        </div>
        </DialogAutoHeight>
      </DialogContent>
    </Dialog>
  );
}

