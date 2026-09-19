import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { AlignDataReport } from "@/lib/types";

/** 对齐勾选项（本地新增 UI，上游对话框只保留 import）。 */
export interface AlignOptions {
  alignAutomations: boolean;
  alignFiles: boolean;
}

/** 条目语气：normal 正常 / warn 注意 / error 出错了。 */
export type PreviewTone = "normal" | "warn" | "error";

/** 预览里的一行：label 在左、value 靠右；chips 放项目名一类短标签。 */
export interface PreviewItem {
  label: string;
  value?: string;
  tone?: PreviewTone;
  /** 短标签（项目名等），渲染在 label 下一行的可换行容器里。 */
  chips?: string[];
}

/** 预览里的一组：左侧色条 + 可选组标题。 */
export interface PreviewGroup {
  title?: string;
  tone?: PreviewTone;
  items: PreviewItem[];
}

/** 预览报告：总览一行 + 若干分组 + 末行结论。 */
export interface PreviewReport {
  summary?: string;
  groups: PreviewGroup[];
  footnote?: string;
}

/** 预览状态：报告 + 是否已过期（结果算完之后又动过勾选）。 */
export interface PreviewState {
  report: PreviewReport;
  stale: boolean;
}

interface Props {
  value: AlignOptions;
  onChange: (next: AlignOptions) => void;
  preview: PreviewState | null;
}

function Row({
  title,
  hint,
  checked,
  onCheckedChange,
}: {
  title: string;
  hint: string;
  checked: boolean;
  onCheckedChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-md border px-3 py-2.5">
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">{title}</div>
        <div className="text-xs text-muted-foreground">{hint}</div>
      </div>
      <Switch checked={checked} onCheckedChange={onCheckedChange} />
    </div>
  );
}

/** 左侧色条：让正常 / 注意 / 出错三档一眼分开，不靠缩进。 */
const BAR: Record<PreviewTone, string> = {
  normal: "border-l-border",
  warn: "border-l-amber-500",
  error: "border-l-destructive",
};
/** 组标题色。 */
const TITLE_TONE: Record<PreviewTone, string> = {
  normal: "text-muted-foreground",
  warn: "text-amber-700 dark:text-amber-400",
  error: "text-destructive",
};
/** label 色（左列）。 */
const LABEL_TONE: Record<PreviewTone, string> = {
  normal: "text-muted-foreground",
  warn: "text-amber-700 dark:text-amber-400",
  error: "text-destructive",
};
/** value 色（右列，要点数字）。 */
const VALUE_TONE: Record<PreviewTone, string> = {
  normal: "text-foreground",
  warn: "text-amber-700 dark:text-amber-400",
  error: "text-destructive",
};

function PreviewGroupBlock({ group }: { group: PreviewGroup }) {
  const tone = group.tone ?? "normal";
  return (
    <div className={cn("border-l-2 pl-2.5", BAR[tone])}>
      {group.title && (
        <div className={cn("mb-1 text-xs font-medium", TITLE_TONE[tone])}>{group.title}</div>
      )}
      <div className="space-y-1">
        {group.items.map((item, i) => {
          const itemTone = item.tone ?? tone;
          return (
            <div key={i} className="text-xs">
              <div className="flex items-baseline justify-between gap-3">
                <span className={cn("min-w-0 break-words", LABEL_TONE[itemTone])}>
                  {item.label}
                </span>
                {item.value && (
                  <span
                    className={cn("shrink-0 font-mono tabular-nums", VALUE_TONE[itemTone])}
                  >
                    {item.value}
                  </span>
                )}
              </div>
              {item.chips && item.chips.length > 0 && (
                <div className="mt-1 flex flex-wrap gap-1">
                  {item.chips.map((c) => (
                    <span
                      key={c}
                      className="rounded bg-background px-1.5 py-0.5 text-xs text-muted-foreground"
                    >
                      {c}
                    </span>
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** 预览面板：总览 + 分组 + 结论；过期时整块压暗并给重跑提示。 */
function PreviewPanel({ preview }: { preview: PreviewState }) {
  const { report, stale } = preview;
  return (
    <div
      className={cn(
        "space-y-2.5 rounded-md border bg-muted/40 px-3 py-2.5",
        stale && "opacity-60",
      )}
      aria-live="polite"
    >
      <div className="flex flex-wrap items-center justify-between gap-x-2 gap-y-1">
        <div className="text-xs font-medium text-muted-foreground">
          切换前预览<span className="ml-1.5 font-normal">按当前勾选算</span>
        </div>
        {stale && (
          <span className="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-xs text-amber-700 dark:text-amber-300">
            设置动过了，重新预览一下
          </span>
        )}
      </div>

      {report.summary && <div className="text-xs text-foreground">{report.summary}</div>}

      {report.groups.map((g, i) => (
        <PreviewGroupBlock key={i} group={g} />
      ))}

      {report.footnote && (
        <div className="text-xs text-muted-foreground">{report.footnote}</div>
      )}
    </div>
  );
}

/** 切号弹窗里的对齐开关 + 预览结果。 */
export function AlignOptionsPanel({ value, onChange, preview }: Props) {
  return (
    <>
      <Row
        title="带走定时任务"
        hint="让目标账号也能看到、继续管你现在这些定时任务（只改本机记录，云端不受影响）"
        checked={value.alignAutomations}
        onCheckedChange={(v) => onChange({ ...value, alignAutomations: v })}
      />

      <Row
        title="同步设置与文件"
        hint="把设置、界面主题、用户数据按目标账号补齐。登录凭据不动，免得串号"
        checked={value.alignFiles}
        onCheckedChange={(v) => onChange({ ...value, alignFiles: v })}
      />

      {preview && <PreviewPanel preview={preview} />}
    </>
  );
}

/** 取路径最后一段做项目名（`D:\w-dev\x` → `x`）。 */
function basename(cwd: string): string {
  const norm = cwd.replace(/[\\/]+$/, "");
  const i = Math.max(norm.lastIndexOf("\\"), norm.lastIndexOf("/"));
  return i >= 0 ? norm.slice(i + 1) : norm;
}

/**
 * 把「切换前预览」的对齐报告整理成分组文本。
 *
 * ⚠️ 只服务预览（`dryRun=true`）：真实执行完成后的口播在 `doSwitch` 里自拼 toast，
 * 不走这里 ⇒ 别把 `r.dryRun === false` 的分支当活路径读，它只是同源的兜底。
 */
export function formatAlignReport(r: AlignDataReport): PreviewReport {
  if (r.error) {
    return {
      groups: [
        { tone: "error", title: "出问题了", items: [{ label: r.error, tone: "error" }] },
      ],
    };
  }
  if (r.noop) {
    return { groups: [{ items: [{ label: "这次没有勾选要处理的项目" }] }] };
  }

  // ── 组：同步设置（定时任务归属 + 设置/资料/主题跟随）──
  // ⚠️ 这一组是「同步」，不是「转移」：数据不会被搬走，源账号仍保留自己的一份。
  const move: PreviewItem[] = [];
  // `syncChanged` 只数**真会动**的项：「已经一致 / 待更 0」不算，
  // 总览报这个数（明细仍列全部 6 行，好对照哪些不用管）。
  let syncChanged = 0;
  if (r.automations) {
    if (r.automations.updated) syncChanged += 1;
    move.push({ label: "定时任务", value: `${r.automations.updated} 条` });
    if (r.automations.outbox) {
      syncChanged += 1;
      move.push({ label: "待发送任务", value: `${r.automations.outbox} 条` });
    }
  }
  const f = r.settings;
  if (f) {
    if (f.claw) {
      if (f.claw.changed) syncChanged += 1;
      move.push(
        f.claw.changed
          ? { label: "应用设置", value: `${f.claw.changed} 项` }
          : { label: "应用设置", value: "已经一致" },
      );
    }
    if (f.storage) {
      // 三类是互斥动作，只报真正会发生的那几种：0 的项不显示。
      // `deferred` = 合并型文件（如汇总文件），只并内容不覆盖，说「汇总」没人懂。
      if (f.storage.copied || f.storage.deferred) syncChanged += 1;
      const sb: string[] = [];
      if (f.storage.copied) sb.push(`复制 ${f.storage.copied} 份`);
      if (f.storage.skipped) sb.push(`跳过 ${f.storage.skipped} 份`);
      if (f.storage.deferred) sb.push(`合并 ${f.storage.deferred} 份（不覆盖）`);
      move.push({ label: "用户数据", value: sb.length ? sb.join(" · ") : "已经一致" });
    }
    if (f.memory) {
      if (f.memory.changed) syncChanged += 1;
      move.push({ label: "账号资料", value: f.memory.changed ? "待同步" : "已经一致" });
    }
    if (f.myFiles) {
      if (f.myFiles.changed) syncChanged += 1;
      move.push({
        label: "我的文件",
        value: `${f.myFiles.files} 份 · 待更 ${f.myFiles.changed}`,
      });
    }
    if (f.theme) {
      // 主题跟随受「同步设置与文件」管辖：报告没写 theme 字段 = 本次没开，行直接隐藏。
      if (f.theme.planned !== false) syncChanged += 1;
      move.push({ label: "界面主题", value: "跟随目标账号" });
    }
  }

  // ── 组：清理旧会话 ──
  let slimGroup: PreviewGroup | null = null;
  if (r.slim) {
    if (r.slim.error) {
      slimGroup = {
        title: "清理旧会话",
        tone: "error",
        items: [{ label: "出错了", value: r.slim.error, tone: "error" }],
      };
    } else {
      const items: PreviewItem[] = [
        {
          label: `本机 · 每项目留 ${r.slim.keep ?? 3} 条`,
          value: `软删 ${r.slim.planned ?? 0} 条`,
        },
      ];
      const gs = r.slim.groups ?? [];
      if (gs.length) {
        const chips = gs.slice(0, 3).map((g) => `${basename(g.cwd)} ${g.count}`);
        if (gs.length > 3) chips.push(`+${gs.length - 3}`);
        items.push({ label: `涉及 ${gs.length} 个项目`, chips });
      }
      if (r.dryRun) {
        // 本次会搬过去的会话（手动复制 + 共享）都不参与收起；没搬运就不显示这行（免得空喊一句）。
        const cp = r.slim.copyPlanned;
        if (cp && cp.total > 0) {
          items.push({
            label: `这次搬过去的 ${cp.total} 条不会被软删${
              cp.hitCount ? `（其中 ${cp.hitCount} 条就在上面这些项目里）` : ""
            }`,
            tone: "warn",
          });
        }
      }
      const cl = r.slim.cloud;
      if (cl?.enabled) {
        if (!cl.tokenReady) {
          items.push({
            label: "云端这轮不动（没读到该账号的登录凭证）",
            value: "只清本机",
            tone: "warn",
          });
        } else if (r.dryRun) {
          items.push({ label: "云端 · 仅本账号", value: `约 ${cl.planned ?? 0} 条` });
        } else {
          const removed = cl.removed ?? 0;
          const gone = cl.alreadyGone ?? 0;
          const total = cl.deleted ?? removed + gone;
          if (total > 0) {
            items.push({
              label: gone > 0 ? "云端清掉（含本来就没有的）" : "云端删掉",
              value: `${gone > 0 ? total : removed || total} 条`,
            });
          }
          if (cl.noToken) {
            items.push({
              label: "缺登录凭证，只清在本机",
              value: `${cl.noToken} 条`,
              tone: "warn",
            });
          }
          if (cl.failed) {
            items.push({
              label: "云端没删掉，本机先留着，下次切号再试",
              value: `${cl.failed} 条`,
              tone: "warn",
            });
          }
          if (cl.foreign) {
            items.push({ label: "属于其他账号，没动", value: `${cl.foreign} 条` });
          }
          if (!total && !cl.noToken && !cl.failed && !cl.foreign) {
            items.push({ label: "云端这轮没有要清的", value: "0 条" });
          }
        }
      }
      slimGroup = { title: "清理旧会话", tone: "warn", items };
    }
  }

  // ── 组：云端正本（对账 + 全账巡检，只读） ──
  let cloudGroup: PreviewGroup | null = null;
  const cl = r.slim?.cloud;
  if (cl?.enabled) {
    const items: PreviewItem[] = [];
    const inv = cl.inventory;
    if (inv?.enabled) {
      items.push({ label: "云端名下会话", value: `${inv.cloud ?? 0} 条` });
      items.push({ label: "其中本机对得上", value: `${inv.aligned ?? 0} 条` });
      if (inv.foreign) {
        items.push({ label: "别的设备在用，没动", value: `${inv.foreign} 条` });
      }
      if (inv.stale) {
        items.push({ label: "本机已经删了", value: `${inv.stale} 条` });
      }
      // 只报「活着但还没上云」：已删且云端也没了是常态，混进来数字会虚高一百多。
      if (inv.localOnlyAlive) {
        items.push({
          label: "本机还在、但没上云",
          value: `${inv.localOnlyAlive} 条`,
          tone: "warn",
        });
      }
    }
    // 只留「看得懂、用得上」的四行：云端名下 / 对得上 / 别的设备 / 没上云。
    // 「同步记录 / 本机查不到来历 / 清遗留」属内部账本口径：清遗留清的是早就
    // 失效的云端映射行（实测抽样全 404，不是用户的云端资产），显示只会吓人。
    if (items.length) cloudGroup = { title: "云端正本", items };
  }

  // ── 总览：一行给「会动多少」，数字好扫 ──
  const bits: string[] = [];
  if (move.length) {
    // 报「列了几项（几项要动）」：只报要动的会跟明细行数对不上，只报总数又会被当成全都要动。
    bits.push(
      syncChanged
        ? `同步设置 ${move.length} 项（${syncChanged} 项要动）`
        : `同步设置 ${move.length} 项 · 都已经一致`,
    );
  }
  if (r.slim && !r.slim.error) bits.push(`清理旧会话 ${r.slim.planned ?? 0} 条`);
  if (cl?.enabled && cl.tokenReady && r.dryRun) {
    // 只报「跟着本机收起一起删」的那部分，跟下面云端正本里看得见的数字对得上。
    // 对账清遗留（reconcile）不报：它清的是失效映射行，明细里也不显示。
    bits.push(`清理云端约 ${cl.planned ?? 0} 条`);
  }

  const groups: PreviewGroup[] = [];
  if (move.length) groups.push({ title: "同步设置", items: move });
  if (slimGroup) groups.push(slimGroup);
  if (cloudGroup) groups.push(cloudGroup);

  return {
    summary: bits.length ? bits.join(" · ") : undefined,
    groups,
    footnote: r.dryRun ? "以上是预览，还没真正执行" : "切换完成（已先备份数据与设置）",
  };
}
