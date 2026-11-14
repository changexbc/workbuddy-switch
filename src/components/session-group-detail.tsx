import { useLayoutEffect, useRef, useState } from "react";
import { ArrowRight, Check, ChevronDown, ChevronLeft, Copy, Ellipsis, FileText, Folder, Info, Link2, Loader2, Plus, RefreshCw, TriangleAlert, Unlink, X } from "lucide-react";
import { DemoAction } from "@/components/demo-action";
import { CodeBuddyAiIdeMark, CodeBuddyCnIdeMark, VscodeExtAiMark, VscodeExtMark, WorkBuddyAiMark, WorkBuddyMark } from "@/components/product-marks";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Popover, PopoverArrow, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { displayName } from "@/lib/account-display";
import { useT, type Translate } from "@/lib/i18n";
import { intlLocale } from "@/lib/i18n/format";
import type { MessageKey } from "@/lib/i18n/messages";
import { accountVariant, variantIsIntl, variantLabel } from "@/lib/variant";
import type { SessionGroupClient, SessionGroupCurrentAccount, SessionGroupDetail, SessionGroupMemberDetail, SessionGroupUnifyPlan } from "@/lib/types";
import "./session-group-detail.css";

export interface GroupDetailPanelProps {
  client: SessionGroupClient;
  detail: SessionGroupDetail | null;
  currentAccounts: SessionGroupCurrentAccount[];
  sourceMemberId: string;
  setSourceMemberId: (id: string) => void;
  addTargetId: string;
  setAddTargetId: (id: string) => void;
  unifyPlan: SessionGroupUnifyPlan | null;
  unifyLoading: string | null;
  setUnifyPlan: (plan: SessionGroupUnifyPlan | null) => void;
  busy: boolean;
  error: string | null;
  loading: boolean;
  onClose: () => void;
  onRetry: () => void;
  onPrepareUnify: (member: SessionGroupMemberDetail) => void;
  onConfirmUnify: () => Promise<void>;
  onBatchSync: () => void;
  onAdd: () => Promise<void>;
  onUnlinkMember: (member: SessionGroupMemberDetail) => Promise<void>;
  addOpen: boolean;
  setAddOpen: (open: boolean) => void;
  fullPage: boolean;
}

const CLIENT_TITLE_KEY: Record<SessionGroupClient, MessageKey> = {
  workbuddy: "sessions.common.clientWorkbuddy", codebuddyIde: "sessions.common.clientCodebuddyIde", vscodeExt: "sessions.common.clientVscodeExt",
};

type GraphLine = { path: string; tone: "default" | "common" | `branch-${number}`; points: { x: number; y: number }[] };

export function GroupDetailPanel(props: GroupDetailPanelProps) {
  const t = useT();
  const { detail } = props;
  // 只存被点成员 id，确认框的成员由当前 detail 派生：浮层不会停留在过期数据上。
  const [unlinkTargetId, setUnlinkTargetId] = useState<string | null>(null);
  const [unlinkBusy, setUnlinkBusy] = useState(false);
  const unlinkTarget = detail?.members.find((member) => member.memberId === unlinkTargetId) ?? null;
  const graphRef = useRef<HTMLDivElement>(null);
  const [connections, setConnections] = useState<GraphLine[]>([]);
  useLayoutEffect(() => {
    const graph = graphRef.current;
    if (!graph) return;
    function measure() {
      if (!graph) return;
      const svg = graph.querySelector("svg");
      const matrix = svg?.getScreenCTM();
      if (!matrix) return;
      // 屏幕坐标包含弹窗缩放；还原到 SVG 坐标，连线才不会被二次缩放。
      const inverse = matrix.inverse();
      const toLocal = (x: number, y: number) => new DOMPoint(x, y).matrixTransform(inverse);
      const hub = graph.querySelector<HTMLElement>("[data-session-hub]")?.getBoundingClientRect();
      if (!hub) return;
      const nodes = [...graph.querySelectorAll<HTMLElement>("[data-session-member]")];
      const lineage = detail?.summaryStatus === "diverge" && detail.divergence;
      const hasLineage = !!lineage && nodes.every((node) => node.dataset.graphRole === "common" || node.dataset.graphRole === "branch");
      const leftCount = hasLineage ? nodes.filter((node) => node.dataset.graphRole === "common").length : Math.ceil(nodes.length / 2);
      const next = nodes.map((node, index) => {
        const box = node.getBoundingClientRect();
        const left = index < leftCount;
        const sideCount = left ? leftCount : nodes.length - leftCount;
        const sideIndex = left ? index : index - leftCount;
        const { x, y } = toLocal(left ? box.right : box.left, box.top + box.height / 2);
        const { x: hx, y: hy } = toLocal(left ? hub.left : hub.right, hub.top + hub.height * ((sideIndex + 1) / (sideCount + 1)));
        const mid = (x + hx) / 2;
        const dx = Math.sign(hx - x);
        const dy = Math.sign(hy - y);
        const radius = Math.min(12, Math.abs(hy - y) / 2, Math.abs(hx - x) / 4);
        const path = hasLineage && Math.abs(hy - y) > 1
          ? `M ${x} ${y} C ${mid} ${y} ${mid} ${hy} ${hx} ${hy}`
          : radius < 1 ? `M ${x} ${y} H ${hx}`
            : `M ${x} ${y} H ${mid - dx * radius} Q ${mid} ${y} ${mid} ${y + dy * radius} V ${hy - dy * radius} Q ${mid} ${hy} ${mid + dx * radius} ${hy} H ${hx}`;
        const tone = hasLineage ? left ? "common" as const : `branch-${Number(node.dataset.branchIndex ?? 0) % 5}` as const : "default" as const;
        return { path, tone, points: [{ x, y }, { x: hx, y: hy }] };
      });
      setConnections(next);
    }
    const observer = new ResizeObserver(measure);
    observer.observe(graph);
    graph.querySelectorAll<HTMLElement>("[data-session-member], [data-session-hub]").forEach((node) => observer.observe(node));
    measure();
    return () => observer.disconnect();
  }, [detail]);

  const active = detail?.members.filter((member) => member.linkState === "active") ?? [];
  const equal = detail?.summaryStatus === "latest" && active.length > 0;
  const sourceOptions = active.filter((member) => member.canBeSource);
  const selectedSource = sourceOptions.find((member) => member.memberId === props.sourceMemberId);
  // Batch sync chooses its own verified source on the backend, independently of the copy form.
  const batchSource = active.find((member) => member.memberId === detail?.safeSourceMemberId);
  const behind = active.filter((member) => member.versionStatus === "behind");
  const canBatch = !!batchSource && behind.length > 0;
  const conflict = detail?.summaryStatus === "diverge";
  const divergence = conflict ? detail?.divergence : undefined;
  const commonMemberIds = new Set(divergence?.commonMemberIds ?? []);
  const branchOf = new Map(divergence?.branches.flatMap((branch, index) => branch.map((memberId) => [memberId, index] as const)) ?? []);
  const displayMembers = divergence
    ? [...detail!.members].sort((left, right) => {
      const rank = (member: SessionGroupMemberDetail) => commonMemberIds.has(member.memberId) ? 0 : branchOf.has(member.memberId) ? 1 : 2;
      return rank(left) - rank(right);
    })
    : detail?.members ?? [];
  const blockedTargets = props.unifyPlan?.targets.flatMap((target) => {
    const preview = target.preview;
    const issue = target.error || !preview ? target.error ?? t("sessions.detail.issueNoPreview")
      : preview.client !== props.unifyPlan?.client || preview.groupId !== props.unifyPlan.groupId
        || preview.sourceMemberId !== props.unifyPlan.sourceMemberId || preview.targetMemberId !== target.memberId
        ? t("sessions.detail.issueMismatch")
        : preview.verdict !== "identical" && (!preview.previewToken || !preview.availableModes.some((mode) => ["fastForward", "overwrite", "unifyOverwrite"].includes(mode)))
          ? preview.reason || t("sessions.detail.issueUnsafe")
          : null;
    return issue ? [{ ...target, issue }] : [];
  }) ?? [];
  const changedTargets = props.unifyPlan?.targets.filter((target) => target.preview?.verdict !== "identical") ?? [];
  const overwrittenTargets = props.unifyPlan?.targets.filter((target) => target.preview?.availableModes.some((mode) => mode === "overwrite" || mode === "unifyOverwrite")) ?? [];
  const runningVariants = props.currentAccounts.flatMap((current) => current.running && current.variant ? [current.variant] : []);
  const restartVariants = runningVariants.filter((variant) => props.unifyPlan?.targets.some((target) => {
    if (target.preview?.verdict === "identical") return false;
    const member = detail?.members.find((item) => item.memberId === target.memberId);
    const current = props.currentAccounts.find((item) => item.variant === variant);
    return member?.variant === variant && (!current?.uid || current.uid === member.uid || current.uid === member.accountId);
  }));
  /** 插件侧：VS Code 是否正在运行（详情加载时的状态；执行前后端会再次核对实际状态）。 */
  const editorRunning = props.client === "vscodeExt" && props.currentAccounts.some((current) => current.running === true);
  /** 统一确认框的运行提示：WorkBuddy 按档位、插件按编辑器分别措辞；其他客户端不显示。 */
  const restartNotice = props.client === "workbuddy"
    ? {
        title: runningVariants.length ? t("sessions.detail.restartNotice.runningTitle", { variants: runningVariants.map(variantLabel).join(t("sessions.common.listSeparator")) }) : t("sessions.detail.restartNotice.idleTitle"),
        description: restartVariants.length
          ? t("sessions.detail.restartNotice.workbuddyWillRestart", { variants: restartVariants.map(variantLabel).join(t("sessions.common.listSeparator")) })
          : t("sessions.detail.restartNotice.workbuddyMayRestart"),
        willRestart: restartVariants.length > 0,
      }
    : props.client === "vscodeExt"
      ? {
          title: editorRunning ? t("sessions.detail.restartNotice.vscodeRunningTitle") : t("sessions.detail.restartNotice.vscodeIdleTitle"),
          description: editorRunning ? t("sessions.detail.restartNotice.vscodeWillRestart") : t("sessions.detail.restartNotice.vscodeMayRestart"),
          willRestart: editorRunning,
        }
      : null;
  const summary = equal
    ? t("sessions.detail.summaryEqual", { count: active.length })
    : canBatch ? t("sessions.detail.summaryBehind", { count: behind.length })
    : conflict && divergence ? t("sessions.detail.summaryDivergeBranches", { common: divergence.commonMemberIds.length, branches: divergence.branches.length })
    : conflict ? t("sessions.detail.summaryDiverge", { count: active.length })
    : detail?.summaryStatus === "missing" ? t("sessions.detail.summaryMissing") : t("sessions.detail.summaryUnknown");
  // Close the popover after the page handler settled; it resolves on failure too and keeps the toast.
  async function completeAdd() {
    await props.onAdd();
    props.setAddOpen(false);
  }
  function openAdd() {
    props.setAddOpen(true);
  }
  // 确认框成员来自当前 detail。成功后父层刷新会让成员消失；失败时父层 toast 并 reject，框留着可重试。
  async function completeUnlink() {
    if (!unlinkTarget || unlinkBusy) return;
    setUnlinkBusy(true);
    try {
      await props.onUnlinkMember(unlinkTarget);
      setUnlinkTargetId(null);
    } catch {
      // 父层已 toast。
    } finally {
      setUnlinkBusy(false);
    }
  }
  function sourceSelect(label: string) {
    return <Select value={selectedSource?.memberId ?? ""} onValueChange={props.setSourceMemberId} disabled={props.busy || sourceOptions.length === 0}>
      <SelectTrigger size="sm" className="w-full min-w-0 bg-background text-xs" aria-label={label}><SelectValue placeholder={t("sessions.detail.sourcePlaceholder")} /></SelectTrigger>
      <SelectContent>{sourceOptions.map((member) => <SelectItem key={member.memberId} value={member.memberId} className="text-xs">{member.accountName} · {variantLabel(member.variant)}</SelectItem>)}</SelectContent>
    </Select>;
  }

  return <section aria-label={t("sessions.detail.groupAria")} className="session-relationship flex h-full min-h-0 min-w-0 flex-col bg-card">
    <div className="flex shrink-0 items-center justify-between px-4 py-2.5">
      <span className="text-xs font-medium text-muted-foreground">{t("sessions.detail.title")}</span>
      <Button variant="ghost" size={props.fullPage ? "sm" : "icon"} className={props.fullPage ? "h-7 text-xs" : "size-7"} aria-label={props.fullPage ? t("sessions.detail.backToSessions") : t("sessions.detail.close")} onClick={props.onClose}>
        {props.fullPage ? <><ChevronLeft className="size-3.5" />{t("sessions.detail.backToSessions")}</> : <X className="size-4" />}
      </Button>
    </div>
    <div data-detail-scroll className="min-h-0 flex-1 overflow-y-auto px-4 pb-3">
      {props.error && <div role="alert" className="mb-3 space-y-1.5 rounded-lg border border-destructive/30 p-2.5 text-xs text-destructive">
        <p className="break-words">{props.error}</p><Button variant="outline" size="sm" className="h-7 text-xs" disabled={props.loading} onClick={props.onRetry}>{t("common.retry")}</Button>
      </div>}
      {!detail && !props.error && <div aria-label={t("sessions.detail.loadingAria")} aria-busy="true" className="space-y-3">
        <Skeleton className="h-6 w-3/4" /><Skeleton className="h-5 w-28" /><Skeleton className="h-12 w-full" /><Skeleton className="h-5 w-32" /><Skeleton className="h-52 w-full" />
      </div>}
      {detail && <>
        <h2 className="session-detail-title break-words text-lg font-semibold leading-snug tracking-tight">{detail.title}</h2>
        <div className="mt-2 flex flex-wrap items-center gap-x-2.5 gap-y-1">
          <Badge variant="success">{t(CLIENT_TITLE_KEY[props.client])}</Badge>
          <span className="flex min-w-0 items-center gap-1 text-[11px] text-muted-foreground"><Folder className="size-3 shrink-0" /><span className="truncate" title={detail.projectLabel}>{detail.projectLabel || t("sessions.group.noProject")}</span></span>
        </div>
        <Alert variant={conflict ? "destructive" : canBatch ? "warning" : equal ? "success" : "default"} className={`my-3 min-w-0 items-center py-2.5 pr-11 [&>svg]:translate-y-0 ${conflict ? "border-destructive/35 bg-destructive/10 text-destructive dark:text-red-300" : canBatch ? "dark:text-amber-200" : equal ? "" : "bg-muted/70"}`} data-relationship-summary>
          {conflict ? <TriangleAlert aria-hidden="true" /> : equal ? <Check aria-hidden="true" /> : <Info aria-hidden="true" />}
          <AlertTitle className="text-xs leading-4">{summary}</AlertTitle>
          {conflict && <AlertDescription className="text-[11px] leading-4 text-destructive/85 dark:text-red-200/80">{divergence ? t("sessions.detail.conflictHintDiverge") : t("sessions.detail.conflictHint")}</AlertDescription>}
          {equal && active[0]?.recordCount != null && <AlertDescription className="text-[11px] leading-4">{t("sessions.detail.equalRecords", { count: active[0].recordCount })}</AlertDescription>}
          <Button variant="ghost" size="icon" className={`absolute right-2 top-1 size-7 ${conflict ? "hover:bg-destructive/10" : canBatch ? "hover:bg-amber-500/10" : "hover:bg-emerald-500/10"}`} aria-label={t("sessions.detail.recheckAria")} title={t("sessions.detail.recheck")} disabled={props.loading || props.busy} onClick={props.onRetry}><RefreshCw className={`size-3.5 ${props.loading ? "animate-spin" : ""}`} /></Button>
        </Alert>
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
          <div><h3 className="text-sm font-semibold">{divergence ? t("sessions.detail.branchGraph") : t("sessions.detail.linkGraph")}</h3><p className="mt-0.5 text-[11px] leading-4 text-muted-foreground">{divergence ? t("sessions.detail.branchGraphHint", { count: divergence.branches.length }) : t("sessions.detail.linkGraphHint")}</p></div>
          <span className="rounded-full bg-muted px-2 py-0.5 text-[11px] tabular-nums text-muted-foreground">{t("sessions.detail.accountCountBadge", { count: detail.members.length })}</span>
        </div>
        <div className="relationship-canvas rounded-xl border border-brand/10 bg-brand/5 p-3">
          {divergence && <div className="relationship-group-labels mb-2 flex items-center justify-between gap-3 text-[11px] leading-4">
            <span className="rounded-md bg-amber-500/10 px-2 py-1 font-medium text-amber-800 dark:text-amber-200">{t("sessions.detail.commonOldVersionCount", { count: divergence.commonMemberIds.length })}</span>
            <ArrowRight className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
            <span className="rounded-md bg-destructive/10 px-2 py-1 font-medium text-destructive">{t("sessions.detail.branchCount", { count: divergence.branches.length })}</span>
          </div>}
          <div ref={graphRef} className="relationship-graph" data-lineage={!!divergence}>
          <svg className="relationship-connections" aria-hidden="true">{connections.map((line, index) => <g key={index} data-line-tone={line.tone}><path d={line.path} />{line.points.map((point, pointIndex) => <circle key={pointIndex} cx={point.x} cy={point.y} r="3.5" />)}</g>)}</svg>
          <div className="relationship-root" data-session-hub>
            <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-brand/25 bg-brand/10 text-brand"><FileText className="size-5" /></span>
            <div><p className="text-sm font-semibold">{divergence ? t("sessions.detail.commonOldVersion") : t("sessions.detail.sameSession")}</p><p className="mt-0.5 text-[11px] text-muted-foreground">{divergence ? t("sessions.detail.commonContentCount", { count: divergence.commonMemberIds.length }) : t("sessions.detail.linkedAccountCount", { count: detail.members.length })}</p></div>{equal && <Badge variant="success">{t("sessions.detail.identicalBadge")}</Badge>}
          </div>
          <ul className="relationship-members" aria-label={t("sessions.detail.membersAria")}>
            {displayMembers.map((member, index) => {
              const isActive = member.linkState === "active";
              const isCurrent = isActive && props.currentAccounts.some((current) =>
                (!current.variant || current.variant === member.variant)
                && ((current.uid && (current.uid === member.uid || current.uid === member.accountId))
                  || (current.accountId && current.accountId === member.accountId)),
              );
              const status = member.linkState === "active" ? member.versionStatus : member.linkState;
              const branchIndex = branchOf.get(member.memberId);
              const graphRole = divergence ? commonMemberIds.has(member.memberId) ? "common" : branchIndex != null ? "branch" : undefined : undefined;
              const commonCount = divergence?.commonMemberIds.length ?? Math.ceil(displayMembers.length / 2);
              return <li key={member.memberId} className="relationship-branch" style={{ gridColumn: index < commonCount ? 1 : 3, gridRow: index < commonCount ? index + 1 : index - commonCount + 1 }} data-session-member={member.memberId} data-link-state={member.linkState} data-graph-role={graphRole} data-branch-index={branchIndex}>
                  <article className={`relative min-w-0 rounded-lg border bg-card p-2.5 shadow-xs ${isActive ? "border-border" : "border-dashed border-border text-muted-foreground"}`}>
                    <div className="flex min-w-0 items-start justify-between gap-2">
                      <h4 className="min-w-0 break-words text-sm font-semibold [overflow-wrap:anywhere]">{member.accountName}</h4>
                      <div className="flex shrink-0 items-center gap-0.5">
                        <MemberVariantMark client={props.client} variant={member.variant} isCurrent={isCurrent} />
                        <DropdownMenu>
                          <DropdownMenuTrigger asChild>
                            <Button variant="ghost" size="icon" className="size-6 text-muted-foreground hover:text-foreground" aria-label={t("sessions.detail.memberActionsAria", { name: member.accountName })} title={t("sessions.group.moreActions")}><Ellipsis className="size-3.5" /></Button>
                          </DropdownMenuTrigger>
                          <DropdownMenuContent align="end" className="w-36">
                            <DropdownMenuItem onSelect={() => setUnlinkTargetId(member.memberId)}><Unlink />{t("sessions.detail.unlink")}</DropdownMenuItem>
                          </DropdownMenuContent>
                        </DropdownMenu>
                      </div>
                    </div>
                    <div className="mt-1 flex flex-wrap items-center justify-between gap-1.5"><span className="text-xs tabular-nums text-muted-foreground">{member.recordCount == null ? t("sessions.detail.recordCountUnknown") : t("sessions.detail.recordCount", { count: member.recordCount })}</span>{divergence && commonMemberIds.has(member.memberId) ? <Badge variant="warning" className="text-[10px]">{t("sessions.detail.commonVersionBadge")}</Badge> : branchIndex != null ? <Badge variant="outline" data-branch-badge className="text-[10px]">{t("sessions.detail.branchBadge", { index: branchIndex + 1 })}</Badge> : (!conflict || !member.canBeSource) && <MemberStatus status={status} equal={equal} />}</div>
                    <Collapsible className="mt-0.5">
                      <div className="flex flex-wrap items-center justify-between gap-x-2"><p className="text-[11px] text-muted-foreground">{formatDate(t, member.updatedAt)}</p><CollapsibleTrigger asChild><Button variant="ghost" size="sm" className="h-5 gap-0.5 px-0 text-[11px] text-brand">{conflict && member.canBeSource ? t("sessions.detail.viewContent") : t("sessions.detail.copyDetails")}<ChevronDown className="size-3" /></Button></CollapsibleTrigger></div>
                      <CollapsibleContent className="space-y-1.5 py-1.5 text-[11px] leading-4 text-muted-foreground">
                        {conflict && member.canBeSource && <div className="max-h-44 space-y-1.5 overflow-y-auto rounded-md bg-muted/60 p-2">
                          {member.contentPreview?.length ? <><p>{t("sessions.detail.recentContent", { count: member.contentPreview.length })}</p>{member.contentPreview.map((item, previewIndex) => <p key={previewIndex} className="break-words"><span className="font-medium text-foreground">{t("sessions.detail.speakerLabel", { speaker: item.speaker })}</span>{item.text}</p>)}</> : <p>{t("sessions.detail.noPreview")}</p>}
                        </div>}
                        <p className="break-words">{member.projectLabel || t("sessions.detail.noWorkspace")}</p><p className="break-words">{member.reason}</p>
                      </CollapsibleContent>
                    </Collapsible>
                    {isActive && member.versionStatus === "behind" && batchSource && <p className="mt-1.5 flex items-start gap-1 text-[11px] leading-4 text-muted-foreground"><ArrowRight className="mt-0.5 size-3 shrink-0" /><span className="break-words">{t("sessions.detail.syncFrom", { source: batchSource.accountName })}</span></p>}
                    {conflict && member.canBeSource && <Button variant="outline" size="sm" className="mt-1.5 h-7 px-2 text-xs" disabled={props.busy || !!props.unifyLoading} onClick={() => props.onPrepareUnify(member)}>{props.unifyLoading === member.memberId ? <Loader2 className="size-3.5 animate-spin" /> : null}{commonMemberIds.has(member.memberId) ? t("sessions.detail.useOldAsSource") : t("sessions.detail.useThisAsSource")}</Button>}
                  </article>
              </li>;
            })}
          </ul>
          </div>
          {equal && <p className="mt-3 flex items-center justify-center gap-1.5 text-[11px] leading-4 text-muted-foreground"><Check className="size-3.5 shrink-0 text-brand" />{t("sessions.detail.equalFooter")}</p>}
        </div>
        <p className="mt-2 flex items-start gap-1.5 text-[11px] leading-4 text-muted-foreground"><Info className="mt-0.5 size-3 shrink-0" />{divergence ? t("sessions.detail.hintDiverge") : conflict ? t("sessions.detail.hintConflict") : equal ? t("sessions.detail.hintEqual") : t("sessions.detail.hintDefault")}</p>
      </>}
    </div>
    {detail && <Popover open={props.addOpen} onOpenChange={(open) => { if (!open) props.setAddOpen(false); }}>
      <footer data-detail-footer className="relationship-footer shrink-0 space-y-1.5 border-t border-border/70 bg-card px-4 py-2.5">
        {canBatch ? <>
          <DemoAction className="w-full"><Button size="sm" className="h-8 w-full bg-brand text-xs text-brand-foreground hover:bg-brand/90" disabled={props.busy} onClick={props.onBatchSync}><RefreshCw className={`size-3.5 ${props.busy ? "animate-spin" : ""}`} />{t("sessions.detail.syncToBehind", { count: behind.length })}</Button></DemoAction>
          <div className="flex items-center justify-between gap-2"><p className="min-w-0 truncate text-[11px] text-muted-foreground" title={`${batchSource.accountName} → ${behind.map((member) => member.accountName).join(t("sessions.common.listSeparator"))}`}>{t("sessions.detail.syncTargets", { source: batchSource.accountName, count: behind.length })}</p><PopoverTrigger asChild><Button variant="outline" size="sm" className="h-8 shrink-0 text-xs" disabled={detail.addTargets.length === 0} onClick={openAdd}><Plus className="size-3.5" />{t("sessions.detail.linkNewAccount")}</Button></PopoverTrigger></div>
        </> : conflict ? <>
          <p className="text-center text-xs text-muted-foreground">{t("sessions.detail.chooseCopyFirst")}</p>
          <PopoverTrigger asChild><Button variant="outline" size="sm" className="h-8 w-full text-xs" disabled={detail.addTargets.length === 0} onClick={openAdd}><Plus className="size-3.5" />{t("sessions.detail.linkNewAccount")}</Button></PopoverTrigger>
        </> : <>
          <PopoverTrigger asChild><Button variant="outline" size="sm" className="h-8 w-full text-xs" disabled={detail.addTargets.length === 0} onClick={openAdd}><Link2 className="size-3.5" />{t("sessions.detail.linkNewAccount")}</Button></PopoverTrigger>
          <p className="text-center text-[11px] leading-4 text-muted-foreground">{t("sessions.detail.linkNewAccountHint")}</p>
        </>}
      </footer>
      <PopoverContent side="top" collisionPadding={12} aria-label={t("sessions.detail.linkNewAccount")} className="w-[min(304px,calc(100vw-32px))] space-y-2.5 p-3">
        <h3 className="text-xs font-semibold">{t("sessions.detail.linkNewAccount")}</h3>
        <p className="text-[11px] leading-4 text-muted-foreground">{equal ? t("sessions.detail.addDescriptionEqual") : t("sessions.detail.addDescription")}</p>
        <div className="space-y-1"><p className="text-[11px] font-medium">{t("sessions.detail.copySource")}</p>{sourceSelect(t("sessions.detail.copySourceAria"))}</div>
        <div className="space-y-1"><p className="text-[11px] font-medium">{t("sessions.common.targetAccount")}</p><Select value={props.addTargetId} onValueChange={props.setAddTargetId} disabled={props.busy || detail.addTargets.length === 0}>
          <SelectTrigger size="sm" className="w-full min-w-0 bg-background text-xs" aria-label={t("sessions.detail.targetAccountAria")}><SelectValue placeholder={t("sessions.detail.targetPlaceholder")} /></SelectTrigger>
          <SelectContent>{detail.addTargets.map((account) => <SelectItem key={account.id} value={account.id} className="text-xs">{displayName(account)} · {variantLabel(accountVariant(account))}</SelectItem>)}</SelectContent>
        </Select></div>
        {detail.addTargets.length === 0 && <p className="text-[11px] text-muted-foreground">{t("sessions.detail.noCompatibleAccounts")}</p>}
        <DemoAction className="w-full"><Button size="sm" className="h-8 w-full text-xs" disabled={props.busy || !selectedSource || !props.addTargetId} onClick={() => void completeAdd()}><Copy className="size-3.5" />{props.busy ? t("sessions.common.processing") : t("sessions.common.copyAndLink")}</Button></DemoAction>
        <PopoverArrow />
      </PopoverContent>
    </Popover>}
    <AlertDialog open={!!props.unifyPlan} onOpenChange={(open) => { if (!open) props.setUnifyPlan(null); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("sessions.detail.unifyTitle", { source: props.unifyPlan?.sourceName ?? "" })}</AlertDialogTitle>
          <AlertDialogDescription>
            {t("sessions.detail.unifyDescription", { count: props.unifyPlan?.targets.length ?? 0 })}
          </AlertDialogDescription>
        </AlertDialogHeader>
        {props.unifyPlan && commonMemberIds.has(props.unifyPlan.sourceMemberId) && <Alert variant="destructive" className="py-2.5">
          <TriangleAlert aria-hidden="true" />
          <AlertTitle className="text-xs leading-4">{t("sessions.detail.unifyOldWarningTitle")}</AlertTitle>
          <AlertDescription className="text-[11px] leading-4">{t("sessions.detail.unifyOldWarning")}</AlertDescription>
        </Alert>}
        {restartNotice && changedTargets.length > 0 && <Alert variant="warning" className="py-2.5 dark:text-amber-200">
          <Info aria-hidden="true" />
          <AlertTitle className="text-xs leading-4">{restartNotice.title}</AlertTitle>
          <AlertDescription className="text-[11px] leading-4 text-amber-900/80 dark:text-amber-200/80">{restartNotice.description}</AlertDescription>
        </Alert>}
        {overwrittenTargets.length > 0 && <p className="text-sm text-destructive">{t("sessions.detail.unifyOverwriteWarning", { count: overwrittenTargets.length })}</p>}
        {blockedTargets.length > 0 && <div role="alert" className="space-y-1 rounded-lg bg-muted p-3 text-sm">
          <p className="font-medium">{t("sessions.detail.unifyBlockedTitle")}</p>
          {blockedTargets.map((target) => <p key={target.memberId} className="break-words text-muted-foreground">{t("sessions.common.errorEntry", { name: target.accountName, error: target.issue })}</p>)}
        </div>}
        {blockedTargets.length === 0 && <p className="text-xs text-muted-foreground">{t("sessions.detail.unifyBlockedSummary", { count: changedTargets.length })}</p>}
        <AlertDialogFooter>
          <AlertDialogCancel disabled={props.busy}>{t("common.cancel")}</AlertDialogCancel>
          {blockedTargets.length === 0 && <DemoAction><Button variant={overwrittenTargets.length > 0 ? "destructive" : "default"} disabled={props.busy} onClick={() => void props.onConfirmUnify()}>{props.busy ? t("sessions.common.processing") : restartNotice?.willRestart ? t("sessions.detail.unifyConfirmRestart") : t("sessions.detail.unifyConfirm")}</Button></DemoAction>}
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
    <AlertDialog open={!!unlinkTarget} onOpenChange={(open) => { if (!open && !unlinkBusy) setUnlinkTargetId(null); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("sessions.detail.unlinkTitle", { name: unlinkTarget?.accountName ?? "" })}</AlertDialogTitle>
          <AlertDialogDescription>
            {t("sessions.detail.unlinkDescription")}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={unlinkBusy}>{t("common.cancel")}</AlertDialogCancel>
          <DemoAction><Button variant="destructive" disabled={unlinkBusy || props.busy} onClick={() => void completeUnlink()}>{unlinkBusy ? t("sessions.common.processing") : t("sessions.detail.unlinkConfirm")}</Button></DemoAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </section>;
}

function MemberStatus({ status, equal }: { status: SessionGroupMemberDetail["versionStatus"]; equal: boolean }) {
  const t = useT();
  const labels: Record<typeof status, MessageKey> = {
    latest: equal ? "sessions.detail.identicalBadge" : "sessions.detail.memberLatest", behind: "sessions.detail.memberBehind", diverge: "sessions.status.diverge",
    missing: "sessions.status.missing", unknown: "sessions.status.unknown", stale: "sessions.detail.memberStale", superseded: "sessions.detail.memberSuperseded",
  };
  const variant = status === "latest" ? "success" : ["behind", "diverge", "missing"].includes(status) ? "warning" : "outline";
  return <Badge variant={variant} className="text-[10px] font-medium">{t(labels[status])}</Badge>;
}

/** 档位标记：与账号页 / 设置页同一套官方图标，国际版带 INTL 角标；当前账号用绿色描边区分。 */
function MemberVariantMark({ client, variant, isCurrent }: { client: SessionGroupClient; variant: SessionGroupMemberDetail["variant"]; isCurrent: boolean }) {
  const t = useT();
  const intl = variantIsIntl(variant);
  const mark = client === "workbuddy"
    ? (intl ? <WorkBuddyAiMark size={18} /> : <WorkBuddyMark size={18} />)
    : client === "codebuddyIde"
      ? (intl ? <CodeBuddyAiIdeMark size={18} /> : <CodeBuddyCnIdeMark size={18} />)
      : (intl ? <VscodeExtAiMark size={18} /> : <VscodeExtMark size={18} />);
  const label = `${t(CLIENT_TITLE_KEY[client])} · ${variantLabel(variant)}${isCurrent ? ` · ${t("sessions.common.currentAccount")}` : ""}`;
  return <span className={`inline-flex shrink-0 rounded-[26%] ${isCurrent ? "ring-2 ring-brand ring-offset-1 ring-offset-card" : ""}`} title={label} aria-label={label} role="img">{mark}</span>;
}

function formatDate(t: Translate, timestamp: number): string {
  if (!timestamp) return t("sessions.detail.updatedUnknown");
  return new Intl.DateTimeFormat(intlLocale(), { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }).format(timestamp);
}
