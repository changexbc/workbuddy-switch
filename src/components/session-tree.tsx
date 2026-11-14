import type { ReactNode } from "react";
import { ChevronDown, ChevronRight, Folder } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { useT, type Translate } from "@/lib/i18n";
import type { Session } from "@/lib/types";

/**
 * WorkBuddy 会话树（切号弹窗与「会话管理」页共用）：
 * 任务（playground）平铺，空间按文件夹（cwd 最后一段）分组，头部三态勾选。
 *
 * 只负责展示与收集：勾选集合与展开集合都由调用方持有（与切号弹窗的既有契约一致）。
 */

export type FolderGroup = { key: string; label: string; sessions: Session[] };
export type KindGroup = {
  key: "task" | "space";
  label: string;
  count: number;
  sessions: Session[];
  folders?: FolderGroup[];
};

export function selectionState(sessions: Session[], selected: Set<string>) {
  const ids = sessions.map((s) => s.id);
  const n = ids.filter((id) => selected.has(id)).length;
  return { allOn: n === ids.length && ids.length > 0, someOn: n > 0 && n < ids.length };
}

export function TreeCheckbox({
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
      className="size-3.5 shrink-0 cursor-pointer accent-primary"
      checked={allOn}
      ref={(el) => {
        if (el) el.indeterminate = someOn;
      }}
      onChange={onChange}
      aria-label={ariaLabel}
    />
  );
}

export function SessionPickRow({
  session,
  checked,
  indentClass,
  onToggle,
  trailing,
}: {
  session: Session;
  checked: boolean;
  indentClass: string;
  onToggle: () => void;
  /** 行尾附加内容（会话管理页展示副本状态）；缺省不渲染。 */
  trailing?: ReactNode;
}) {
  const t = useT();
  return (
    <label
      className={`flex cursor-pointer items-center gap-2.5 rounded-md py-1.5 pr-2 hover:bg-accent/50 ${indentClass}`}
    >
      <input
        type="checkbox"
        className="size-3.5 shrink-0 cursor-pointer accent-primary"
        checked={checked}
        onChange={onToggle}
      />
      <span className="min-w-0 flex-1 truncate text-sm" title={session.title}>
        {session.title}
      </span>
      {session.hasHistory && (
        <Badge variant="outline" className="shrink-0 text-[10px]">
          {t("sessions.tree.hasContent")}
        </Badge>
      )}
      {trailing}
    </label>
  );
}

/** WorkBuddy 侧栏文件夹名：cwd 最后一段。 */
export function sessionFolderLabel(cwd: string, t: Translate): string {
  const normalized = cwd.trim().replace(/[\\/]+$/, "");
  if (!normalized) return t("sessions.tree.ungrouped");
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || normalized;
}

/** 按工作目录分组，文件夹顺序跟会话一样按最近活动排。 */
export function groupSessionsByFolder(sessions: Session[], t: Translate): FolderGroup[] {
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
    label: key === "__none__" ? t("sessions.tree.ungrouped") : sessionFolderLabel(key, t),
    sessions: groups.get(key) ?? [],
  }));
}

/** 对齐 WorkBuddy 侧栏：任务（playground）平铺，空间按文件夹分组。 */
export function buildSessionTree(sessions: Session[], t: Translate): KindGroup[] {
  const tasks = sessions.filter((s) => s.isPlayground);
  const spaces = sessions.filter((s) => !s.isPlayground);
  const groups: KindGroup[] = [];
  if (tasks.length > 0) {
    groups.push({ key: "task", label: t("sessions.tree.tasks"), count: tasks.length, sessions: tasks });
  }
  if (spaces.length > 0) {
    const folders = groupSessionsByFolder(spaces, t);
    groups.push({
      key: "space",
      label: t("sessions.tree.spaces"),
      count: folders.length,
      sessions: spaces,
      folders,
    });
  }
  return groups;
}

/**
 * 会话树列表：任务 / 空间（按文件夹）分组，组头三态勾选与折叠。
 *
 * 勾选与展开集合由调用方持有；`renderTrailing` 用于行尾附加内容
 * （会话管理页展示副本状态），缺省与切号弹窗的渲染逐字一致。
 */
export function SessionTreeList({
  sessions,
  selected,
  expanded,
  onToggleSession,
  onToggleGroup,
  onToggleExpanded,
  renderTrailing,
  className,
}: {
  sessions: Session[];
  selected: Set<string>;
  /** 展开的节点：任务 / 空间 / 文件夹 key。 */
  expanded: Set<string>;
  onToggleSession: (id: string) => void;
  onToggleGroup: (ids: string[]) => void;
  onToggleExpanded: (key: string) => void;
  renderTrailing?: (session: Session) => ReactNode;
  className?: string;
}) {
  const t = useT();
  return (
    <div className={className}>
      {buildSessionTree(sessions, t).map((kind) => {
        const kindOpen = expanded.has(kind.key);
        const kindSel = selectionState(kind.sessions, selected);
        return (
          <div key={kind.key} className="mb-0.5">
            <div className="sticky top-0 z-10 flex items-center gap-1.5 rounded-md bg-background px-1.5 py-1">
              <TreeCheckbox
                allOn={kindSel.allOn}
                someOn={kindSel.someOn}
                onChange={() => onToggleGroup(kind.sessions.map((s) => s.id))}
                ariaLabel={t("sessions.tree.selectAria", { label: kind.label })}
              />
              <button
                type="button"
                className="flex min-w-0 flex-1 cursor-pointer items-center gap-1 rounded px-1 py-0.5 text-left hover:bg-accent/50"
                onClick={() => onToggleExpanded(kind.key)}
                aria-expanded={kindOpen}
                aria-label={kindOpen
                  ? t("sessions.tree.collapseAria", { label: kind.label })
                  : t("sessions.tree.expandAria", { label: kind.label })}
              >
                <span className="min-w-0 flex-1 truncate text-sm font-medium">
                  {kind.label}
                  <span className="ml-1 font-normal text-muted-foreground">({kind.count})</span>
                </span>
                {kindOpen ? (
                  <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
                ) : (
                  <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
                )}
              </button>
            </div>
            {kindOpen &&
              kind.key === "task" &&
              kind.sessions.map((s) => (
                <SessionPickRow
                  key={s.id}
                  session={s}
                  checked={selected.has(s.id)}
                  indentClass="pl-7"
                  onToggle={() => onToggleSession(s.id)}
                  trailing={renderTrailing?.(s)}
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
                        onChange={() => onToggleGroup(folder.sessions.map((s) => s.id))}
                        ariaLabel={t("sessions.tree.selectFolderAria", { label: folder.label })}
                      />
                      <button
                        type="button"
                        className="flex min-w-0 flex-1 cursor-pointer items-center gap-1.5 rounded px-1 py-0.5 text-left hover:bg-accent/50"
                        onClick={() => onToggleExpanded(folder.key)}
                        aria-expanded={folderOpen}
                        aria-label={folderOpen
                          ? t("sessions.tree.collapseFolderAria", { label: folder.label })
                          : t("sessions.tree.expandFolderAria", { label: folder.label })}
                      >
                        <Folder className="size-3.5 shrink-0 text-muted-foreground" />
                        <span className="min-w-0 flex-1 truncate text-sm">{folder.label}</span>
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
                          onToggle={() => onToggleSession(s.id)}
                          trailing={renderTrailing?.(s)}
                        />
                      ))}
                  </div>
                );
              })}
          </div>
        );
      })}
    </div>
  );
}
