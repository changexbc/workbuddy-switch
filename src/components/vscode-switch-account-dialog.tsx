import { useEffect, useMemo, useState } from "react";
import { CircleCheck, Loader2, RefreshCw, TriangleAlert } from "lucide-react";
import { toast } from "sonner";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { buildGroups, SessionCopyTab } from "@/components/session-copy-tab";
import type { SessionLinksMeta } from "@/components/session-link-shared";
import { VscodeSessionSyncSection } from "@/components/vscode-session-sync-section";
import * as api from "@/lib/api";
import { displayName } from "@/lib/account-display";
import { useT, type Translate } from "@/lib/i18n";
import type {
  AccountMeta,
  SessionLinkPreviewGroup,
  SessionSyncSelection,
  VscodeExtStatus,
  VscodeSession,
  VscodeSessionRef,
} from "@/lib/types";
import { cn } from "@/lib/utils";

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

/** 自动关闭并重开 VS Code 的开关持久化 key（缺省开启，与 `wb-switch.compact` 同风格）。 */
const AUTO_RESTART_KEY = "wb-switch.vscodeExt.autoRestart";

/** tab 样式：下划线指示 + 可选计数徽标（与 WorkBuddy 切号弹窗一致）。 */
const TAB_TRIGGER_CLASS =
  "-mb-px h-9 flex-none rounded-none border-b-2 border-transparent px-0.5 pb-2 text-sm font-medium text-muted-foreground hover:text-foreground data-[state=active]:border-primary data-[state=active]:bg-transparent data-[state=active]:text-foreground data-[state=active]:shadow-none";

/** tab 计数徽标：0 时不显示，避免出现空的「0」。 */
function tabCount(count: number) {
  return count > 0 ? (
    <span className="ml-1 text-xs text-muted-foreground tabular-nums">{count}</span>
  ) : null;
}

/** VS Code 扩展会话切换弹窗：可勾选「当前扩展账号」的会话复制到目标账号。 */
export function VscodeSwitchAccountDialog({ open, onOpenChange, account, vscodeExtStatus, onDone }: Props) {
  const t = useT();
  const [sessions, setSessions] = useState<VscodeSession[]>([]);
  const [sourceUid, setSourceUid] = useState<string | null>(null);
  /** 扩展数据根目录：`null` = 未找到（与「有目录但无会话」区分）；`undefined` = 后端未返回该字段。 */
  const [dataRoot, setDataRoot] = useState<string | null | undefined>(undefined);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [copyEnabled, setCopyEnabled] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  /** 展开的工作区分组（默认全部展开，会话较少）。 */
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  /** 自动关闭并重开 VS Code：默认开启，持久化到 localStorage。 */
  const [autoRestart, setAutoRestart] = useState<boolean>(() => {
    try {
      return localStorage.getItem(AUTO_RESTART_KEY) !== "0";
    } catch {
      return true;
    }
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  /** 当前 tab：与 WorkBuddy 切换弹窗同一顺序与默认值（关联会话在前）。 */
  const [tab, setTab] = useState<"links" | "copy">("links");
  /** 关联会话区块上报的状态（tab 徽标 / 未登录时隐藏 tab）。 */
  const [linksMeta, setLinksMeta] = useState<SessionLinksMeta | null>(null);
  /** 勾选提交给后端的同步选择（绑定预览凭据，执行前后端会重新校验）。 */
  const [syncSelections, setSyncSelections] = useState<SessionSyncSelection[]>([]);
  /** 已勾选的关联会话（结果反馈里回显会话名）。 */
  const [syncGroups, setSyncGroups] = useState<SessionLinkPreviewGroup[]>([]);

  function toggleAutoRestart(next: boolean) {
    setAutoRestart(next);
    try {
      localStorage.setItem(AUTO_RESTART_KEY, next ? "1" : "0");
    } catch {
      /* 存储不可用时静默 */
    }
  }

  // 打开时加载「当前扩展账号」可复制的会话。
  useEffect(() => {
    if (!open || !account) return;
    setCopyEnabled(false);
    setSelected(new Set());
    setCollapsed(new Set());
    setDataRoot(undefined);
    setError("");
    setLoadingSessions(true);
    api
      .listVscodeSessions()
      .then((res) => {
        setSessions(res.sessions);
        setSourceUid(res.sourceUid);
        setDataRoot(res.dataRoot);
      })
      .catch(() => {
        setSessions([]);
        setSourceUid(null);
        setDataRoot(undefined);
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

      // 勾选绑定预览凭据；执行前后端会重新校验，版本变化则跳过该项。
      const res = await api.switchVscodeExtAccount(
        account.id,
        autoRestart,
        refs,
        syncSelections.length > 0 ? syncSelections : undefined,
      );
      const nickname = displayName(account);
      const copied = res.sessionCopy?.copied.length ?? 0;
      const errors = res.sessionCopy?.errors ?? [];
      const linkErrors = res.sessionCopy?.linkErrors ?? [];
      const syncReport = res.sessionSync;
      const syncedItems = syncReport?.synced ?? [];
      const skippedItems = syncReport?.skipped ?? [];
      const syncErrors = syncReport?.errors ?? [];
      // 生效方式提示：重载窗口读不到外部写入，不再提示；区分「已重开 / 重开失败 / 本来没运行」。
      // 重开失败时前端拿不到原因，用后端 message（含具体错误）兜底。
      const restartHint = res.restarted
        ? t("dialogs.vscode.restarted")
        : res.closedByUs
          ? res.message || t("dialogs.vscode.restartFailed")
          : t("dialogs.vscode.openToApply");
      const copiedHint = copied > 0 ? t("dialogs.common.copiedCount", { count: copied }) : null;
      const syncedHint =
        syncedItems.length > 0 ? t("dialogs.common.syncedCount", { count: syncedItems.length }) : null;
      const requestedSync = syncSelections.length > 0;

      if (errors.length > 0) {
        toast.warning(t("dialogs.common.switchedPartialCopy", { name: nickname }), {
          description: [
            t("dialogs.common.copiedCount", { count: copied }),
            t("dialogs.common.failedCount", {
              count: errors.length,
              errors: errors.map((e) => e.error).join(t("dialogs.common.separator")),
            }),
            restartHint,
          ].join(t("dialogs.common.separator")),
        });
      } else {
        toast.success(t("dialogs.common.switchedTo", { name: nickname }), {
          description: [copiedHint, syncedHint, restartHint]
            .filter(Boolean)
            .join(t("dialogs.common.separator")),
        });
      }
      // 复制成功但登记失败：复制本身有效，必须提示「未建立关联」而不是当成完整成功。
      if (linkErrors.length > 0) {
        toast.warning(t("dialogs.common.linkPartialFailed"), {
          description: [
            ...linkErrors.map((item) =>
              t("dialogs.common.itemError", {
                label: item.conversationId ?? t("dialogs.common.sessionFallback"),
                error: item.error,
              }),
            ),
            t("dialogs.common.unlinkedNotListed"),
          ].join(t("dialogs.common.separator")),
        });
      }
      // 同步结果：成功、跳过、失败都要能看到，不能只显示成功数。
      const groupLabel = (groupId: string) =>
        syncGroups.find((group) => group.groupId === groupId)?.title || groupId;
      const syncIssues = [
        ...syncErrors.map((item) =>
          t("dialogs.common.itemError", {
            label: item.groupId ? groupLabel(item.groupId) : t("dialogs.common.allSessions"),
            error: item.error,
          }),
        ),
        // 预览过期/前置条件变化一律跳过并给出原因，绝不显示成已同步。
        ...skippedItems.map((item) =>
          t("dialogs.common.itemError", { label: groupLabel(item.groupId), error: item.message }),
        ),
      ];
      if (syncIssues.length > 0) {
        if (syncErrors.length > 0) {
          toast.error(t("dialogs.common.syncPartialFailedLinked"), {
            description: syncIssues.join(t("dialogs.common.separator")),
          });
        } else {
          toast.warning(t("dialogs.common.syncSkippedLinked"), {
            description: syncIssues.join(t("dialogs.common.separator")),
          });
        }
      } else if (requestedSync && !syncReport) {
        toast.warning(t("dialogs.common.syncNotRunLinked"), {
          description: t("dialogs.common.syncNotRunLinkedDescription"),
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
  const syncCount = syncSelections.length;
  /** 覆盖项数量：底部摘要据此提示风险。 */
  const overwriteCount = syncSelections.filter((item) => item.mode === "overwrite").length;
  /** 关联会话 tab 是否可用：区块不可用（能力判定不通过）时回落到仅复制会话。 */
  const linksAvailable = linksMeta?.available ?? true;
  const summaryMain =
    copyCount > 0 && syncCount > 0
      ? t("dialogs.common.summaryCopyAndSync", { copy: copyCount, sync: syncCount })
      : copyCount > 0
        ? t("dialogs.common.summaryCopy", { count: copyCount })
        : syncCount > 0
          ? t("dialogs.common.summarySync", { count: syncCount })
          : t("dialogs.common.summarySwitchOnly");
  const summarySub =
    overwriteCount > 0
      ? t("dialogs.common.summaryOverwrite", { count: overwriteCount })
      : copyCount === 0 && syncCount === 0
        ? t("dialogs.common.summaryNoneSelected")
        : copyCount === 0
          ? t("dialogs.common.summaryNoCopy")
          : syncCount === 0
            ? t("dialogs.common.summaryNoSync")
            : null;
  const running = vscodeExtStatus?.running === true;
  /** 扩展未登录（`loggedIn === false`）：按新会话写入，仅影响提示文案。 */
  const notLoggedIn = vscodeExtStatus?.loggedIn === false;
  /** 本次会由后端关闭并重开 VS Code（仅在编辑器正在运行且开关打开时）。 */
  const autoClose = running && autoRestart;
  /** 未登录时的追加说明（三态提示共用，渲染为 tooltip 内第二段）。 */
  const notLoggedInHint = notLoggedIn ? t("dialogs.vscode.notLoggedInHint") : undefined;
  const emptyHint = emptyStateHint(vscodeExtStatus, loadingSessions, sourceUid, dataRoot, hasCopyable, t);
  /** 标题行状态图标：三态提示收进 tooltip，仅以图标色调区分正常/警告。 */
  const statusNotice = running
    ? autoRestart
      ? {
          icon: <RefreshCw className="size-4" />,
          title: t("dialogs.vscode.autoRestartTitle"),
          description: t("dialogs.vscode.autoRestartDescription"),
          warning: false,
        }
      : {
          icon: <TriangleAlert className="size-4" />,
          title: t("dialogs.vscode.quitFirstTitle"),
          description: t("dialogs.vscode.quitFirstDescription"),
          warning: true,
        }
    : {
        icon: <CircleCheck className="size-4" />,
        title: t("dialogs.vscode.notRunningTitle"),
        description: t("dialogs.vscode.notRunningDescription"),
        warning: false,
      };

  // 关联 tab 不可用（能力判定不通过）时回落到复制会话，避免停在空 tab。
  useEffect(() => {
    if (!linksAvailable && tab === "links") setTab("copy");
  }, [linksAvailable, tab]);

  // 「复制会话」tab 内容：勾选即意图，提交结果由底部摘要兜底确认。
  const copyTabContent = (
    <SessionCopyTab
      loading={loadingSessions}
      hint={emptyHint}
      hasCopyable={hasCopyable}
      enabled={copyEnabled}
      onEnabledChange={setCopyEnabled}
      groups={groups}
      selected={selected}
      onToggleSession={toggleSession}
      onToggleGroup={toggleGroup}
      collapsed={collapsed}
      onToggleCollapsed={toggleCollapsed}
    />
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        showCloseButton={!busy}
        className="flex max-h-[min(90vh,calc(100vh-2rem))] min-w-0 flex-col overflow-hidden"
        tabIndex={-1}
        onOpenAutoFocus={(event) => {
          // Radix 默认把焦点交给第一个可聚焦元素（状态图标 / 自动重开开关），
          // 其 Tooltip 会因 focus 常驻在弹窗上；改为聚焦弹窗容器本身。
          event.preventDefault();
          const container = event.target as HTMLElement | null;
          window.requestAnimationFrame(() => container?.focus());
        }}
      >
        <DialogHeader className="shrink-0">
          <div className="flex items-center gap-3">
            <DialogTitle className="min-w-0 flex-1">
              {t("dialogs.common.switchTitle", {
                name: account ? displayName(account) : t("dialogs.common.targetAccount"),
              })}
            </DialogTitle>
            {/* 提示收进图标 tooltip、开关与标题同行；mr-6 为右上角关闭按钮留空位。 */}
            <div className="mr-6 flex shrink-0 items-center gap-2">
              <Tooltip>
                <TooltipTrigger asChild>
                  {/* 仅悬停出提示：不加 tabIndex，避免点击/自动聚焦后 Tooltip 常驻不消失。 */}
                  <span
                    className={cn(
                      "flex size-5 cursor-help items-center justify-center",
                      statusNotice.warning
                        ? "text-amber-600 dark:text-amber-400"
                        : "text-muted-foreground",
                    )}
                    aria-label={statusNotice.title}
                  >
                    {statusNotice.icon}
                  </span>
                </TooltipTrigger>
                <TooltipContent side="bottom" align="end" className="max-w-xs space-y-1">
                  <div className="font-medium">{statusNotice.title}</div>
                  <p className="text-muted-foreground">{statusNotice.description}</p>
                  {notLoggedInHint && <p className="text-muted-foreground">{notLoggedInHint}</p>}
                </TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span className="inline-flex">
                    <Switch
                      checked={autoRestart}
                      onCheckedChange={toggleAutoRestart}
                      disabled={busy}
                      aria-label={t("dialogs.vscode.autoRestartLabel")}
                    />
                  </span>
                </TooltipTrigger>
                <TooltipContent side="bottom" align="end" className="max-w-xs space-y-1">
                  <div className="font-medium">{t("dialogs.vscode.autoRestartLabel")}</div>
                  <p className="text-muted-foreground">{t("dialogs.vscode.autoRestartTooltip")}</p>
                </TooltipContent>
              </Tooltip>
            </div>
          </div>
          <DialogDescription>{t("dialogs.vscode.description")}</DialogDescription>
        </DialogHeader>

        {busy && (
          <div className="absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg bg-background/85 backdrop-blur-sm">
            <Loader2 className="size-8 animate-spin text-primary" />
            <p className="text-sm font-medium">
              {autoClose
                ? t("dialogs.vscode.busyClosing")
                : copyCount > 0
                  ? t("dialogs.vscode.busyCopying")
                  : syncCount > 0
                    ? t("dialogs.vscode.busySyncing")
                    : t("dialogs.common.switchingAccount")}
            </p>
            <p className="max-w-xs text-center text-xs text-muted-foreground">
              {autoClose ? t("dialogs.vscode.savePromptHint") : t("dialogs.common.busyHint")}
            </p>
          </div>
        )}

        <div className="min-h-0 space-y-3 overflow-x-hidden overflow-y-auto">
          {linksAvailable ? (
            <Tabs
              value={tab}
              onValueChange={(value) => setTab(value as "links" | "copy")}
              className="flex min-h-0 flex-col gap-3 overflow-hidden"
            >
              <TabsList className="h-auto w-full shrink-0 justify-start gap-5 rounded-none border-b border-border bg-transparent p-0">
                <TabsTrigger value="links" className={TAB_TRIGGER_CLASS}>
                  {t("dialogs.common.linkedSessions")}
                  {tabCount(linksMeta?.groupCount ?? 0)}
                </TabsTrigger>
                <TabsTrigger value="copy" className={TAB_TRIGGER_CLASS}>
                  {t("dialogs.common.copySessions")}
                  {tabCount(copyCount)}
                </TabsTrigger>
              </TabsList>
              {/* forceMount：切换 tab 不得卸载另一侧，否则关联勾选会被预览重拉重置。 */}
              <TabsContent
                value="copy"
                forceMount
                className="min-h-0 space-y-3 overflow-y-auto data-[state=inactive]:hidden data-[state=active]:animate-in data-[state=active]:fade-in-0 data-[state=active]:duration-150"
              >
                {copyTabContent}
              </TabsContent>
              <TabsContent
                value="links"
                forceMount
                className="min-h-0 overflow-y-auto data-[state=inactive]:hidden data-[state=active]:animate-in data-[state=active]:fade-in-0 data-[state=active]:duration-150"
              >
                <VscodeSessionSyncSection
                  open={open}
                  account={account}
                  loggedIn={!notLoggedIn}
                  disabled={busy}
                  onChange={(state) => {
                    setSyncSelections(state.selections);
                    setSyncGroups(state.groups);
                  }}
                  onMetaChange={setLinksMeta}
                />
              </TabsContent>
            </Tabs>
          ) : (
            <div className="min-h-0 space-y-3 overflow-y-auto">{copyTabContent}</div>
          )}

          {error && (
            <Alert variant="destructive" className="min-w-0 break-all">
              <AlertDescription className="min-w-0 break-all">{error}</AlertDescription>
            </Alert>
          )}
        </div>

        <DialogFooter className="shrink-0 sm:justify-between">
          <div className="min-w-0 space-y-0.5">
            <div className="text-sm font-medium">{summaryMain}</div>
            {summarySub && <div className="text-xs text-muted-foreground">{summarySub}</div>}
          </div>
          <div className="flex shrink-0 gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
              {t("common.cancel")}
            </Button>
            {/* 勾了复制却没选会话时仍然拦住；只要还有同步项可执行就允许确认。 */}
            <Button
              onClick={doSwitch}
              disabled={busy || (copyEnabled && copyCount === 0 && syncCount === 0)}
            >
              {busy ? t("dialogs.common.switching") : t("dialogs.common.confirmSwitch")}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 统一空态文案：区分未装 VS Code / 未装扩展 / 未找到数据目录 / 未登录 / 无会话。 */
function emptyStateHint(
  status: VscodeExtStatus | null | undefined,
  loading: boolean,
  sourceUid: string | null,
  dataRoot: string | null | undefined,
  hasCopyable: boolean,
  t: Translate,
): string {
  if (loading) return t("dialogs.common.loadingSessions");
  if (status && !status.installed) return t("dialogs.vscode.emptyNoVscode");
  if (status && !status.extensionInstalled) return t("dialogs.vscode.emptyNoExtension");
  if (dataRoot === null) return t("dialogs.vscode.emptyNoDataRoot");
  if (!sourceUid) return t("dialogs.vscode.emptyNoLogin");
  if (!hasCopyable) return t("dialogs.common.noCopyableSessions");
  return t("dialogs.vscode.copyHint");
}
