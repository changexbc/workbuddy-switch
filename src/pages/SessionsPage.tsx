import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronLeft, ChevronRight, CheckCircle2, Ellipsis, Folder, Link2, Loader2, MessageCircle, Layers3, RefreshCw, SlidersHorizontal, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { CodeBuddyCnIdeMark, VscodeExtMark, WorkBuddyAiMark, WorkBuddyMark } from "@/components/product-marks";
import { DemoAction } from "@/components/demo-action";
import { GroupDetailPanel } from "@/components/session-group-detail";
import { SessionTreeList } from "@/components/session-tree";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog";
import { DialogAutoHeight } from "@/components/ui/dialog-auto-height";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { displayName } from "@/lib/account-display";
import * as api from "@/lib/api";
import { useT, type Translate } from "@/lib/i18n";
import { intlLocale } from "@/lib/i18n/format";
import type { MessageKey } from "@/lib/i18n/messages";
import type { AccountMeta, Session, SessionGroupClient, SessionGroupCurrentAccount, SessionGroupDetail, SessionGroupMemberDetail, SessionGroupSummary, SessionGroupStatus, SessionGroupUnifyPlan, SessionSyncMode, WbVariant } from "@/lib/types";
import { accountVariant, variantLabel } from "@/lib/variant";
import { useAccountsStore } from "@/stores/accounts";

const PAGE_SIZE = 8;
/** 列表不超过该数量时隐藏底部分页条：一页装得下，去掉更清爽。 */
const PAGINATION_HIDE_MAX = 4;
const CLIENT_TITLE_KEY: Record<SessionGroupClient, MessageKey> = {
  workbuddy: "sessions.common.clientWorkbuddy",
  codebuddyIde: "sessions.common.clientCodebuddyIde",
  vscodeExt: "sessions.common.clientVscodeExt",
};
const CLIENTS: { id: SessionGroupClient; subtitleKey: MessageKey }[] = [
  { id: "workbuddy", subtitleKey: "sessions.page.workbuddySubtitle" },
  { id: "vscodeExt", subtitleKey: "sessions.page.vscodeExtSubtitle" },
  { id: "codebuddyIde", subtitleKey: "sessions.page.codebuddyIdeSubtitle" },
];
const SUMMARY_STATUS_KEY: Record<SessionGroupStatus, MessageKey> = {
  latest: "sessions.status.latest",
  behind: "sessions.status.behind",
  diverge: "sessions.status.diverge",
  missing: "sessions.status.missing",
  unknown: "sessions.status.unknown",
};

export default function SessionsPage() {
  const t = useT();
  const { accounts, fetchAll } = useAccountsStore();
  const [client, setClient] = useState<SessionGroupClient>("workbuddy");
  const [variantScope, setVariantScope] = useState<WbVariant>("cn");
  const [groups, setGroups] = useState<SessionGroupSummary[]>([]);
  const [storeStatus, setStoreStatus] = useState<"missing" | "ready" | "unavailable">("missing");
  const [storeError, setStoreError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedGroupId, setSelectedGroupId] = useState<string | null>(null);
  const [detail, setDetail] = useState<SessionGroupDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [statusFilter, setStatusFilter] = useState("all");
  const [sortOrder, setSortOrder] = useState("recent");
  const [page, setPage] = useState(1);
  const [sourceMemberId, setSourceMemberId] = useState("");
  const [addTargetId, setAddTargetId] = useState("");
  const [unifyPlan, setUnifyPlan] = useState<SessionGroupUnifyPlan | null>(null);
  const [unifyLoading, setUnifyLoading] = useState<string | null>(null);
  const [currentAccounts, setCurrentAccounts] = useState<SessionGroupCurrentAccount[]>([]);
  const [actionBusy, setActionBusy] = useState(false);
  const [detailOpen, setDetailOpen] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  /** 列表卡片「… → 删除关联」的待确认目标；非空时展示确认框。 */
  const [deleteTarget, setDeleteTarget] = useState<SessionGroupSummary | null>(null);
  const [contentWidth, setContentWidth] = useState(0);
  /** 插件侧「VS Code 运行中」的确认框：确认后执行 `run`（带 restart），取消则丢弃。 */
  const [restartPrompt, setRestartPrompt] = useState<{ actionLabel: string; run: () => Promise<void> } | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const detailMode = "modal";
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const observer = new ResizeObserver(([entry]) => setContentWidth(entry.contentRect.width));
    observer.observe(root);
    return () => observer.disconnect();
  }, []);
  function closeDetail() {
    setDetailOpen(false);
    requestAnimationFrame(() => openerRef.current?.focus({ preventScroll: true }));
  }
  function openDetail(id: string) {
    openerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetail(null);
    setDetailError(null);
    setDetailLoading(true);
    setSelectedGroupId(id);
    setAddOpen(false);
    setDetailOpen(true);
  }
  /**
   * 预检：本次写入目标是否包含当前登录账号（只有涉及才需要「关闭并重开」确认框）。
   * 探测失败按「不需要」处理：后端在各写入口仍有实时判定兜底，不会误关编辑器。
   */
  async function needsEditorRestart(targetAccountIds: string[]): Promise<boolean> {
    if (client !== "vscodeExt") return false;
    try {
      return (await api.vscodeRestartPrecheck(targetAccountIds)).required === true;
    } catch {
      return false;
    }
  }
  const scope = client === "codebuddyIde" ? variantScope : undefined;
  const listRequestId = useRef(0);
  const detailRequestId = useRef(0);
  const previewRequestId = useRef(0);
  const contextRef = useRef({ client, scope, selectedGroupId, sourceMemberId });
  contextRef.current = { client, scope, selectedGroupId, sourceMemberId };
  const listContextKey = `${client}:${scope ?? "all"}`;
  const detailContextKey = `${listContextKey}:${selectedGroupId ?? "none"}`;

  useEffect(() => {
    if (!selectedGroupId) { setCurrentAccounts([]); return; }
    let live = true;
    setCurrentAccounts([]);
    const reads = client === "workbuddy"
      ? [api.getStatus("cn").then((status) => ({ variant: "cn" as const, uid: status.current?.uid, running: status.running })),
        api.getStatus("ai").then((status) => ({ variant: "ai" as const, uid: status.current?.uid, running: status.running }))]
      : client === "codebuddyIde"
        ? [api.getCodebuddyCnIdeStatus().then((status) => ({ variant: "cn" as const, accountId: status.activeAccountId })),
          api.getCodebuddyIdeStatus().then((status) => ({ variant: "ai" as const, accountId: status.activeAccountId }))]
        : [api.getVscodeExtStatus().then((status) => ({ accountId: status.activeAccountId, running: status.running }))];
    void Promise.allSettled(reads).then((results) => {
      if (live) setCurrentAccounts(results.flatMap((result) => result.status === "fulfilled" ? [result.value] : []));
    });
    return () => { live = false; };
  }, [client, selectedGroupId, detail]);

  useEffect(() => {
    if (accounts.length === 0) void fetchAll();
  }, [accounts.length, fetchAll]);

  const reloadGroups = useCallback(async () => {
    const key = `${client}:${scope ?? "all"}`;
    const requestId = ++listRequestId.current;
    setLoading(true);
    setLoadError(null);
    setStoreError(null);
    try {
      const result = await api.listSessionGroups(client, scope);
      if (`${contextRef.current.client}:${contextRef.current.scope ?? "all"}` !== key || listRequestId.current !== requestId) return;
      setGroups(result.groups);
      setStoreStatus(result.storeStatus);
      setStoreError(result.storeError ?? null);
    } catch (error) {
      if (`${contextRef.current.client}:${contextRef.current.scope ?? "all"}` !== key || listRequestId.current !== requestId) return;
      setGroups([]);
      setLoadError(api.asError(error));
    } finally {
      if (`${contextRef.current.client}:${contextRef.current.scope ?? "all"}` === key && listRequestId.current === requestId) setLoading(false);
    }
  }, [client, scope]);

  useEffect(() => {
    let live = true;
    const requestId = ++listRequestId.current;
    const key = listContextKey;
    setLoading(true);
    setLoadError(null);
    setDetail(null);
    setSelectedGroupId(null);
    previewRequestId.current += 1;
    setUnifyPlan(null);
    setUnifyLoading(null);
    api.listSessionGroups(client, scope).then((result) => {
      if (!live || listRequestId.current !== requestId || `${contextRef.current.client}:${contextRef.current.scope ?? "all"}` !== key) return;
      setGroups(result.groups);
      setStoreStatus(result.storeStatus);
      setStoreError(result.storeError ?? null);
    }).catch((error) => {
      if (!live || listRequestId.current !== requestId || `${contextRef.current.client}:${contextRef.current.scope ?? "all"}` !== key) return;
      setGroups([]);
      setLoadError(api.asError(error));
    }).finally(() => {
      if (live && listRequestId.current === requestId && `${contextRef.current.client}:${contextRef.current.scope ?? "all"}` === key) setLoading(false);
    });
    return () => { live = false; };
  }, [client, scope, listContextKey]);

  useEffect(() => {
    // 只有弹窗打开且已选中会话组才需要详情：关闭时同样进入这里，作废在途请求并停掉加载态，
    // 但保留已渲染内容给退场动画（重开时加载流程会清空）。
    if (!detailOpen || !selectedGroupId) {
      detailRequestId.current += 1;
      previewRequestId.current += 1;
      setDetailLoading(false);
      setUnifyPlan(null);
      setUnifyLoading(null);
      if (!selectedGroupId) {
        setDetail(null);
        setDetailError(null);
        setSourceMemberId("");
        setAddTargetId("");
      }
      return;
    }
    let live = true;
    const requestId = ++detailRequestId.current;
    const key = detailContextKey;
    setDetail(null);
    setDetailError(null);
    setDetailLoading(true);
    previewRequestId.current += 1;
    setUnifyPlan(null);
    setUnifyLoading(null);
    api.getSessionGroup(client, selectedGroupId, scope).then((result) => {
      if (!live || detailRequestId.current !== requestId || `${contextRef.current.client}:${contextRef.current.scope ?? "all"}:${contextRef.current.selectedGroupId ?? "none"}` !== key) return;
      setDetail(result);
      setSourceMemberId(result.safeSourceMemberId ?? "");
      setAddTargetId(result.addTargets[0]?.id ?? "");
    }).catch((error) => {
      if (!live || detailRequestId.current !== requestId || `${contextRef.current.client}:${contextRef.current.scope ?? "all"}:${contextRef.current.selectedGroupId ?? "none"}` !== key) return;
      setDetailError(api.asError(error));
    }).finally(() => {
      if (live && detailRequestId.current === requestId && `${contextRef.current.client}:${contextRef.current.scope ?? "all"}:${contextRef.current.selectedGroupId ?? "none"}` === key) setDetailLoading(false);
    });
    return () => { live = false; };
  }, [client, scope, selectedGroupId, detailContextKey, detailOpen]);

  const filteredGroups = useMemo(() => {
    const result = groups.filter((group) => {
      return statusFilter === "all" || group.summaryStatus === statusFilter;
    });
    result.sort((left, right) => sortOrder === "oldest" ? left.latestActivityAt - right.latestActivityAt : left.latestActivityAt === right.latestActivityAt ? left.title.localeCompare(right.title) : right.latestActivityAt - left.latestActivityAt);
    return result;
  }, [groups, statusFilter, sortOrder]);
  const pageCount = Math.max(1, Math.ceil(filteredGroups.length / PAGE_SIZE));
  const currentPage = Math.min(page, pageCount);
  const visibleGroups = filteredGroups.slice((currentPage - 1) * PAGE_SIZE, currentPage * PAGE_SIZE);

  useEffect(() => { setPage(1); }, [statusFilter, sortOrder, client, scope]);

  function changeClient(value: string) {
    const next = value as SessionGroupClient;
    setClient(next);
    setDetailOpen(false);
    setSelectedGroupId(null);
    setDetail(null);
    previewRequestId.current += 1;
    setUnifyPlan(null);
    setUnifyLoading(null);
  }

  async function prepareUnify(source: SessionGroupMemberDetail) {
    if (!detail || !source.canBeSource || actionBusy || unifyLoading) return;
    const requestId = ++previewRequestId.current;
    const key = detailContextKey;
    const groupId = detail.groupId;
    const targets = detail.members.filter((member) => member.linkState === "active" && member.memberId !== source.memberId);
    setUnifyPlan(null);
    setUnifyLoading(source.memberId);
    const results = await Promise.allSettled(targets.map((target) => api.previewSessionGroupPair({
      client, groupId, sourceMemberId: source.memberId, targetMemberId: target.memberId, variantScope: scope,
    })));
    if (previewRequestId.current !== requestId || `${contextRef.current.client}:${contextRef.current.scope ?? "all"}:${contextRef.current.selectedGroupId ?? "none"}` !== key) return;
    setUnifyLoading(null);
    setUnifyPlan({
      client, groupId, sourceMemberId: source.memberId, sourceName: source.accountName,
      targets: targets.map((target, index) => {
        const result = results[index];
        return { memberId: target.memberId, accountName: target.accountName,
          preview: result.status === "fulfilled" ? result.value : null,
          error: result.status === "rejected" ? api.asError(result.reason) : null };
      }),
    });
  }

  async function confirmUnify() {
    const plan = unifyPlan;
    if (!plan || !detail || plan.client !== client || plan.groupId !== detail.groupId || actionBusy) return;
    const actions = plan.targets.flatMap((target) => {
      const preview = target.preview;
      if (!preview || preview.verdict === "identical") return [];
      const mode: SessionSyncMode | undefined = ["fastForward", "overwrite", "unifyOverwrite"].find(
        (candidate) => preview.availableModes.includes(candidate as SessionSyncMode),
      ) as SessionSyncMode | undefined;
      return mode && preview.previewToken && preview.client === client && preview.groupId === plan.groupId
        && preview.sourceMemberId === plan.sourceMemberId && preview.targetMemberId === target.memberId
        ? [{ target, preview, mode }] : [];
    });
    if (plan.targets.some((target) => target.error || !target.preview
      || target.preview.client !== client || target.preview.groupId !== plan.groupId
      || target.preview.sourceMemberId !== plan.sourceMemberId || target.preview.targetMemberId !== target.memberId
      || (target.preview.verdict !== "identical" && !actions.some((action) => action.target.memberId === target.memberId)))) return;
    setUnifyPlan(null);
    setActionBusy(true);
    try {
      if ((client === "workbuddy" || client === "vscodeExt") && actions.length > 0) {
        // 插件侧：只有写入目标含当前登录账号才需要关闭/重开（统一确认框已提前告知）。
        const targetAccountIds = actions
          .map(({ target }) => detail?.members.find((member) => member.memberId === target.memberId)?.accountId)
          .filter((id): id is string => Boolean(id));
        const restart = client === "vscodeExt" && (await needsEditorRestart(targetAccountIds));
        const report = await api.syncSessionGroupUnify({
          client, groupId: plan.groupId, sourceMemberId: plan.sourceMemberId,
          targets: actions.map(({ target, preview, mode }) => ({ targetMemberId: target.memberId, previewToken: preview.previewToken!, mode })),
          ...(restart ? { restart: true } : {}),
        });
        notifyResult(t, report);
      } else {
        const combined: { synced: unknown[]; skipped: unknown[]; errors: { error: string }[]; needsRecovery: boolean } = { synced: [], skipped: [], errors: [], needsRecovery: false };
        for (const { target, preview, mode } of actions) {
          try {
            const report = await api.syncSessionGroupPair({
              client, groupId: plan.groupId, sourceMemberId: plan.sourceMemberId, targetMemberId: target.memberId,
              previewToken: preview.previewToken!, mode, variantScope: scope,
            });
            combined.synced.push(...report.synced);
            combined.skipped.push(...report.skipped);
            combined.errors.push(...report.errors);
            combined.needsRecovery ||= Boolean(report.needsRecovery);
            if (report.needsRecovery) break;
          } catch (error) {
            combined.errors.push({ error: `${target.accountName}：${api.asError(error)}` });
            break;
          }
        }
        notifyResult(t, combined);
      }
      await reloadSelectedGroup();
    } catch (error) {
      toast.error(t("sessions.page.unifyFailed"), { description: api.asError(error) });
    } finally {
      setActionBusy(false);
    }
  }

  async function syncSafeBatch() {
    if (!detail?.safeSourceMemberId || actionBusy) return;
    // 插件侧：写入目标含当前登录账号时才会被运行中的 VS Code 覆盖，先确认「关闭并重开」再执行。
    const targetAccountIds = (detail.members ?? [])
      .filter((member) => member.linkState === "active" && member.versionStatus === "behind")
      .map((member) => member.accountId)
      .filter((id): id is string => Boolean(id));
    if (await needsEditorRestart(targetAccountIds)) {
      setRestartPrompt({ actionLabel: t("sessions.page.restartCloseAndSync"), run: () => runSafeBatch(true) });
      return;
    }
    await runSafeBatch(false);
  }

  async function runSafeBatch(restart: boolean) {
    if (!detail?.safeSourceMemberId || actionBusy) return;
    setActionBusy(true);
    try {
      const report = await api.syncSessionGroupSafeBatch(client, detail.groupId, scope, restart);
      notifyResult(t, report);
      await reloadSelectedGroup();
    } catch (error) {
      toast.error(t("sessions.page.batchSyncFailed"), { description: api.asError(error) });
    } finally {
      setActionBusy(false);
    }
  }

  async function addMember() {
    if (!detail || !sourceMemberId || !addTargetId || actionBusy) return;
    if (await needsEditorRestart([addTargetId])) {
      setRestartPrompt({ actionLabel: t("sessions.page.restartCloseAndCopy"), run: () => runAddMember(true) });
      return;
    }
    await runAddMember(false);
  }

  async function runAddMember(restart: boolean) {
    if (!detail || !sourceMemberId || !addTargetId || actionBusy) return;
    setActionBusy(true);
    try {
      const report = await api.addSessionGroupMember({
        client,
        groupId: detail.groupId,
        sourceMemberId,
        targetAccountId: addTargetId,
        variantScope: scope,
        restart,
      });
      if (report.status === "linked") toast.success(t("sessions.page.addedAndLinked"), report.restartedEditor ? { description: t("sessions.page.reopenedVscode") } : undefined);
      else if (report.status === "alreadyLinked") toast.info(t("sessions.page.alreadyLinked"));
      else if (report.status === "copiedUnlinked") toast.warning(t("sessions.page.copiedNotLinked"), { description: JSON.stringify(report.linkErrors ?? []) });
      else toast.error(t("sessions.page.addFailed"));
      if (report.editorError) toast.warning(t("sessions.page.vscodeReopenFailed"), { description: report.editorError });
      await reloadSelectedGroup();
    } catch (error) {
      toast.error(t("sessions.page.addFailed"), { description: api.asError(error) });
    } finally {
      setActionBusy(false);
    }
  }

  /** 确认框「关闭并继续」：执行动作，结束后关闭确认框（失败由动作内部 toast）。 */
  async function confirmRestart() {
    const prompt = restartPrompt;
    if (!prompt || actionBusy) return;
    try {
      await prompt.run();
    } finally {
      setRestartPrompt(null);
    }
  }

  async function unlinkMember(member: SessionGroupMemberDetail) {
    if (actionBusy) {
      const error = new Error(t("sessions.page.busy"));
      toast.error(t("sessions.page.unlinkFailed"), { description: error.message });
      throw error;
    }
    if (!detail || detail.client !== client || detail.groupId !== selectedGroupId) {
      const error = new Error(t("sessions.page.groupChanged"));
      toast.error(t("sessions.page.unlinkFailed"), { description: error.message });
      throw error;
    }
    const groupId = detail.groupId;
    setActionBusy(true);
    try {
      const report = await api.unlinkSessionGroupMember({
        client,
        groupId,
        memberId: member.memberId,
        variantScope: scope,
      });
      if (report.status === "groupRemoved") {
        toast.success(t("sessions.page.groupRemoved"));
        // 组已删除，再拉详情只会得到「不存在」。关掉弹窗并只刷新列表。
        closeDetail();
        setSelectedGroupId(null);
        setDetail(null);
        setDetailError(null);
        await reloadGroups();
      } else {
        toast.success(t("sessions.page.unlinkedMember", { name: member.accountName }));
        await reloadSelectedGroup();
      }
    } catch (error) {
      toast.error(t("sessions.page.unlinkFailed"), { description: api.asError(error) });
      throw error;
    } finally {
      setActionBusy(false);
    }
  }

  /** 删除整个会话组：组内所有成员一起解除关联，只解除管理关系，不删除会话内容。 */
  async function deleteGroup() {
    const group = deleteTarget;
    if (!group || actionBusy) return;
    setActionBusy(true);
    try {
      const report = await api.deleteSessionGroup({ client, groupId: group.groupId, variantScope: scope });
      toast.success(t("sessions.page.groupDeleted", { title: group.title }), { description: t("sessions.page.groupDeletedDescription", { count: report.removed }) });
      setDeleteTarget(null);
      if (group.groupId === selectedGroupId) {
        closeDetail();
        setSelectedGroupId(null);
        setDetail(null);
        setDetailError(null);
      }
      await reloadGroups();
    } catch (error) {
      // 失败时保留确认框，方便直接重试。
      toast.error(t("sessions.page.deleteFailed"), { description: api.asError(error) });
    } finally {
      setActionBusy(false);
    }
  }

  async function reloadSelectedGroup() {
    const key = `${client}:${scope ?? "all"}:${selectedGroupId ?? "none"}`;
    // An operation may finish after the user selected another group/client.
    if (`${contextRef.current.client}:${contextRef.current.scope ?? "all"}:${contextRef.current.selectedGroupId ?? "none"}` !== key || !selectedGroupId) return;
    const requestId = ++detailRequestId.current;
    setDetailLoading(true);
    setDetailError(null);
    previewRequestId.current += 1;
    setUnifyPlan(null);
    setUnifyLoading(null);
    await reloadGroups();
    if (`${contextRef.current.client}:${contextRef.current.scope ?? "all"}:${contextRef.current.selectedGroupId ?? "none"}` !== key) return;
    if (!selectedGroupId) return;
    try {
      const next = await api.getSessionGroup(client, selectedGroupId, scope);
      if (`${contextRef.current.client}:${contextRef.current.scope ?? "all"}:${contextRef.current.selectedGroupId ?? "none"}` !== key || detailRequestId.current !== requestId) return;
      setDetail(next);
      setSourceMemberId(next.safeSourceMemberId ?? sourceMemberId);
      setAddTargetId(next.addTargets[0]?.id ?? "");
      previewRequestId.current += 1;
      setUnifyPlan(null);
      setUnifyLoading(null);
    } catch (error) {
      if (`${contextRef.current.client}:${contextRef.current.scope ?? "all"}:${contextRef.current.selectedGroupId ?? "none"}` === key && detailRequestId.current === requestId) setDetailError(api.asError(error));
    } finally {
      if (detailRequestId.current === requestId) setDetailLoading(false);
    }
  }

  const detailPanel = (
    <GroupDetailPanel
      client={client} detail={detail} sourceMemberId={sourceMemberId}
      currentAccounts={currentAccounts}
      setSourceMemberId={setSourceMemberId}
      addTargetId={addTargetId} setAddTargetId={setAddTargetId}
      unifyPlan={unifyPlan} unifyLoading={unifyLoading} setUnifyPlan={setUnifyPlan} busy={actionBusy || detailLoading || !!detailError}
      error={detailError} loading={detailLoading} onClose={closeDetail}
      onRetry={() => { setDetailError(null); void reloadSelectedGroup(); }}
      onPrepareUnify={prepareUnify} onConfirmUnify={confirmUnify} onBatchSync={syncSafeBatch} onAdd={addMember}
      onUnlinkMember={unlinkMember}
      addOpen={addOpen} setAddOpen={setAddOpen} fullPage={false}
    />
  );
  const statusOptions: [string, string][] = [
    ["all", t("sessions.status.all")], ["behind", t("sessions.status.behind")], ["diverge", t("sessions.status.diverge")], ["latest", t("sessions.status.latest")],
    ["missing", t("sessions.status.missing")], ["unknown", t("sessions.status.unknown")],
  ];
  const pages = Array.from({ length: pageCount }, (_, index) => index + 1)
    .filter((value) => value === 1 || value === pageCount || Math.abs(value - currentPage) <= 1);

  return (
    <div data-detail-mode={detailMode} className="mx-auto w-full max-w-[1180px] min-w-0 px-6 py-8 sm:px-8 sm:py-9">
      <div ref={rootRef} className="flex min-w-0 flex-col">
        <header className="mb-6">
          <div className="flex min-w-0 flex-wrap items-start justify-between gap-4">
            <div className="min-w-0">
              <h1 className="text-[28px] font-semibold tracking-tight">{t("sessions.page.title")}</h1>
              <p className="mt-2 max-w-2xl text-sm leading-6 text-muted-foreground">{t("sessions.page.description")}</p>
            </div>
            <div className="flex max-w-full flex-wrap items-center justify-end gap-2">
              <Button variant="outline" size="sm" className="shrink-0" onClick={() => void (selectedGroupId ? reloadSelectedGroup() : reloadGroups())} disabled={loading || detailLoading}><RefreshCw className={loading ? "animate-spin" : undefined} />{t("common.refresh")}</Button>
              <AddLinkedSessionDialog client={client} disabled={loading} onDone={() => void (selectedGroupId ? reloadSelectedGroup() : reloadGroups())} requestRestartConfirm={(actionLabel, run) => setRestartPrompt({ actionLabel, run })} />
            </div>
          </div>
          <Tabs value={client} onValueChange={changeClient} className="mt-4">
            <TabsList aria-label={t("sessions.page.tabsAria")} className="grid h-auto w-full grid-cols-[repeat(auto-fit,minmax(160px,1fr))] gap-2 bg-transparent p-0">
              {CLIENTS.map((item) => <TabsTrigger key={item.id} value={item.id} className="relative h-auto min-w-0 justify-start gap-3 rounded-lg border border-border bg-background px-3 py-4 text-left hover:border-brand/50 data-[state=active]:border-brand data-[state=active]:bg-brand/5 data-[state=active]:shadow-sm">
                <ClientMark client={item.id} />
                <span className="min-w-0"><span className="block truncate text-sm font-semibold text-foreground">{t(CLIENT_TITLE_KEY[item.id])}</span><span className="mt-1 block truncate text-xs font-normal text-muted-foreground">{t(item.subtitleKey)}</span></span>
                {client === item.id && <CheckCircle2 className="absolute right-2 top-2 size-3.5 text-brand" />}
              </TabsTrigger>)}
            </TabsList>
          </Tabs>
        </header>
        <section aria-labelledby="session-groups-title" className="min-w-0">
          <div className="mb-4 flex flex-wrap items-center gap-2">
            <h2 id="session-groups-title" className="text-base font-semibold tracking-tight">{t("sessions.page.groupsHeading", { client: t(CLIENT_TITLE_KEY[client]) })}</h2>
            <Badge variant="secondary" className="h-6 min-w-6 rounded-full border-0 px-1.5 text-[11px] tabular-nums text-muted-foreground shadow-none" aria-label={t("sessions.page.groupsCountAria", { count: groups.length })}>{groups.length}</Badge>
          </div>
          <div className="my-4 flex flex-wrap items-center justify-between gap-2">
            <div className="flex min-w-0 max-w-full flex-wrap items-center gap-2">
              {client === "codebuddyIde" && <Select value={variantScope} onValueChange={(value) => { setVariantScope(value as WbVariant); setSelectedGroupId(null); setDetailOpen(false); }}><SelectTrigger size="sm" className="w-32 bg-background text-xs" aria-label={t("sessions.page.variantAria")}><SelectValue /></SelectTrigger><SelectContent><SelectItem value="cn">{t("sessions.common.cnVersion")}</SelectItem><SelectItem value="ai">{t("sessions.common.intlVersion")}</SelectItem></SelectContent></Select>}
              <Tabs value={statusFilter} onValueChange={setStatusFilter} className="min-w-0 max-w-full gap-0">
                <TabsList className="h-auto max-w-full flex-wrap justify-start gap-0.5" aria-label={t("sessions.page.filterAria")}>
                  {statusOptions.filter(([value]) => ["all", "behind", "diverge", "latest"].includes(value) || value === statusFilter || groups.some((group) => group.summaryStatus === value)).map(([value, label]) => <TabsTrigger key={value} value={value} className="h-8 gap-1.5 px-2.5 hover:bg-background/60">
                    {label}<span className={`rounded-full px-1.5 text-[11px] tabular-nums ${statusFilter === value ? "bg-muted" : "bg-background/70"}`}>{value === "all" ? groups.length : groups.filter((group) => group.summaryStatus === value).length}</span>
                  </TabsTrigger>)}
                </TabsList>
              </Tabs>
            </div>
            <Select value={sortOrder} onValueChange={setSortOrder}><SelectTrigger size="sm" className="w-auto min-w-28 gap-1.5 border-0 bg-transparent px-2 text-xs text-muted-foreground shadow-none hover:bg-accent hover:text-foreground" aria-label={t("sessions.page.sortAria")}><SlidersHorizontal className="size-4" aria-hidden="true" /><SelectValue /></SelectTrigger><SelectContent><SelectItem value="recent">{t("sessions.page.sortRecent")}</SelectItem><SelectItem value="oldest">{t("sessions.page.sortOldest")}</SelectItem></SelectContent></Select>
          </div>
          {(storeStatus === "unavailable" || storeError) && <p role="status" className="mb-3 rounded-lg border border-amber-500/30 p-3 text-sm text-muted-foreground">{storeError ?? t("sessions.page.storeUnavailable")}</p>}
          {loadError && <div role="alert" className="mb-3 rounded-lg border border-destructive/30 p-3 text-sm text-destructive">{loadError}<Button variant="outline" size="sm" className="ml-2" onClick={() => void reloadGroups()}>{t("common.retry")}</Button></div>}
          {loading ? <GroupSkeleton /> : filteredGroups.length === 0 ? <div className="flex min-h-56 flex-col items-center justify-center rounded-xl border border-dashed p-6 text-center"><Layers3 className="size-7 text-muted-foreground" /><h3 className="mt-3 text-sm font-medium">{groups.length === 0 ? t("sessions.page.emptyTitle") : t("sessions.page.emptyFilteredTitle")}</h3><p className="mt-2 text-sm text-muted-foreground">{groups.length === 0 ? t("sessions.page.emptyHint") : t("sessions.page.emptyFilteredHint")}</p></div> : <div className={`grid min-w-0 gap-3 ${contentWidth >= 600 ? "grid-cols-2" : "grid-cols-1"}`}>{visibleGroups.map((group) => <SessionGroupCard key={group.key} group={group} selected={group.groupId === selectedGroupId} onSelect={() => openDetail(group.groupId)} onRequestDelete={() => setDeleteTarget(group)} />)}</div>}
          {!loading && filteredGroups.length > PAGINATION_HIDE_MAX && <div className="mt-6 flex flex-wrap items-center justify-between gap-3 text-xs text-muted-foreground"><span>{t("sessions.page.range", { start: (currentPage - 1) * PAGE_SIZE + 1, end: Math.min(currentPage * PAGE_SIZE, filteredGroups.length), total: filteredGroups.length })}</span><nav aria-label={t("sessions.page.paginationAria")} className="flex flex-wrap items-center gap-1.5"><Button variant="outline" size="icon" className="size-8" aria-label={t("sessions.page.previousPage")} disabled={currentPage <= 1} onClick={() => setPage(currentPage - 1)}><ChevronLeft /></Button>{pages.map((value, index) => <span key={value} className="flex items-center gap-1.5">{index > 0 && value - pages[index - 1] > 1 && <span>…</span>}<Button variant={value === currentPage ? "default" : "outline"} className={`size-8 p-0 ${value === currentPage ? "bg-brand text-brand-foreground hover:bg-brand/90" : ""}`} aria-label={t("sessions.page.pageNumber", { page: value })} aria-current={value === currentPage ? "page" : undefined} onClick={() => setPage(value)}>{value}</Button></span>)}<Button variant="outline" size="icon" className="size-8" aria-label={t("sessions.page.nextPage")} disabled={currentPage >= pageCount} onClick={() => setPage(currentPage + 1)}><ChevronRight /></Button></nav></div>}
        </section>
      </div>
      <Dialog open={detailOpen} onOpenChange={(open) => { if (!open) closeDetail(); }}>
        <DialogContent showCloseButton={false} aria-describedby={undefined} onCloseAutoFocus={(event) => { event.preventDefault(); if (!detailOpen) openerRef.current?.focus({ preventScroll: true }); }} className="session-detail-modal flex flex-col gap-0 overflow-hidden p-0">
          <DialogTitle className="sr-only">{t("sessions.detail.title")}</DialogTitle>
          <DialogAutoHeight>{detailPanel}</DialogAutoHeight>
        </DialogContent>
      </Dialog>
      {/* 插件侧写操作的运行中确认框：写入会被运行中的 VS Code 覆盖，需要先关闭并在结束后重开。 */}
      <AlertDialog open={!!restartPrompt} onOpenChange={(open) => { if (!open && !actionBusy) setRestartPrompt(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("sessions.page.restartTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("sessions.page.restartDescription")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          {actionBusy && <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><Loader2 className="size-4 animate-spin" />{t("sessions.page.restartBusy")}</p>}
          <AlertDialogFooter>
            <AlertDialogCancel disabled={actionBusy}>{t("common.cancel")}</AlertDialogCancel>
            <DemoAction><Button disabled={actionBusy} onClick={() => void confirmRestart()}>{actionBusy ? t("sessions.common.processing") : restartPrompt?.actionLabel ?? t("sessions.page.closeAndContinue")}</Button></DemoAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <AlertDialog open={!!deleteTarget} onOpenChange={(open) => { if (!open && !actionBusy) setDeleteTarget(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("sessions.page.deleteTitle", { title: deleteTarget?.title ?? "" })}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("sessions.page.deleteDescription", { count: deleteTarget?.memberCount ?? 0 })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={actionBusy}>{t("common.cancel")}</AlertDialogCancel>
            <DemoAction><Button variant="destructive" disabled={actionBusy} onClick={() => void deleteGroup()}>{actionBusy ? t("sessions.common.processing") : t("sessions.page.deleteConfirm")}</Button></DemoAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

function ClientMark({ client, variantScope }: { client: SessionGroupClient; variantScope?: WbVariant }) {
  if (client === "workbuddy") return variantScope === "ai" ? <WorkBuddyAiMark size={34} /> : <WorkBuddyMark size={34} />;
  if (client === "codebuddyIde") return <CodeBuddyCnIdeMark size={34} />;
  return <VscodeExtMark size={34} className="text-foreground" />;
}

/** 会话树区最小高度：加载态、空态与列表共用同一下沿，避免弹窗高度跳变。 */
const LINKED_TREE_MIN_H = "min-h-[min(10rem,30vh)]";

/**
 * 右上角「新增关联会话」弹窗（与切号弹窗同构）：来源账号 → 会话树勾选 → 目标账号，
 * 一次提交全部勾选。会话树复用 `SessionTreeList`（任务平铺 / 空间按文件夹分组 / 组头三态勾选）。
 *
 * 本次仅 WorkBuddy 客户端开放；其他客户端入口禁用（其复制 / 关联能力后续再补）。执行沿用
 * `copySessionsCross`：来源会话已属于某关联组则加入该组，未关联则新建关联组。
 */
function AddLinkedSessionDialog({ client, disabled, onDone, requestRestartConfirm }: {
  client: SessionGroupClient;
  disabled: boolean;
  onDone: () => void;
  /** 插件侧运行中：请求页面统一的「关闭并重开」确认框；用户确认后执行 `run`。 */
  requestRestartConfirm: (actionLabel: string, run: () => Promise<void>) => void;
}) {
  const t = useT();
  const accounts = useAccountsStore((state) => state.accounts);
  const [open, setOpen] = useState(false);
  const [sourceAccountId, setSourceAccountId] = useState("");
  const [targetAccountId, setTargetAccountId] = useState("");
  const [sessions, setSessions] = useState<Session[]>([]);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  const [sessionsError, setSessionsError] = useState("");
  /** 插件侧数据目录：`null` = 未找到（与「该账号无会话」区分）；`undefined` = 返回未携带该字段。 */
  const [dataRoot, setDataRoot] = useState<string | null | undefined>(undefined);
  /** 勾选 / 展开集合由调用方持有（会话树契约），执行时一次提交全部勾选。 */
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const requestId = useRef(0);
  /** 复制请求代号。关窗换代后，晚到的结果不得再关闭或改写当前这次弹窗。 */
  const copyRequestId = useRef(0);
  // 账号库就是本机 WorkBuddy 账号，国内版 / 国际版都可以作为来源或目标。
  const workbuddyAccounts = accounts;
  const sourceAccount = workbuddyAccounts.find((account) => account.id === sourceAccountId) ?? null;
  const targetAccount = workbuddyAccounts.find((account) => account.id === targetAccountId) ?? null;
  const targetOptions = workbuddyAccounts.filter((account) => account.id !== sourceAccountId);
  const supported = client === "workbuddy" || client === "vscodeExt";
  const unsupportedTitle = t("sessions.add.unsupported");
  /** 弹窗文案用：来源账号的客户端称呼。 */
  const clientAccountLabel = client === "workbuddy" ? t("sessions.add.workbuddyAccount") : t("sessions.add.vscodeAccount");

  async function loadSessions(account: AccountMeta) {
    const id = ++requestId.current;
    setSessionsLoading(true);
    setSessionsError("");
    try {
      const result = await api.listAccountSessions(account.id, client);
      if (requestId.current !== id) return;
      // 插件会话没有工作目录概念（只有 workspaceHash）：补空 cwd，交给会话树平铺展示。
      setSessions(result.sessions.map((session) => ({ ...session, cwd: session.cwd ?? "" })));
      setDataRoot(result.dataRoot);
    } catch (error) {
      if (requestId.current !== id) return;
      setSessions([]);
      setSessionsError(api.asError(error));
    } finally {
      if (requestId.current === id) setSessionsLoading(false);
    }
  }
  // 换来源账号：作废在途请求，勾选 / 展开 / 目标账号全部重来，源会话按新账号重新加载。
  function changeSource(id: string) {
    setSourceAccountId(id);
    setTargetAccountId("");
    setSessions([]);
    setSessionsError("");
    setSelected(new Set());
    setExpanded(new Set());
    requestId.current += 1;
    setSessionsLoading(false);
    const account = workbuddyAccounts.find((item) => item.id === id);
    if (account) void loadSessions(account);
  }
  function blockDismissWhileBusy(event: { preventDefault: () => void }) {
    // 遮罩写明执行中不可关闭；只藏关闭按钮挡不住 Esc 和点遮罩。
    if (busy) event.preventDefault();
  }
  function openChange(next: boolean) {
    if (!next) {
      if (busy) return;
      // 关闭时作废在途的会话读取和复制回写：晚到结果不得写回下一次打开。
      requestId.current += 1;
      copyRequestId.current += 1;
      setSessionsLoading(false);
      setOpen(false);
      return;
    }
    // 重新打开回到干净状态：来源 / 目标 / 勾选都重选，不复用上一次的会话列表。
    requestId.current += 1;
    copyRequestId.current += 1;
    setSessionsLoading(false);
    setBusy(false);
    setSourceAccountId("");
    setTargetAccountId("");
    setSessions([]);
    setSessionsError("");
    setSelected(new Set());
    setExpanded(new Set());
    setOpen(true);
  }
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
  function toggleExpanded(key: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }
  /** 预检：目标账号含当前登录账号才需要「关闭并重开」（失败按不需要处理，后端仍有实时判定兜底）。 */
  async function pluginNeedsRestart(): Promise<boolean> {
    try {
      return (await api.vscodeRestartPrecheck([targetAccountId])).required === true;
    } catch {
      return false;
    }
  }
  async function copyAndLink() {
    if (!sourceAccount || !targetAccountId || sourceAccount.id === targetAccountId || selected.size === 0 || busy) return;
    if (client === "vscodeExt") {
      // 运行中的写入会被 VS Code 覆盖：先走页面统一的确认框授权「关闭并重开」。
      if (await pluginNeedsRestart()) {
        requestRestartConfirm(t("sessions.page.restartCloseAndCopy"), () => copyAndLinkPlugin(true));
        return;
      }
      await copyAndLinkPlugin(false);
      return;
    }
    await copyAndLinkWorkbuddy();
  }
  async function copyAndLinkPlugin(restart: boolean) {
    if (!sourceAccount || !targetAccountId || sourceAccount.id === targetAccountId || selected.size === 0 || busy) return;
    const copyId = ++copyRequestId.current;
    setBusy(true);
    try {
      const report = await api.copyLinkedSessions({
        client: "vscodeExt",
        sourceAccountId: sourceAccount.id,
        targetAccountId,
        sessionIds: [...selected],
        restart,
      });
      const targetName = targetAccount ? displayName(targetAccount) : t("sessions.common.targetAccount");
      const status = String(report.status);
      const skipped = Array.isArray(report.skipped) ? report.skipped : [];
      const copiedCount = (report.report as { copied?: unknown[] } | undefined)?.copied?.length ?? 0;
      if (status === "linked") {
        toast.success(t("sessions.page.copiedToTarget", { count: copiedCount, target: targetName }), report.restartedEditor ? { description: t("sessions.page.reopenedVscode") } : undefined);
      } else if (status === "copiedUnlinked") {
        toast.warning(t("sessions.page.copiedNotLinked"), { description: JSON.stringify(report.linkErrors ?? []) });
      } else {
        toast.error(t("sessions.page.copyFailed"));
      }
      if (skipped.length > 0) toast.info(t("sessions.page.skippedMissing", { count: skipped.length }), { description: skipped.map((item) => item.error).join(t("dialogs.common.separator")) });
      if (report.editorError) toast.warning(t("sessions.page.vscodeReopenFailed"), { description: report.editorError });
      // 关窗换代后，结果已经提示过；不要把新打开的弹窗关掉，也不要清掉新一次执行的 busy。
      if (copyRequestId.current !== copyId) {
        if (status === "linked") onDone();
        return;
      }
      if (status === "linked") {
        requestId.current += 1;
        setSessionsLoading(false);
        setOpen(false);
        onDone();
      }
    } catch (error) {
      // 换代后也要报失败：请求已经发出，静默会让用户以为没执行。
      toast.error(t("sessions.page.copyFailed"), { description: api.asError(error) });
    } finally {
      if (copyRequestId.current === copyId) setBusy(false);
    }
  }
  async function copyAndLinkWorkbuddy() {
    if (!sourceAccount || !targetAccountId || sourceAccount.id === targetAccountId || selected.size === 0 || busy) return;
    const copyId = ++copyRequestId.current;
    setBusy(true);
    try {
      const report = await api.copySessionsCross(sourceAccount.id, targetAccountId, [...selected]);
      const copied = report.copied?.length ?? 0;
      const alreadyLinked = report.alreadyLinked?.length ?? 0;
      const errors = report.errors ?? [];
      const targetName = targetAccount ? displayName(targetAccount) : t("sessions.common.targetAccount");
      // 同一份报告可能同时含已复制、已存在与失败项：各分支各自提示，互不隐藏。
      if (copied > 0) toast.success(t("sessions.page.copiedToTarget", { count: copied, target: targetName }));
      if (alreadyLinked > 0) toast.info(t("sessions.page.alreadyCopied", { target: targetName, count: alreadyLinked }));
      if (errors.length > 0) {
        const allFailed = copied === 0 && alreadyLinked === 0;
        toast.error(allFailed ? t("sessions.page.copyFailedShort") : t("sessions.page.copyPartlyFailed"), {
          description: errors.map((item) => t("sessions.common.errorEntry", { name: sessions.find((session) => session.id === item.id)?.title || item.id, error: item.error })).join(t("dialogs.common.separator")),
        });
      }
      if (report.needsRecovery) toast.error(t("sessions.page.needsRecovery"), { description: t("sessions.page.needsRecoveryKeep") });
      if (copied === 0 && alreadyLinked === 0 && errors.length === 0 && !report.needsRecovery) {
        toast.error(t("sessions.page.copyFailed"), report.error ? { description: report.error } : undefined);
      }
      // 关窗换代后，结果已经提示过；不要把新打开的弹窗关掉，也不要清掉新一次执行的 busy。
      if (copyRequestId.current !== copyId) {
        if (copied > 0 || alreadyLinked > 0) onDone();
        return;
      }
      // 有成功复制，或没有 errors 的已存在副本：关闭并刷新。纯失败留在弹窗。
      if (copied > 0 || (alreadyLinked > 0 && errors.length === 0)) {
        requestId.current += 1;
        setSessionsLoading(false);
        setOpen(false);
        onDone();
      }
    } catch (error) {
      // 换代后也要报失败：请求已经发出，静默会让用户以为没执行。
      toast.error(t("sessions.page.copyFailed"), { description: api.asError(error) });
    } finally {
      if (copyRequestId.current === copyId) setBusy(false);
    }
  }
  // 页脚摘要固定带目标账号名：摘要随勾选数与目标变化，执行前就能确认去向。
  const summarySub = !sourceAccount
    ? t("sessions.add.summarySelectSource")
    : !targetAccount
      ? t("sessions.add.summarySelectTarget")
      : selected.size > 0
        ? t("sessions.add.summaryWillCopy", { target: displayName(targetAccount) })
        : t("sessions.add.summaryTickSessions", { target: displayName(targetAccount) });
  return <Dialog open={open} onOpenChange={openChange}>
    <span className="inline-flex" title={supported ? undefined : unsupportedTitle}>
      <DialogTrigger asChild>
        <Button size="sm" className="shrink-0 bg-brand text-brand-foreground hover:bg-brand/90" disabled={disabled || !supported} title={supported ? t("sessions.add.title") : unsupportedTitle}><Link2 />{t("sessions.add.title")}</Button>
      </DialogTrigger>
    </span>
    <DialogContent showCloseButton={!busy} onEscapeKeyDown={blockDismissWhileBusy} onPointerDownOutside={blockDismissWhileBusy} onInteractOutside={blockDismissWhileBusy} className="flex max-h-[min(90vh,calc(100vh-2rem))] min-w-0 flex-col overflow-hidden">
      <DialogHeader className="shrink-0">
        <DialogTitle>{t("sessions.add.title")}</DialogTitle>
        <DialogDescription>{t("sessions.add.description")}</DialogDescription>
      </DialogHeader>
      {busy && <div className="absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg bg-background/85 backdrop-blur-sm">
        <Loader2 className="size-8 animate-spin text-primary" />
        <p className="text-sm font-medium">{t("sessions.add.copyingTitle")}</p>
        <p className="max-w-xs text-center text-xs text-muted-foreground">{t("sessions.add.copyingHint")}</p>
      </div>}
      <div className="flex min-h-0 flex-col gap-3 overflow-x-hidden overflow-y-auto">
        <div className="min-w-0 space-y-1.5"><p className="text-xs font-medium">{t("sessions.add.sourceAccount")}</p>
          <Select value={sourceAccountId} onValueChange={changeSource} disabled={busy}>
            <SelectTrigger size="sm" className="w-full min-w-0 bg-background text-xs" aria-label={t("sessions.add.sourceAccountAria")}><SelectValue placeholder={t("sessions.add.sourcePlaceholder", { client: clientAccountLabel })} /></SelectTrigger>
            <SelectContent>{workbuddyAccounts.map((account) => <SelectItem key={account.id} value={account.id} className="text-xs">{displayName(account)} · {variantLabel(accountVariant(account))}</SelectItem>)}</SelectContent>
          </Select>
          {workbuddyAccounts.length === 0 && <p className="text-xs text-muted-foreground">{t("sessions.add.noAccounts", { client: clientAccountLabel })}</p>}
        </div>
        <div className="min-w-0 space-y-1.5"><p className="text-xs font-medium">{t("sessions.add.sourceSessions")}</p>
          {!sourceAccountId
            ? <p className={`flex items-center justify-center px-3 text-center text-sm text-muted-foreground ${LINKED_TREE_MIN_H}`}>{t("sessions.add.selectSourceFirst")}</p>
            : sessionsLoading
              ? <div className={`flex items-center justify-center gap-2 text-sm text-muted-foreground ${LINKED_TREE_MIN_H}`}><Loader2 className="animate-spin" />{t("sessions.add.loadingSessions")}</div>
              : sessionsError
                ? <div role="alert" className={`flex flex-col items-center justify-center gap-2 px-3 text-center ${LINKED_TREE_MIN_H}`}><p className="text-sm text-destructive">{sessionsError}</p><Button variant="outline" size="sm" onClick={() => { if (sourceAccount) void loadSessions(sourceAccount); }}>{t("common.retry")}</Button></div>
                : sessions.length === 0
                  ? <p className={`flex items-center justify-center px-3 text-center text-sm text-muted-foreground ${LINKED_TREE_MIN_H}`}>{client === "vscodeExt" && dataRoot === null ? t("sessions.add.noPluginDataRoot") : t("sessions.add.noSessions")}</p>
                  : <SessionTreeList
                    sessions={sessions}
                    selected={selected}
                    expanded={expanded}
                    onToggleSession={toggleSession}
                    onToggleGroup={toggleGroup}
                    onToggleExpanded={toggleExpanded}
                    className={`max-h-[min(22rem,45vh)] overflow-y-auto pr-1 ${LINKED_TREE_MIN_H}`}
                  />}
        </div>
        <div className="min-w-0 space-y-1.5"><p className="text-xs font-medium">{t("sessions.common.targetAccount")}</p>
          <Select value={targetAccountId} onValueChange={setTargetAccountId} disabled={busy || !sourceAccountId || targetOptions.length === 0}>
            <SelectTrigger size="sm" className="w-full min-w-0 bg-background text-xs" aria-label={t("sessions.add.targetAccountAria")}><SelectValue placeholder={sourceAccountId ? t("sessions.add.targetPlaceholder") : t("sessions.add.summarySelectSource")} /></SelectTrigger>
            <SelectContent>{targetOptions.map((account) => <SelectItem key={account.id} value={account.id} className="text-xs">{displayName(account)} · {variantLabel(accountVariant(account))}</SelectItem>)}</SelectContent>
          </Select>
        </div>
      </div>
      <DialogFooter className="shrink-0 sm:justify-between">
        <div className="min-w-0 space-y-0.5">
          <div className="text-sm font-medium">{t("sessions.add.selectedCount", { count: selected.size })}</div>
          <div className="text-xs text-muted-foreground">{summarySub}</div>
        </div>
        <div className="flex shrink-0 gap-2">
          <Button variant="outline" onClick={() => openChange(false)} disabled={busy}>{t("common.cancel")}</Button>
          <DemoAction><Button onClick={() => void copyAndLink()} disabled={busy || !sourceAccount || !targetAccountId || sourceAccount.id === targetAccountId || selected.size === 0}>{busy ? <Loader2 className="animate-spin" /> : <Link2 />}{busy ? t("sessions.common.processing") : t("sessions.common.copyAndLink")}</Button></DemoAction>
        </div>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}

function GroupSkeleton() {
  const t = useT();
  return <div className="grid min-w-0 grid-cols-1 gap-3 sm:grid-cols-2" aria-label={t("sessions.page.loadingGroupsAria")}>
    {Array.from({ length: 4 }, (_, index) => <Card key={index} className="gap-3 rounded-xl py-3 shadow-none"><CardContent className="space-y-3 px-3"><Skeleton className="h-4 w-2/3" /><Skeleton className="h-3 w-1/2" /><Skeleton className="h-7 w-full" /></CardContent></Card>)}
  </div>;
}

function SessionGroupCard({ group, selected, onSelect, onRequestDelete }: { group: SessionGroupSummary; selected: boolean; onSelect: () => void; onRequestDelete: () => void }) {
  const t = useT();
  // 「…」不能嵌在整卡按钮里（button 不能嵌套 button），所以绝对定位到卡片右上角。
  return <div className="relative min-w-0">
    <button type="button" onClick={onSelect} aria-pressed={selected} className={`w-full min-w-0 cursor-pointer rounded-lg border bg-card px-4 py-3.5 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${selected ? "border-brand shadow-sm" : "border-border hover:border-brand/50"}`}>
      <div className="flex min-w-0 items-center justify-between gap-2 pr-7 text-xs text-muted-foreground"><span className="flex min-w-0 items-center gap-2"><Folder className="size-4 shrink-0" /><span className="truncate" title={group.projectLabel}>{group.projectLabel || t("sessions.group.noProject")}</span></span><span className="shrink-0" title={formatDate(group.latestActivityAt)}>{relativeDate(t, group.latestActivityAt)}</span></div>
      <h3 className="my-3 truncate text-base font-semibold" title={group.title}>{group.title}</h3>
      <div className="flex flex-wrap items-center justify-between gap-2 text-sm text-muted-foreground"><span className="flex items-center gap-2" title={group.accountNames.join(t("sessions.common.listSeparator"))}><MessageCircle className="size-4" />{t("sessions.group.accountCount", { count: group.memberCount })}</span><span className="flex min-w-0 items-center gap-2 text-xs"><span className={`size-2 shrink-0 rounded-full ${group.summaryStatus === "latest" ? "bg-brand" : group.summaryStatus === "behind" ? "bg-amber-500" : group.summaryStatus === "diverge" ? "bg-destructive" : "bg-muted-foreground"}`} /><span className="truncate" title={group.summaryText}>{t(SUMMARY_STATUS_KEY[group.summaryStatus])}</span></span></div>
    </button>
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="absolute right-2 top-2.5 size-6 text-muted-foreground hover:text-foreground" aria-label={t("sessions.group.actionsAria", { title: group.title })} title={t("sessions.group.moreActions")}><Ellipsis className="size-3.5" /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-36">
        <DropdownMenuItem onSelect={onRequestDelete}><Trash2 />{t("sessions.group.deleteLink")}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  </div>;
}

function relativeDate(t: Translate, timestamp: number): string {
  if (!timestamp) return "—";
  const minutes = Math.floor((Date.now() - timestamp) / 60000);
  if (minutes < 1) return t("sessions.relative.justNow");
  if (minutes < 60) return t("sessions.relative.minutesAgo", { count: minutes });
  if (minutes < 1440) return t("sessions.relative.hoursAgo", { count: Math.floor(minutes / 60) });
  return formatDate(timestamp);
}

function formatDate(timestamp: number): string {
  if (!timestamp) return "—";
  return new Intl.DateTimeFormat(intlLocale(), { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }).format(timestamp);
}

function notifyResult(t: Translate, report: { synced?: unknown[]; skipped?: unknown[]; errors?: { error: string }[]; needsRecovery?: boolean; temporaryFiles?: { reason: string }[]; restartedVariants?: WbVariant[]; restartedEditor?: boolean; editorError?: string }, targetName?: string) {
  const synced = report.synced?.length ?? 0;
  const skipped = report.skipped?.length ?? 0;
  const errors = report.errors ?? [];
  const done = report.restartedVariants?.length
    ? t("sessions.page.syncDoneVariants", { count: synced, variants: report.restartedVariants.map(variantLabel).join(t("sessions.common.listSeparator")) })
    : report.restartedEditor ? t("sessions.page.syncDoneEditor", { count: synced }) : t("sessions.page.syncDone", { count: synced });
  if (synced > 0) toast.success(targetName ? t("sessions.page.syncedTo", { target: targetName }) : t("sessions.page.syncedCount", { count: synced }), { description: done });
  if (skipped > 0) toast.warning(t("sessions.page.skippedCount", { count: skipped }), { description: t("sessions.page.skippedDescription") });
  if (errors.length > 0) toast.error(t("sessions.page.syncPartlyFailed"), { description: errors.map((item) => item.error).join(t("dialogs.common.separator")) });
  if (report.needsRecovery) toast.error(t("sessions.page.needsRecovery"), { description: report.temporaryFiles?.map((item) => item.reason).join(t("dialogs.common.separator")) || t("sessions.page.needsRecoveryKeep") });
  if (report.editorError) toast.warning(t("sessions.page.vscodeReopenFailed"), { description: report.editorError });
  if (synced === 0 && skipped === 0 && errors.length === 0 && !report.needsRecovery) toast.info(t("sessions.page.nothingToSync"));
}
