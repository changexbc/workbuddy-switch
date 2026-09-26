import { ChevronDown, ChevronRight } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import type { VscodeSession } from "@/lib/types";

/**
 * 切号弹窗「复制会话」tab 的共用展示件（VS Code 插件 / CodeBuddy IDE 同形）。
 *
 * 只负责渲染与收集：勾选集合由父组件持有，空态文案与能力判定由父组件按目标侧状态计算。
 * 会话 id 生成规则（沿用 / 新 id）由后端决定，前端只提交 `{workspaceHash, conversationId}`。
 */
export interface SessionCopyTabProps {
  /** 会话列表加载中。 */
  loading: boolean;
  /** 状态提示文案（未安装 / 未登录 / 无会话 / 可复制说明，由父组件计算）。 */
  hint: string;
  /** 是否有可复制（含正文）的会话；false 时开关禁用且提示按告警色渲染。 */
  hasCopyable: boolean;
  /** 复制开关：勾选即意图。 */
  enabled: boolean;
  onEnabledChange: (next: boolean) => void;
  /** 按工作区分组的可复制会话。 */
  groups: WorkspaceGroup[];
  selected: Set<string>;
  onToggleSession: (id: string) => void;
  onToggleGroup: (ids: string[]) => void;
  collapsed: Set<string>;
  onToggleCollapsed: (key: string) => void;
}

export function SessionCopyTab({
  loading,
  hint,
  hasCopyable,
  enabled,
  onEnabledChange,
  groups,
  selected,
  onToggleSession,
  onToggleGroup,
  collapsed,
  onToggleCollapsed,
}: SessionCopyTabProps) {
  return (
    <>
      <div className="flex items-center justify-between gap-3 rounded-md border px-3 py-2.5">
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium">复制会话到目标账号</div>
          {loading ? (
            // 单行高度对齐真实提示文案（实测常见态折一行，16px），加载完成后不跳高度。
            <Skeleton className="h-4 w-3/4" aria-hidden="true" />
          ) : (
            <div
              className={
                !hasCopyable ? "text-xs text-amber-700 dark:text-amber-400" : "text-xs text-muted-foreground"
              }
            >
              {hint}
            </div>
          )}
        </div>
        <Switch
          checked={enabled}
          onCheckedChange={onEnabledChange}
          disabled={loading || !hasCopyable}
          aria-label="复制会话到目标账号"
        />
      </div>

      {enabled && (
        <>
          <Separator />
          <div className="max-h-[min(20rem,45vh)] overflow-y-auto pr-1">
            {loading ? (
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
                        onChange={() => onToggleGroup(ids)}
                        ariaLabel={`选择${group.label}`}
                      />
                      <button
                        type="button"
                        className="flex min-w-0 flex-1 items-center gap-1 rounded px-1 py-0.5 text-left hover:bg-accent/50"
                        onClick={() => onToggleCollapsed(group.key)}
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
                          onToggle={() => onToggleSession(session.id)}
                        />
                      ))}
                  </div>
                );
              })
            )}
          </div>
        </>
      )}
    </>
  );
}

export interface WorkspaceGroup {
  key: string;
  label: string;
  hash: string;
  sessions: VscodeSession[];
}

/** 按工作区 hash 分组，仅保留含正文的会话；分组按最近活动时间降序。 */
export function buildGroups(sessions: VscodeSession[]): WorkspaceGroup[] {
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

/** 组头三态复选框：全选 / 半选（点击即全选）/ 未选。 */
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
    <Checkbox
      checked={allOn ? true : someOn ? "indeterminate" : false}
      onCheckedChange={onChange}
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
      <Checkbox
        checked={checked}
        onCheckedChange={onToggle}
        aria-label={`选择会话 ${session.title}`}
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
