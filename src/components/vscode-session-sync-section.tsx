import { useEffect, useState } from "react";
import { Link2, RotateCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  buildSelections,
  isActionable,
  isDirectlySyncable,
  LinksSkeleton,
  LIST_MIN_H,
  SessionLinkCard,
  STATUS_MIN_H,
  type SessionLinksMeta,
} from "@/components/session-link-shared";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import type {
  AccountMeta,
  SessionLinkPreviewGroup,
  SessionLinksPreview,
  SessionSyncSelection,
} from "@/lib/types";

interface Props {
  /** 目标账号（来源账号身份由后端从插件登录态读取，前端不传） */
  account: AccountMeta | null;
  /** 弹窗是否打开：打开时拉取一次预览 */
  open: boolean;
  /** 切换进行中：禁止继续交互 */
  disabled?: boolean;
  /** 插件是否已登录：未登录时渲染专属空态，不发预览请求 */
  loggedIn: boolean;
  /** 勾选结果变化：父组件据此提交 `syncSelections`，并用于结果反馈里回显会话名 */
  onChange: (state: { selections: SessionSyncSelection[]; groups: SessionLinkPreviewGroup[] }) => void;
  /** 预览状态上报：父组件用于 tab 徽标 */
  onMetaChange?: (meta: SessionLinksMeta) => void;
  /**
   * 预览数据源：默认走 VS Code 插件通道；CodeBuddy IDE 弹窗传入 IDE 通道。
   * 必须传稳定引用（模块级函数），否则会随重渲染反复拉取预览。
   */
  fetchPreview?: (targetAccountId: string) => Promise<SessionLinksPreview>;
  /** 未登录时的空态文案（按目标客户端定制）。 */
  loggedOutHint?: string;
}

/**
 * 切号弹窗「关联会话」tab 的内容（VS Code 插件；CodeBuddy IDE 通过 `fetchPreview` 复用同一展示件）。
 *
 * 展示件与勾选逻辑与 WorkBuddy 侧共用（`session-link-shared.tsx`），差异只在数据源：
 * 这里读目标侧的关联表（`vscode_session_links.json` / `codebuddy_ide_session_links.json`），
 * 且没有档位与「自动同步」开关（D5：同步跟随「确认切换」）。
 */
export function VscodeSessionSyncSection({
  account,
  open,
  disabled,
  loggedIn,
  onChange,
  onMetaChange,
  fetchPreview = api.vscodeSessionLinksPreview,
  loggedOutHint = "未检测到 VS Code CodeBuddy 插件当前登录账号，请先在 VS Code 中登录该插件后再切换。",
}: Props) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [preview, setPreview] = useState<SessionLinksPreview | null>(null);
  /** 用户逐项勾选状态；仅在可勾选项上生效。 */
  const [checked, setChecked] = useState<Set<string>>(new Set());
  /** 手动重试计数：用于「重新检查」。 */
  const [reloadToken, setReloadToken] = useState(0);

  useEffect(() => {
    // 未登录插件时后端没有来源身份可读，不发请求，直接渲染专属空态。
    if (!open || !account || !loggedIn) {
      setPreview(null);
      setError("");
      setChecked(new Set());
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError("");
    fetchPreview(account.id)
      .then((res) => {
        if (cancelled) return;
        setPreview(res);
        // 默认勾选值来自后端 defaultChecked，前端不扩大权限。
        const defaults = new Set(
          res.groups.filter((group) => group.defaultChecked && isActionable(group)).map((g) => g.groupId),
        );
        setChecked(defaults);
        onChange({ selections: buildSelections(res.groups, defaults), groups: res.groups });
      })
      .catch((e) => {
        if (cancelled) return;
        setPreview(null);
        setError(api.asError(e));
        onChange({ selections: [], groups: [] });
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // 预览拉取只看「弹窗开关 / 目标账号 / 登录态 / 手动重试 / 数据源」；onChange 只做状态回写，不进依赖。
    // fetchPreview 必须是稳定引用（模块级函数），否则会随重渲染反复拉取。
  }, [open, account, loggedIn, reloadToken, fetchPreview]);

  const groups = preview?.groups ?? [];
  // 插件侧没有档位能力探测，正常不会出现 supported=false；保留兜底以免渲染空 tab。
  const unsupported = Boolean(preview && (!preview.supported || preview.storeStatus === "unsupported"));
  const targetLabel = account?.nickname || account?.email || account?.uid || "目标账号";

  // 状态上报：父组件据此渲染 tab 徽标。
  useEffect(() => {
    onMetaChange?.({
      available: !unsupported,
      groupCount: groups.length,
      error,
      storeStatus: preview?.storeStatus ?? null,
      storeError: preview?.storeError ?? "",
    });
    // onMetaChange 只做状态回写，不进依赖。
  }, [unsupported, groups.length, error, preview?.storeStatus, preview?.storeError]);

  const syncable = groups.filter(isDirectlySyncable);
  const selectedCount = groups.filter((group) => isActionable(group) && checked.has(group.groupId)).length;
  const selectedSyncableCount = syncable.filter((group) => checked.has(group.groupId)).length;
  const allSyncableSelected = syncable.length > 0 && selectedSyncableCount === syncable.length;

  /** 全选/取消全选：只作用于可直接同步的会话；覆盖项必须单独勾选。 */
  function toggleAllSyncable() {
    const updated = new Set(checked);
    if (allSyncableSelected) syncable.forEach((group) => updated.delete(group.groupId));
    else syncable.forEach((group) => updated.add(group.groupId));
    setChecked(updated);
    onChange({ selections: buildSelections(groups, updated), groups });
  }

  function toggleGroup(group: SessionLinkPreviewGroup, next: boolean) {
    const updated = new Set(checked);
    if (next) updated.add(group.groupId);
    else updated.delete(group.groupId);
    setChecked(updated);
    onChange({ selections: buildSelections(groups, updated), groups });
  }

  if (unsupported) return null;

  const pending = loading || (!preview && !error && loggedIn);
  const storeUnavailable = preview?.storeStatus === "unavailable";

  return (
    <section className="space-y-3" aria-label="关联会话">
      <div className="flex items-start gap-3 rounded-md border bg-muted/30 px-3 py-3">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-background text-muted-foreground">
          <Link2 className="size-4" />
        </span>
        <div className="min-w-0 space-y-0.5">
          <div className="text-sm font-medium">什么是关联会话？</div>
          <p className="text-xs text-muted-foreground">
            通过本工具复制到其他账号的会话，会自动建立关联。切换时，可选择将当前账号的后续内容同步到对应会话。
          </p>
        </div>
      </div>

      {pending && <LinksSkeleton />}

      {!pending && !loggedIn && (
        <div
          className={cn(
            "flex items-center justify-center rounded-md border px-3 py-2.5",
            STATUS_MIN_H,
          )}
        >
          <p className="text-center text-xs text-muted-foreground">{loggedOutHint}</p>
        </div>
      )}

      {!pending && loggedIn && (error || storeUnavailable) && (
        <div
          className={cn(
            "flex items-center justify-between gap-3 rounded-md border px-3 py-2.5",
            STATUS_MIN_H,
          )}
        >
          <span className="min-w-0 flex-1 text-xs text-muted-foreground">
            {error ? "暂时无法检查会话，本次不能同步" : "同步记录不可用，本次不会同步"}
          </span>
          <Button
            variant="outline"
            size="sm"
            className="shrink-0"
            disabled={disabled}
            onClick={() => setReloadToken((token) => token + 1)}
          >
            <RotateCw />
            重新检查
          </Button>
        </div>
      )}

      {!pending && loggedIn && !error && preview?.storeStatus === "missing" && (
        <div
          className={cn(
            "flex items-center justify-center rounded-md border px-3 py-2.5",
            STATUS_MIN_H,
          )}
        >
          <p className="text-center text-xs text-muted-foreground">
            还没有可以同步的会话：先在「复制会话」里复制一次，之后切换回来就能在这里同步新内容。
          </p>
        </div>
      )}

      {!pending && loggedIn && !error && !storeUnavailable && preview?.storeStatus === "ready" && groups.length === 0 && (
        <div
          className={cn(
            "flex items-center justify-center rounded-md border px-3 py-2.5",
            STATUS_MIN_H,
          )}
        >
          <p className="text-center text-xs text-muted-foreground">
            这两个账号还没有共同复制过的会话（只处理双方都有的，不涉及其他账号）。
          </p>
        </div>
      )}

      {!pending && loggedIn && !error && groups.length > 0 && (
        <div className="space-y-2">
          <div className="flex items-center gap-2.5 px-1">
            <Checkbox
              checked={allSyncableSelected ? true : selectedSyncableCount > 0 ? "indeterminate" : false}
              disabled={disabled || syncable.length === 0}
              onCheckedChange={() => toggleAllSyncable()}
              aria-label="全选可直接同步的会话"
            />
            <span className="min-w-0 flex-1 text-sm font-medium">全选可直接同步的会话</span>
            <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
              {`已选 ${selectedCount} / ${groups.length}`}
            </span>
          </div>
          <div
            className={cn(
              "max-h-[min(24rem,50vh)] divide-y overflow-y-auto rounded-md border",
              LIST_MIN_H,
            )}
          >
            {groups.map((group) => (
              <SessionLinkCard
                key={group.groupId}
                group={group}
                targetLabel={targetLabel}
                checked={checked.has(group.groupId) && isActionable(group)}
                disabled={disabled}
                onToggle={(next) => toggleGroup(group, next)}
              />
            ))}
          </div>
          <p className="px-1 text-xs text-muted-foreground">全选仅包含可直接同步的会话，覆盖需单独勾选。</p>
        </div>
      )}
    </section>
  );
}
