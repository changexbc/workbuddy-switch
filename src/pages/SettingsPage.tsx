import { useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import {
  ArrowUpCircle,
  ChevronDown,
  CircleCheck,
  ExternalLink,
  Loader2,
  RefreshCw,
  Save,
} from "lucide-react";
import { toast } from "sonner";

import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from "@/components/ui/accordion";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { TimePicker } from "@/components/ui/time-picker";
import * as api from "@/lib/api";
import { canPersistErrorLog } from "@/lib/error-report";
import { getThemePreference, setThemePreference, type ThemePreference } from "@/lib/theme";
import type {
  AccountMeta,
  AppNotification,
  AutoRotateConfig,
  CheckinConfig,
  CheckinLog,
  GithubConfig,
  RateLimitConfig,
  RateLimitHookStatus,
  RotateLog,
  RotateStatus,
  TravelConfig,
  UpdateInfo,
} from "@/lib/types";
import { GITHUB_RELEASE_URL, GITHUB_REPOSITORY_URL, openReleaseUrl } from "@/lib/update";
import { useUpdateState } from "@/lib/use-update-state";
import { changeCompanionEnabled, reloadCompanionEnabled, useCompanionEnabled } from "@/lib/use-companion-enabled";
import { cn } from "@/lib/utils";
import { accountVariant, variantSupportsCheckin, variantSupportsTravel } from "@/lib/variant";
import { UpdateInstallDialog } from "@/components/update-install-dialog";
import { DemoAction } from "@/components/demo-action";
import { useAccountsStore } from "@/stores/accounts";

interface SettingsGroupProps {
  id: string;
  title: string;
  children: ReactNode;
}

/**
 * 折叠面板的底部分隔线。
 *
 * 用伪元素而不是 border：面板自身无边距，border 会从卡片边缘拉到边缘，与其它行
 * （左右各缩进 mx-4 / sm:mx-5）的线对不齐。线要跟着整块内容走，标题与内容之间不留线。
 * 末行不传此项，但面板仍须保留 relative——行尾箭头是绝对定位，缺定位祖先会飘到页面上。
 */
const ACCORDION_DIVIDER =
  "after:absolute after:inset-x-4 after:bottom-0 after:h-px after:bg-border/50 sm:after:inset-x-5";

function SettingsGroup({ id, title, children }: SettingsGroupProps) {
  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby={id}>
      <div className="px-1">
        <h2 id={id} className="text-[13px] font-medium leading-5">
          {title}
        </h2>
      </div>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">{children}</Card>
    </section>
  );
}

function SettingsRow({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        "mx-4 flex min-w-0 items-center justify-between gap-3 border-b border-border/50 px-0 py-2.5 sm:mx-5",
        className,
      )}
    >
      {children}
    </div>
  );
}

interface SettingsFieldRowProps {
  label: ReactNode;
  description?: ReactNode;
  htmlFor?: string;
  children: ReactNode;
  className?: string;
  operational?: boolean;
}

function SettingsFieldRow({
  label,
  description,
  htmlFor,
  children,
  className,
  operational = false,
}: SettingsFieldRowProps) {
  return (
    <SettingsRow className={cn("flex-col items-stretch gap-2 sm:flex-row sm:items-center", className)}>
      <div className="min-w-0 flex-1">
        {htmlFor ? (
          <Label htmlFor={htmlFor} className="text-[13px] leading-4">
            {label}
          </Label>
        ) : (
          <div className="text-[13px] font-medium leading-4">{label}</div>
        )}
        {description && (
          <p className="mt-0.5 text-xs leading-4 text-muted-foreground/75">{description}</p>
        )}
      </div>
      <div className="flex min-w-0 w-full shrink-0 justify-end sm:w-auto">
        {operational ? <DemoAction className="w-full sm:w-auto">{children as ReactElement}</DemoAction> : children}
      </div>
    </SettingsRow>
  );
}

/** 展开内容的内嵌卡片：与外层行同一左右缩进，内部各行自带分隔线。 */
const INSET_PANEL = "mx-4 rounded-lg bg-foreground/[0.04] px-3 py-1 sm:mx-5";
/** 放进内嵌卡片的参数行：去掉外层缩进，改由卡片统一提供。 */
const PANEL_ROW = "mx-0 sm:mx-0";

interface AccordionSettingsRowProps {
  value: string;
  label: ReactNode;
  description?: ReactNode;
  /** 行内常驻操作（开关、按钮等）；有值时箭头改画在行尾，避开操作区。 */
  actions?: ReactNode;
  /** 是否带底部分隔线；末行传 false。 */
  divider?: boolean;
  children: ReactNode;
}

/**
 * 手风琴设置行：标题可点展开，展开内容在其下方。
 *
 * 行内带 Switch / Button 时触发区只能覆盖标题（button 不能嵌套 button），
 * 此时关掉触发区自带箭头、绝对定位到行尾，避免它落在标题与控件之间。
 */
function AccordionSettingsRow({
  value,
  label,
  description,
  actions,
  divider = true,
  children,
}: AccordionSettingsRowProps) {
  return (
    <AccordionItem value={value} className={cn("relative", divider && ACCORDION_DIVIDER)}>
      <div
        className={cn(
          "relative mx-4 flex min-w-0 flex-col items-stretch justify-between gap-2 py-2.5 sm:mx-5 sm:flex-row sm:items-center",
          actions ? "sm:pr-7" : undefined,
        )}
      >
        <AccordionTrigger chevron={!actions} className="min-w-0 flex-1 gap-0 py-0">
          <div className="min-w-0 flex-1">
            <div className="text-[13px] font-medium leading-4">{label}</div>
            {description && (
              <p className="mt-0.5 text-xs leading-4 text-muted-foreground/75">{description}</p>
            )}
          </div>
          {actions && (
            // -right-1 抵消 p-1：让图形右缘与触发区自带箭头落在同一条线上。
            <ChevronDown className="absolute -right-1 top-1/2 hidden size-4 -translate-y-1/2 box-content p-1 text-muted-foreground transition-transform duration-200 sm:block" />
          )}
        </AccordionTrigger>
        {actions && (
          <div className="flex min-w-0 w-full shrink-0 flex-wrap items-center justify-end gap-2 sm:w-auto">
            {actions}
          </div>
        )}
      </div>
      <AccordionContent className="pt-1.5 pb-4">{children}</AccordionContent>
    </AccordionItem>
  );
}

interface NumberSettingRowProps {
  id: string;
  spec: NumberFieldSpec;
  description: ReactNode;
  value: string;
  onChange: (text: string) => void;
  onCommit: (raw: string) => void;
  /** 作为折叠区最后一行时传 border-b-0，避免与容器底部分隔线叠成双线。 */
  className?: string;
}

/**
 * 数字参数行：输入期间只改本地草稿文本，失焦 / 回车才提交。
 *
 * 用文本草稿而不是直接写回配置数值，才能区分「清空」与「0」，让空值在失焦时回退原值。
 */
function NumberSettingRow({
  id,
  spec,
  description,
  value,
  onChange,
  onCommit,
  className,
}: NumberSettingRowProps) {
  return (
    <SettingsFieldRow
      className={className}
      label={spec.label}
      description={description}
      htmlFor={id}
      operational
    >
      <Input
        id={id}
        className="w-full sm:w-48"
        type="number"
        min={spec.min}
        max={spec.max}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={(e) => onCommit(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
        }}
      />
    </SettingsFieldRow>
  );
}

function formatTime(ts: number): string {
  try {
    return new Date(ts).toLocaleString("zh-CN", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return String(ts);
  }
}

function logLabel(result: string): { text: string; tone: "success" | "warning" | "error" } {
  switch (result) {
    case "success":
      return { text: "签到成功", tone: "success" };
    case "already":
      return { text: "已签到", tone: "warning" };
    default:
      return { text: "失败", tone: "error" };
  }
}

/** "HH:MM" → 当日分钟数；非法返回 null。 */
function clockMinutes(value: string): number | null {
  const match = /^(\d{1,2}):(\d{2})$/.exec(value);
  if (!match) return null;
  const hour = Number(match[1]);
  const minute = Number(match[2]);
  if (hour > 23 || minute > 59) return null;
  return hour * 60 + minute;
}

/**
 * 签到时间段的非法组合说明；返回 null 表示可以落盘（两端都合法或两端都空）。
 *
 * 半填 / 非法组合只改本地显示、不写盘：后端把这种窗口当「不限制」处理，写盘会让下一轮
 * 对全部账号发起窗口外签到，所以文案要说明「未保存」。
 */
function checkinWindowIssue(start: string, end: string): string | null {
  if (!start && !end) return null;
  if (!start || !end) return "开始与结束时间需同时选择，当前选择未保存";
  const startMinutes = clockMinutes(start);
  const endMinutes = clockMinutes(end);
  if (startMinutes === null || endMinutes === null) return "时间格式应为 HH:MM，当前选择未保存";
  if (startMinutes >= endMinutes) return "结束时间需晚于开始时间（不支持跨午夜），当前选择未保存";
  return null;
}

/** 数字参数：展示名 + 收敛边界（与输入框 min/max 同源）。 */
interface NumberFieldSpec {
  label: string;
  min: number;
  max?: number;
}

/** 数字输入收敛：空 / 非数字返回 null（调用方回退到已落盘值），越界收敛到边界。 */
function resolveNumberInput(raw: string, field: NumberFieldSpec): number | null {
  const text = raw.trim();
  if (!text) return null;
  const parsed = Number(text);
  if (!Number.isFinite(parsed)) return null;
  const rounded = Math.round(parsed);
  const max = field.max ?? Number.POSITIVE_INFINITY;
  return Math.min(Math.max(rounded, field.min), max);
}

/** 空值 / 非数字的回退提示。 */
function numberInputHint(field: NumberFieldSpec): string {
  return field.max === undefined
    ? `${field.label}需为不小于 ${field.min} 的数字，已恢复为原值`
    : `${field.label}需为 ${field.min}–${field.max} 之间的数字，已恢复为原值`;
}

/** 待提交的一次落盘：编辑字段 + 成功后提示（连续编辑合并时提示取最新一次）。 */
interface SaveCommit<T> {
  edits: Partial<T>;
  success?: { title: string; description?: string };
}

/** 签到数字参数（后端不做范围校验，前端按这里的边界收敛后提交）。 */
const CHECKIN_NUMBER_FIELDS = {
  keepalive_days: { label: "保活阈值", min: 0, max: 90 },
  lazy_refresh_hours: { label: "惰性刷新", min: 1, max: 72 },
} as const satisfies Record<string, NumberFieldSpec>;

type CheckinNumberKey = keyof typeof CHECKIN_NUMBER_FIELDS;

/** 轮换数字参数。 */
const ROTATE_NUMBER_FIELDS = {
  check_interval_minutes: { label: "检查间隔", min: 1, max: 1440 },
  cooldown_minutes: { label: "切换冷却", min: 1, max: 1440 },
  min_gap_hours: { label: "到期差异阈值", min: 0, max: 720 },
  min_urgency_hours: { label: "到期紧迫阈值", min: 0, max: 720 },
  min_remaining_credits: { label: "最小剩余积分", min: 0 },
} as const satisfies Record<string, NumberFieldSpec>;

type RotateNumberKey = keyof typeof ROTATE_NUMBER_FIELDS;

/** 自动签到配置 + 一键签到 + 日志（含自动旅行行）。 */
function AutoCheckinCard() {
  /** 自动旅行与自动签到同卡，按档位决定是否渲染该行（国际版无成长中心）。 */
  const variant = useAccountsStore((s) => s.variant);
  const accounts = useAccountsStore((s) => s.accounts);
  const fetchAll = useAccountsStore((s) => s.fetchAll);
  const [cfg, setCfg] = useState<CheckinConfig | null>(null);
  /** 显示草稿的同步镜像：事件回调与异步回读都要读最新值，state 只负责渲染。 */
  const draftRef = useRef<CheckinConfig | null>(null);
  /** 最近一次落盘的配置：即时落盘以它为提交基准，未落盘的草稿不写盘。 */
  const savedRef = useRef<CheckinConfig | null>(null);
  /** 待提交编辑：同一次交互里的连续触发合并为一份最新快照。 */
  const pendingRef = useRef<SaveCommit<CheckinConfig> | null>(null);
  /** 提交链：串行发送，避免先发的那份（不含后一次编辑）后到达覆盖新值。 */
  const chainRef = useRef<Promise<void>>(Promise.resolve());
  /** 数字输入框草稿文本：只覆盖正在编辑的字段，失焦提交后清空。 */
  const [numDraft, setNumDraft] = useState<Partial<Record<CheckinNumberKey, string>>>({});
  /**
   * 手风琴展开的面板：参数区 / 逐账号开关 / 签到日志。
   *
   * 用 multiple 而非 single：三块内容彼此独立，用户可能同时对照参数与账号名单。
   */
  const [openSections, setOpenSections] = useState<string[]>([]);
  const logsOpen = openSections.includes("logs");
  const [logs, setLogs] = useState<CheckinLog[] | null>(null);
  const [logsError, setLogsError] = useState("");
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void loadConfig();
  }, []);

  // 账号列表来自全局 store：为空时补拉一次（只取状态与账号，不拉积分）。
  useEffect(() => {
    if (accounts.length === 0) void fetchAll();
  }, [accounts.length, fetchAll]);

  // 日志懒加载：展开时才请求，每次展开重新拉取；收起态无可见列表，不请求。
  useEffect(() => {
    if (!logsOpen) return;
    let cancelled = false;
    void loadLogs(() => cancelled);
    return () => {
      cancelled = true;
    };
  }, [logsOpen]);

  async function loadConfig() {
    try {
      const loaded = await api.getAutoCheckinConfig();
      savedRef.current = loaded;
      adoptSaved(loaded);
    } catch (e) {
      toast.error("自动签到配置加载失败", { description: api.asError(e) });
    }
  }

  /** 更新显示草稿（state 供渲染，ref 供事件回调与异步回读读最新值）。 */
  function updateCfg(next: CheckinConfig) {
    draftRef.current = next;
    setCfg(next);
  }

  /** 落盘值回填显示：保留半填 / 非法时间段草稿，那是刻意不落盘的本地状态。 */
  function adoptSaved(next: CheckinConfig) {
    const draft = draftRef.current;
    if (draft && checkinWindowIssue(draft.checkin_start, draft.checkin_end) !== null) {
      updateCfg({ ...next, checkin_start: draft.checkin_start, checkin_end: draft.checkin_end });
      return;
    }
    updateCfg(next);
  }

  /**
   * 即时落盘：编辑先合并成一份最新快照，再串行提交。
   *
   * 同一次交互里开关 click 与输入框 blur 会先后触发，各自都带整份配置；并发发送时
   * 先发的那份（不含后一次编辑）可能后到达，把新值覆盖回去。
   */
  function enqueueCheckinSave(
    edits: Partial<CheckinConfig>,
    success?: SaveCommit<CheckinConfig>["success"],
  ) {
    if (!savedRef.current) return;
    const pending = pendingRef.current;
    pendingRef.current = { edits: { ...pending?.edits, ...edits }, success: success ?? pending?.success };
    setSaving(true);
    chainRef.current = chainRef.current.then(flushCheckinSave);
  }

  async function flushCheckinSave() {
    const commit = pendingRef.current;
    pendingRef.current = null;
    const saved = savedRef.current;
    if (!commit || !saved) return;
    try {
      const next = await api.saveAutoCheckinConfig({ ...saved, ...commit.edits });
      savedRef.current = next;
      // 回读值只在没有更新编辑排队时才整体覆盖显示，避免顶掉刚做出的改动。
      if (!pendingRef.current) adoptSaved(next);
      if (commit.success) {
        toast.success(commit.success.title, { description: commit.success.description });
      }
    } catch (e) {
      adoptSaved(saved);
      toast.error("自动签到设置保存失败", { description: api.asError(e) });
    } finally {
      if (!pendingRef.current) setSaving(false);
    }
  }

  /** 主开关：拨动即落盘，失败回滚到上一份已确认配置。 */
  function onToggleEnabled(enabled: boolean) {
    const current = draftRef.current;
    if (!current) return;
    updateCfg({ ...current, enabled });
    enqueueCheckinSave({ enabled }, { title: enabled ? "自动签到已开启" : "自动签到已关闭" });
  }

  /** 数字参数：失焦 / 回车提交；空值回退原值，越界收敛，与落盘值相同则不发请求。 */
  function onNumberCommit(key: CheckinNumberKey, raw: string) {
    const current = draftRef.current;
    const saved = savedRef.current;
    if (!current || !saved) return;
    const field = CHECKIN_NUMBER_FIELDS[key];
    const value = resolveNumberInput(raw, field);
    clearNumDraft(key);
    const next = { ...current };
    next[key] = value ?? saved[key];
    updateCfg(next);
    if (value === null) {
      toast.error(numberInputHint(field));
      return;
    }
    if (value === saved[key]) return;
    const edits: Partial<CheckinConfig> = {};
    edits[key] = value;
    enqueueCheckinSave(edits, { title: `${field.label}已保存` });
  }

  /**
   * 时间段：先只改本地显示；仅「两端都填且 start < end」或「两端都清空」才落盘。
   * 半填 / 非法组合不写盘（后端把这种窗口当「不限制」，写盘会让下一轮对全部账号发起签到）。
   */
  function onWindowChange(key: "checkin_start" | "checkin_end", value: string) {
    const current = draftRef.current;
    const saved = savedRef.current;
    if (!current || !saved) return;
    const next = { ...current };
    next[key] = value;
    updateCfg(next);
    if (checkinWindowIssue(next.checkin_start, next.checkin_end) !== null) return;
    if (next.checkin_start === saved.checkin_start && next.checkin_end === saved.checkin_end) return;
    enqueueCheckinSave(
      { checkin_start: next.checkin_start, checkin_end: next.checkin_end },
      next.checkin_start
        ? { title: "签到时间段已保存", description: `${next.checkin_start} 至 ${next.checkin_end}` }
        : { title: "已清除签到时间段限制" },
    );
  }

  /** 「清除」按钮：两端清空属完整语义，立即落盘。 */
  function clearWindow() {
    const current = draftRef.current;
    const saved = savedRef.current;
    if (!current || !saved) return;
    updateCfg({ ...current, checkin_start: "", checkin_end: "" });
    if (!saved.checkin_start && !saved.checkin_end) return;
    enqueueCheckinSave(
      { checkin_start: "", checkin_end: "" },
      { title: "已清除签到时间段限制" },
    );
  }

  async function loadLogs(isCancelled?: () => boolean) {
    setLogsError("");
    try {
      const res = await api.getCheckinLogs();
      if (!isCancelled?.()) setLogs(res.logs);
    } catch (e) {
      if (isCancelled?.()) return;
      setLogs(null);
      setLogsError(api.asError(e));
    }
  }

  async function checkinAllNow() {
    if (busy || saving) return;
    setBusy(true);
    try {
      const res = await api.checkinAll();
      if (res.status === "skipped" && res.reason === "already_running") {
        toast.error("签到任务正在进行，请稍后再试");
        return;
      }
      const ok = res.accounts.filter((a) => a.result === "success").length;
      const already = res.accounts.filter((a) => a.result === "already").length;
      const err = res.accounts.filter((a) => a.result === "error").length;
      const skipped = res.accounts.filter(
        (a) => a.result === "skipped" && a.reason === "auto_checkin_disabled",
      ).length;
      const detail = res.accounts
        .filter((a) => a.result === "error")
        .map((a) => `${a.email}（${a.error}）`)
        .join("；");
      const text = `签到完成：成功 ${ok}，已签 ${already}，失败 ${err}，已忽略 ${skipped} 个关闭自动签到的账号${detail ? `。${detail}` : ""}`;
      const allFailed = res.accounts.length > 0 && err === res.accounts.length;
      const allSkippedOrUnavailable =
        res.accounts.length === 0 || (ok === 0 && already === 0 && err === 0);
      if (allFailed) toast.error(text);
      else if (allSkippedOrUnavailable) toast.info(text);
      else toast.success(text);
      void loadConfig();
      if (logsOpen) void loadLogs();
    } catch (e) {
      toast.error("签到失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 逐账号开关：切换即落盘，提交最新一份名单。
   *
   * 与参数编辑共用同一条提交链，名单从当前显示草稿派生（连续切换不会互相覆盖），
   * 失败由提交链统一回滚并提示。
   */
  function onAutoCheckinChange(account: AccountMeta, allowed: boolean) {
    const current = draftRef.current;
    if (!current) return;
    const excluded = new Set(current.excluded_account_ids ?? []);
    if (allowed) excluded.delete(account.id);
    else excluded.add(account.id);
    const next = [...excluded];
    updateCfg({ ...current, excluded_account_ids: next });
    enqueueCheckinSave({ excluded_account_ids: next });
  }

  /** 数字输入框：输入期间只改本地草稿文本，失焦 / 回车才提交。 */
  function onNumberChange(key: CheckinNumberKey, text: string) {
    setNumDraft((prev) => ({ ...prev, [key]: text }));
  }

  function clearNumDraft(key: CheckinNumberKey) {
    setNumDraft((prev) => {
      const next = { ...prev };
      delete next[key];
      return next;
    });
  }

  /** 逐账号开关只列出支持签到的档位（国际版签到接口未开放）。 */
  const checkinAccounts = accounts.filter((account) => variantSupportsCheckin(accountVariant(account)));
  const excludedIds = new Set(cfg?.excluded_account_ids ?? []);
  const windowIssue = cfg ? checkinWindowIssue(cfg.checkin_start, cfg.checkin_end) : null;

  return (
    <SettingsGroup
      id="settings-auto-checkin"
      title="自动签到"
    >
      <CardContent className="space-y-0 p-0">
        <Accordion type="multiple" value={openSections} onValueChange={setOpenSections}>
          {cfg ? (
            <>
              <AccordionSettingsRow
                value="params"
                label="启用自动签到"
                description="为允许自动签到的账号核验状态并补签；可在下方按账号关闭"
                actions={
                  <>
                    <DemoAction>
                      <Switch
                        aria-label="启用自动签到"
                        checked={cfg.enabled}
                        onCheckedChange={onToggleEnabled}
                      />
                    </DemoAction>
                    <DemoAction>
                      <Button size="sm" variant="outline" onClick={checkinAllNow} disabled={busy || saving}>
                        {busy ? <Loader2 className="animate-spin" /> : <CircleCheck />}全部立即签到
                      </Button>
                    </DemoAction>
                  </>
                }
              >
                <div className={INSET_PANEL}>
                  <SettingsFieldRow
                    className={PANEL_ROW}
                    label="签到时间段"
                    description={
                      <>
                        留空为不限制。设置后每天在窗口内随机时刻自动签到。
                        <span className="mt-0.5 block">需 App 在窗口附近运行才能按时执行。</span>
                      </>
                    }
                  >
                    <div className="flex min-w-0 w-full flex-col items-end gap-1 sm:w-auto">
                      <div className="flex min-w-0 w-full flex-wrap items-center justify-end gap-2 sm:w-auto">
                        <DemoAction className="min-w-0 flex-1 sm:flex-none">
                          <TimePicker
                            className="min-w-0 flex-1 sm:flex-none"
                            value={cfg.checkin_start}
                            hourLabel="签到开始时间（小时）"
                            minuteLabel="签到开始时间（分钟）"
                            onChange={(v) => onWindowChange("checkin_start", v)}
                          />
                        </DemoAction>
                        <span className="shrink-0 text-xs text-muted-foreground">至</span>
                        <DemoAction className="min-w-0 flex-1 sm:flex-none">
                          <TimePicker
                            className="min-w-0 flex-1 sm:flex-none"
                            value={cfg.checkin_end}
                            hourLabel="签到结束时间（小时）"
                            minuteLabel="签到结束时间（分钟）"
                            onChange={(v) => onWindowChange("checkin_end", v)}
                          />
                        </DemoAction>
                        {(cfg.checkin_start || cfg.checkin_end) && (
                          <DemoAction>
                            <Button
                              size="sm"
                              variant="ghost"
                              className="shrink-0"
                              onClick={clearWindow}
                            >
                              清除
                            </Button>
                          </DemoAction>
                        )}
                      </div>
                      {windowIssue && (
                        <p className="text-xs leading-4 text-amber-600">{windowIssue}</p>
                      )}
                    </div>
                  </SettingsFieldRow>

                  <NumberSettingRow
                    className={PANEL_ROW}
                    id="ac-keep"
                    spec={CHECKIN_NUMBER_FIELDS.keepalive_days}
                    description="天；0 表示每天无条件刷新"
                    value={numDraft.keepalive_days ?? String(cfg.keepalive_days)}
                    onChange={(text) => onNumberChange("keepalive_days", text)}
                    onCommit={(raw) => onNumberCommit("keepalive_days", raw)}
                  />
                  <NumberSettingRow
                    className={cn(PANEL_ROW, "border-b-0")}
                    id="ac-lazy"
                    spec={CHECKIN_NUMBER_FIELDS.lazy_refresh_hours}
                    description="小时"
                    value={numDraft.lazy_refresh_hours ?? String(cfg.lazy_refresh_hours)}
                    onChange={(text) => onNumberChange("lazy_refresh_hours", text)}
                    onCommit={(raw) => onNumberCommit("lazy_refresh_hours", raw)}
                  />
                </div>
              </AccordionSettingsRow>

              {/* 逐账号开关：切换即落盘，与参数编辑共用同一条提交链。 */}
              <AccordionSettingsRow
                value="excluded"
                label="不参与自动签到的账号"
                description="关闭后，后台轮次、刷新积分与全部立即签到都会跳过该账号；仍可在账号卡片进行单账号手动签到。"
              >
                <div className={INSET_PANEL}>
                  {checkinAccounts.length === 0 ? (
                    <p className="py-2 text-xs text-muted-foreground">暂无可签到的账号</p>
                  ) : (
                    checkinAccounts.map((account) => {
                      const name = account.nickname || account.email || account.uid || account.id;
                      return (
                        <div
                          key={account.id}
                          className="flex min-w-0 items-center justify-between gap-3 border-b border-border/50 py-2.5 last:border-b-0"
                        >
                          <span className="min-w-0 flex-1 truncate text-xs leading-4" title={name}>
                            {name}
                          </span>
                          <DemoAction>
                            <Switch
                              checked={!excludedIds.has(account.id)}
                              onCheckedChange={(allowed) => onAutoCheckinChange(account, allowed)}
                              aria-label={`${name}参与自动签到`}
                            />
                          </DemoAction>
                        </div>
                      );
                    })
                  )}
                </div>
              </AccordionSettingsRow>
            </>
          ) : (
            <p className="px-4 py-3 text-sm text-muted-foreground sm:px-5">加载配置中…</p>
          )}

          {/* 成长中心（派猫猫旅行）仅国内版开放：国际版不渲染该行，也不请求其配置。
              行本身独立于签到配置的加载状态，签到配置读取失败也不影响开关。 */}
          {variantSupportsTravel(variant) ? <AutoTravelRow /> : null}

          <AccordionSettingsRow
            value="logs"
            label="签到日志"
            description="保留最近 30 天；本机明文保存，可能含账号昵称。"
            divider={false}
          >
            <div className={INSET_PANEL}>
              {logsError ? (
                <p className="py-2 text-xs text-destructive">{logsError}</p>
              ) : !logs ? (
                <p className="py-2 text-xs text-muted-foreground">正在读取…</p>
              ) : logs.length === 0 ? (
                <p className="py-2 text-xs text-muted-foreground">暂无签到记录</p>
              ) : (
                <div className="max-h-64 overflow-y-auto pr-1">
                  {[...logs].reverse().map((l, i) => {
                    const tone = logLabel(l.result);
                    return (
                      <div
                        key={i}
                        className="flex items-center justify-between border-b border-border/50 py-2 text-xs last:border-b-0"
                      >
                        <div className="min-w-0 flex-1 truncate">
                          <span className="font-medium">{l.email}</span>
                          {l.error && <span className="text-destructive">（{l.error}）</span>}
                        </div>
                        <div className="ml-2 flex shrink-0 items-center gap-2">
                          <span
                            className={
                              tone.tone === "error"
                                ? "text-destructive"
                                : tone.tone === "warning"
                                  ? "text-amber-600"
                                  : "text-emerald-600"
                            }
                          >
                            {tone.text}
                          </span>
                          <span className="text-muted-foreground">{formatTime(l.ts)}</span>
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          </AccordionSettingsRow>
        </Accordion>
      </CardContent>
    </SettingsGroup>
  );
}

/**
 * 自动旅行：单行开关，渲染在「自动签到」卡片内部（不独立成卡）。
 *
 * 成长中心仅国内版开放，调用方按档位决定是否渲染；开关切换即落盘（无「保存配置」按钮），
 * 失败回滚并提示。
 */
function AutoTravelRow() {
  const [cfg, setCfg] = useState<TravelConfig | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void api
      .getAutoTravelConfig()
      .then((config) => {
        if (!cancelled) setCfg(config);
      })
      .catch((e) => {
        if (!cancelled) toast.error("自动旅行配置加载失败", { description: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onToggle(enabled: boolean) {
    if (!cfg || busy) return;
    const previous = cfg;
    setCfg({ ...cfg, enabled });
    setBusy(true);
    try {
      setCfg(await api.saveAutoTravelConfig({ ...cfg, enabled }));
      if (enabled) {
        toast.success("自动旅行已开启", { description: "正在按官方状态派发或领取" });
      } else {
        toast.success("自动旅行已关闭");
      }
    } catch (e) {
      setCfg(previous);
      toast.error("自动旅行设置保存失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <SettingsFieldRow
      label="启用自动旅行"
      description="开启后按官方状态自动派发或领取旅行奖励；切换后立即生效"
      htmlFor="at-enabled"
      operational
    >
      <Switch
        id="at-enabled"
        checked={cfg?.enabled ?? false}
        disabled={busy || !cfg}
        onCheckedChange={(v) => void onToggle(v)}
        aria-label="启用自动旅行"
      />
    </SettingsFieldRow>
  );
}

/** 自动轮换配置（CodeBuddy CLI）+ 手动检查 + 日志。 */
function AutoRotateCard() {
  const [cfg, setCfg] = useState<AutoRotateConfig | null>(null);
  const [status, setStatus] = useState<RotateStatus | null>(null);
  /** 显示草稿的同步镜像：事件回调与异步回读都要读最新值，state 只负责渲染。 */
  const draftRef = useRef<AutoRotateConfig | null>(null);
  /** 最近一次落盘的配置：即时落盘以它为提交基准。 */
  const savedRef = useRef<AutoRotateConfig | null>(null);
  /** 待提交编辑：同一次交互里的连续触发合并为一份最新快照。 */
  const pendingRef = useRef<SaveCommit<AutoRotateConfig> | null>(null);
  /** 提交链：串行发送，避免先发的那份（不含后一次编辑）后到达覆盖新值。 */
  const chainRef = useRef<Promise<void>>(Promise.resolve());
  /** 数字输入框草稿文本：只覆盖正在编辑的字段，失焦提交后清空。 */
  const [numDraft, setNumDraft] = useState<Partial<Record<RotateNumberKey, string>>>({});
  /** 手风琴展开的面板：参数区 / 轮换日志，默认全部收起；开关行与操作行常驻。 */
  const [openSections, setOpenSections] = useState<string[]>([]);
  const logsOpen = openSections.includes("logs");
  const [logs, setLogs] = useState<RotateLog[] | null>(null);
  const [logsError, setLogsError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void loadConfig();
  }, []);

  // 日志懒加载：展开时才请求，每次展开重新拉取；收起态无可见列表，不请求。
  useEffect(() => {
    if (!logsOpen) return;
    let cancelled = false;
    void loadLogs(() => cancelled);
    return () => {
      cancelled = true;
    };
  }, [logsOpen]);

  async function loadConfig() {
    try {
      const [c, s] = await Promise.all([api.getAutoRotateConfig(), api.getRotateStatus()]);
      savedRef.current = c;
      updateCfg(c);
      setStatus(s);
    } catch (e) {
      toast.error("自动轮换配置加载失败", { description: api.asError(e) });
    }
  }

  async function loadLogs(isCancelled?: () => boolean) {
    setLogsError("");
    try {
      const res = await api.getRotateLogs();
      if (!isCancelled?.()) setLogs(res.logs);
    } catch (e) {
      if (isCancelled?.()) return;
      setLogs(null);
      setLogsError(api.asError(e));
    }
  }

  /** 更新显示草稿（state 供渲染，ref 供事件回调与异步回读读最新值）。 */
  function updateCfg(next: AutoRotateConfig) {
    draftRef.current = next;
    setCfg(next);
  }

  /** 即时落盘：编辑先合并成一份最新快照，再串行提交（失败回滚到已确认配置）。 */
  function enqueueRotateSave(
    edits: Partial<AutoRotateConfig>,
    success?: SaveCommit<AutoRotateConfig>["success"],
  ) {
    if (!savedRef.current) return;
    const pending = pendingRef.current;
    pendingRef.current = { edits: { ...pending?.edits, ...edits }, success: success ?? pending?.success };
    chainRef.current = chainRef.current.then(flushRotateSave);
  }

  async function flushRotateSave() {
    const commit = pendingRef.current;
    pendingRef.current = null;
    const saved = savedRef.current;
    if (!commit || !saved) return;
    try {
      const next = await api.saveAutoRotateConfig({ ...saved, ...commit.edits });
      savedRef.current = next;
      // 回读值只在没有更新编辑排队时才覆盖显示，避免顶掉刚做出的改动。
      if (!pendingRef.current) updateCfg(next);
      if (commit.success) {
        toast.success(commit.success.title, { description: commit.success.description });
      }
    } catch (e) {
      updateCfg(saved);
      toast.error("自动轮换设置保存失败", { description: api.asError(e) });
    }
  }

  /** 主开关：拨动即落盘，失败回滚到上一份已确认配置。 */
  function onToggleEnabled(enabled: boolean) {
    const current = draftRef.current;
    if (!current) return;
    updateCfg({ ...current, enabled });
    enqueueRotateSave({ enabled }, { title: enabled ? "自动轮换已开启" : "自动轮换已关闭" });
  }

  /** 数字参数：失焦 / 回车提交；空值回退原值，越界收敛，与落盘值相同则不发请求。 */
  function onNumberCommit(key: RotateNumberKey, raw: string) {
    const current = draftRef.current;
    const saved = savedRef.current;
    if (!current || !saved) return;
    const field = ROTATE_NUMBER_FIELDS[key];
    const value = resolveNumberInput(raw, field);
    clearNumDraft(key);
    const next = { ...current };
    next[key] = value ?? saved[key];
    updateCfg(next);
    if (value === null) {
      toast.error(numberInputHint(field));
      return;
    }
    if (value === saved[key]) return;
    const edits: Partial<AutoRotateConfig> = {};
    edits[key] = value;
    enqueueRotateSave(edits, { title: `${field.label}已保存` });
  }

  /** 数字输入框：输入期间只改本地草稿文本，失焦 / 回车才提交。 */
  function onNumberChange(key: RotateNumberKey, text: string) {
    setNumDraft((prev) => ({ ...prev, [key]: text }));
  }

  function clearNumDraft(key: RotateNumberKey) {
    setNumDraft((prev) => {
      const next = { ...prev };
      delete next[key];
      return next;
    });
  }

  async function runNow() {
    setBusy(true);
    try {
      const res = await api.runRotate();
      // webui 没有事件通道：手动检查的推迟提示只能从返回值里取（桌面端由
      // `rotate-deferred` 事件统一弹出，避免同一件事弹两次）。
      if (api.isWebui() && res.notify?.body) {
        toast.warning("自动轮换已推迟", { description: res.notify.body, duration: 10_000 });
      }
      const text =
        res.status === "switched"
          ? `已切换到 ${res.to ?? "目标账号"}`
          : res.status === "disabled"
            ? "自动轮换未启用（请在下方开启后重试）"
            : (res.reason ?? `检查完成：${res.status}`);
      if (res.status === "error") toast.error(text);
      else toast.success(text);
      void loadConfig();
      if (logsOpen) void loadLogs();
    } catch (e) {
      toast.error("轮换检查失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  function actionLabel(action: string): { text: string; tone: "success" | "warning" | "error" } {
    switch (action) {
      case "switched":
        return { text: "已切换", tone: "success" };
      case "skipped":
        return { text: "未切换", tone: "warning" };
      case "disabled":
        return { text: "未启用", tone: "warning" };
      case "error":
        return { text: "出错", tone: "error" };
      default:
        return { text: action, tone: "warning" };
    }
  }

  return (
    <SettingsGroup
      id="settings-auto-rotate"
      title="CodeBuddy CLI 自动轮换"
    >
      <CardContent className="space-y-0 p-0">
        {status && (
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-border/60 bg-muted/25 px-4 py-3 text-xs text-muted-foreground sm:px-5">
            <span>
              当前 CLI 账号：
              <b className="text-foreground">{status.activeAccountName ?? "未配置"}</b>
            </span>
            {status.lastCheckAt && <span>上次检查 {formatTime(status.lastCheckAt)}</span>}
            {status.lastSwitchAt && <span>上次切换 {formatTime(status.lastSwitchAt)}</span>}
            {!status.cliConfigured && (
              <span className="text-destructive">未接入 CodeBuddy CLI（请先到账号页安装 helper）</span>
            )}
          </div>
        )}

        <Accordion type="multiple" value={openSections} onValueChange={setOpenSections}>
        {cfg ? (
          <>
            <AccordionSettingsRow
              value="params"
              label="启用自动轮换"
              description="开启后按设定的间隔自动检查并切换 CodeBuddy CLI 账号"
              actions={
                <>
                  <DemoAction>
                    <Switch
                      aria-label="启用自动轮换"
                      checked={cfg.enabled}
                      onCheckedChange={onToggleEnabled}
                    />
                  </DemoAction>
                  <DemoAction>
                    <Button size="sm" variant="outline" onClick={runNow} disabled={busy}>
                      {busy ? <Loader2 className="animate-spin" /> : <RefreshCw />}立即检查一次
                    </Button>
                  </DemoAction>
                </>
              }
            >
              <div className={INSET_PANEL}>
                <NumberSettingRow
                  className={PANEL_ROW}
                  id="ar-interval"
                  spec={ROTATE_NUMBER_FIELDS.check_interval_minutes}
                  description="分钟"
                  value={numDraft.check_interval_minutes ?? String(cfg.check_interval_minutes)}
                  onChange={(text) => onNumberChange("check_interval_minutes", text)}
                  onCommit={(raw) => onNumberCommit("check_interval_minutes", raw)}
                />
                <NumberSettingRow
                  className={PANEL_ROW}
                  id="ar-cooldown"
                  spec={ROTATE_NUMBER_FIELDS.cooldown_minutes}
                  description="分钟"
                  value={numDraft.cooldown_minutes ?? String(cfg.cooldown_minutes)}
                  onChange={(text) => onNumberChange("cooldown_minutes", text)}
                  onCommit={(raw) => onNumberCommit("cooldown_minutes", raw)}
                />
                <NumberSettingRow
                  className={PANEL_ROW}
                  id="ar-gap"
                  spec={ROTATE_NUMBER_FIELDS.min_gap_hours}
                  description="小时"
                  value={numDraft.min_gap_hours ?? String(cfg.min_gap_hours)}
                  onChange={(text) => onNumberChange("min_gap_hours", text)}
                  onCommit={(raw) => onNumberCommit("min_gap_hours", raw)}
                />
                <NumberSettingRow
                  className={PANEL_ROW}
                  id="ar-urgency"
                  spec={ROTATE_NUMBER_FIELDS.min_urgency_hours}
                  description="小时"
                  value={numDraft.min_urgency_hours ?? String(cfg.min_urgency_hours)}
                  onChange={(text) => onNumberChange("min_urgency_hours", text)}
                  onCommit={(raw) => onNumberCommit("min_urgency_hours", raw)}
                />
                <NumberSettingRow
                  className={cn(PANEL_ROW, "border-b-0")}
                  id="ar-min"
                  spec={ROTATE_NUMBER_FIELDS.min_remaining_credits}
                  description="低于此值时不切换"
                  value={numDraft.min_remaining_credits ?? String(cfg.min_remaining_credits)}
                  onChange={(text) => onNumberChange("min_remaining_credits", text)}
                  onCommit={(raw) => onNumberCommit("min_remaining_credits", raw)}
                />
                <p className="pt-1 text-[13px] leading-5 text-muted-foreground">
                  切换时机：目标账号剩余到期时间少于「紧迫阈值」且比当前账号早超过「差异阈值」，且目标剩余积分不低于「最小剩余积分」。检测到有 CodeBuddy CLI 会话在运行时，本次轮换会跳过并在当日最多提示 5 次；重启 CLI 后新账号才会生效。
                </p>
              </div>
            </AccordionSettingsRow>
          </>
        ) : (
          <p className="px-4 py-3 text-sm text-muted-foreground sm:px-5">加载配置中…</p>
        )}

        <AccordionSettingsRow
          value="logs"
          label="轮换日志"
          description="保留最近 200 条；本机明文保存，可能含账号昵称。"
          divider={false}
        >
          <div className={INSET_PANEL}>
            {logsError ? (
              <p className="py-2 text-xs text-destructive">{logsError}</p>
            ) : !logs ? (
              <p className="py-2 text-xs text-muted-foreground">正在读取…</p>
            ) : logs.length === 0 ? (
              <p className="py-2 text-xs text-muted-foreground">暂无轮换记录</p>
            ) : (
              <div className="max-h-64 overflow-y-auto pr-1">
                {logs.map((l, i) => {
                  const tone = actionLabel(l.action);
                  return (
                    <div
                      key={i}
                      className="flex items-center justify-between border-b border-border/50 py-2 text-xs last:border-b-0"
                    >
                      <div className="min-w-0 flex-1 truncate">
                        {l.action === "switched" && l.from && l.to && (
                          <span className="font-medium">
                            {l.from.name ?? l.from.id} → {l.to.name ?? l.to.id}
                          </span>
                        )}
                        {l.reason && <span className="text-muted-foreground">（{l.reason}）</span>}
                      </div>
                      <div className="ml-2 flex shrink-0 items-center gap-2">
                        <span
                          className={
                            tone.tone === "error"
                              ? "text-destructive"
                              : tone.tone === "success"
                                ? "text-emerald-600"
                                : "text-amber-600"
                          }
                        >
                          {tone.text}
                        </span>
                        <span className="text-muted-foreground">{formatTime(l.ts)}</span>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </AccordionSettingsRow>
        </Accordion>
      </CardContent>
    </SettingsGroup>
  );
}

/** 权限检测卡片：确认本 App 是否有权写入 WorkBuddy 认证文件（探针与展示路径同档位）。 */
function PermissionCheckCard() {
  const authFile = useAuthFile();
  const variant = useAccountsStore((s) => s.variant);
  const [checking, setChecking] = useState(false);
  /** 只在失败时留在卡片内：错误文案与授权四步引导不该被几秒的 toast 吞掉。 */
  const [error, setError] = useState<string | null>(null);

  async function runCheck() {
    setChecking(true);
    setError(null);
    try {
      const res = await api.checkAuthPermission(variant);
      if (res.ok) {
        toast.success(res.message ?? "认证目录可写，权限正常");
      } else {
        setError(`${res.error}（${res.dir ?? ""}）`);
      }
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setChecking(false);
    }
  }

  return (
    <SettingsGroup
      id="settings-permission"
      title="权限检测"
    >
      <CardContent className="space-y-0 p-0">
        <div className="break-all border-b border-border/60 bg-muted/25 px-4 py-3 font-mono text-[11px] leading-5 text-muted-foreground sm:px-5">
          {authFile || "认证文件路径未获取"}
        </div>
        <div className="flex flex-wrap gap-2 border-b-0 border-border/60 px-4 py-3 sm:px-5">
          <DemoAction><Button size="sm" onClick={runCheck} disabled={checking}>
            {checking ? "检测中…" : "检测权限"}
          </Button></DemoAction>
          <DemoAction><Button
            size="sm"
            variant="outline"
            onClick={() => void api.openPermissionSettings("all_files")}
          >
            打开完全磁盘访问
          </Button></DemoAction>
          <DemoAction><Button
            size="sm"
            variant="outline"
            onClick={() => void api.openPermissionSettings("app_management")}
          >
            打开 App 管理
          </Button></DemoAction>
          <DemoAction><Button size="sm" variant="outline" onClick={() => void api.revealAppInFinder()}>
            在 Finder 中显示
          </Button></DemoAction>
        </div>

        {error && (
          <Alert variant="destructive" className="!w-auto mx-4 my-4 sm:mx-5">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        {error && (
          <div className="mx-4 mb-4 border-l-2 border-destructive/50 bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground sm:mx-5">
            <p className="mb-1 font-medium text-foreground">如何授权（拖拽方式）：</p>
            <ol className="list-decimal space-y-1 pl-4">
              <li>点上方「打开完全磁盘访问」</li>
              <li>再点「在 Finder 中显示」打开 workbuddy-switch 所在位置</li>
              <li>
                把 <b>workbuddy-switch.app</b> 从 Finder <b>直接拖进</b>完全磁盘访问的列表区域
                （即使没有提示框，拖入即生效），然后打开它的开关
              </li>
              <li>回到本页点「检测权限」，或直接重试切换</li>
            </ol>
          </div>
        )}
      </CardContent>
    </SettingsGroup>
  );
}

function useAuthFile(): string | undefined {
  return useAccountsStore((s) => s.status?.authFile);
}

/** 自动更新：检查公开 GitHub Releases 源 + 安装签名更新。 */
function UpdateCard() {
  const version = useAccountsStore((s) => s.status?.version);
  // 阶段与进度来自 Rust 更新服务（托盘同源）：下载完成时按钮换成「重启以完成升级」。
  const snapshot = useUpdateState();
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [checking, setChecking] = useState(false);
  const [installOpen, setInstallOpen] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [githubConfig, setGithubConfig] = useState<GithubConfig>({});
  const [proxyUrl, setProxyUrl] = useState("");
  const [proxySaving, setProxySaving] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void api
      .getGithubConfig()
      .then((config) => {
        if (cancelled) return;
        setGithubConfig(config);
        setProxyUrl(config.proxy ?? "");
      })
      .catch((e) => {
        if (!cancelled) toast.error("更新配置加载失败", { description: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function check() {
    setChecking(true);
    try {
      const r = await api.checkUpdate(proxyUrl, true);
      setInfo(r);
      if (!r.ok) {
        toast.error("检查更新失败", { description: r.message || r.error });
      }
    } catch (e) {
      toast.error("检查更新失败", { description: api.asError(e) });
    } finally {
      setChecking(false);
    }
  }

  /** 安装已下载的更新包并重启（下载完成后的主动作，与托盘菜单同一入口）。 */
  async function restartNow() {
    setRestarting(true);
    try {
      await api.updateRestart();
    } catch (e) {
      setRestarting(false);
      toast.error("重启失败", { description: api.asError(e) });
    }
  }

  async function saveProxy() {
    const value = proxyUrl.trim();
    if (value) {
      try {
        const parsed = new URL(value);
        if (!parsed.hostname || !["http:", "https:"].includes(parsed.protocol)) {
          throw new Error("unsupported proxy protocol");
        }
      } catch {
        toast.error("代理地址格式不正确，请填写 HTTP/HTTPS 地址，例如 http://127.0.0.1:7897");
        return;
      }
    }

    setProxySaving(true);
    try {
      const saved = await api.saveGithubConfig({ ...githubConfig, proxy: value });
      setGithubConfig(saved);
      setProxyUrl(saved.proxy ?? "");
      toast.success(value ? "更新代理已保存" : "已关闭更新代理");
    } catch (e) {
      toast.error("保存代理失败", { description: api.asError(e) });
    } finally {
      setProxySaving(false);
    }
  }

  return (
    <SettingsGroup
      id="settings-updates"
      title="自动更新"
    >
      <CardContent className="space-y-0 p-0">
        <div className="border-b border-border/60 px-4 py-3 text-sm sm:px-5">
          当前版本：<span className="font-mono">v{version || "?"}</span>
        </div>

        <div className="flex min-w-0 items-center justify-between gap-3 border-b border-border/60 bg-muted/25 px-4 py-3 text-sm sm:px-5">
          <div className="min-w-0 flex-1">
            <div className="font-medium">公开更新源</div>
            <div className="truncate text-xs text-muted-foreground">{GITHUB_REPOSITORY_URL}</div>
          </div>
          <DemoAction><Button
            variant="ghost"
            size="icon"
            title="打开 GitHub Release"
            onClick={() => void openReleaseUrl(GITHUB_RELEASE_URL)}
          >
            <ExternalLink />
          </Button></DemoAction>
        </div>

        <SettingsFieldRow
          label="更新代理地址"
          description="仅用于 GitHub 更新检查和安装包下载；留空表示关闭显式代理。"
          htmlFor="update-proxy"
          className="bg-muted/25"
          operational
        >
          <Input
            id="update-proxy"
            className="w-full sm:w-80"
            value={proxyUrl}
            onChange={(event) => setProxyUrl(event.target.value)}
            placeholder="例如 http://127.0.0.1:7897"
            spellCheck={false}
            autoComplete="off"
          />
        </SettingsFieldRow>

        <div className="flex flex-wrap gap-2 border-b border-border/60 bg-muted/25 px-4 py-3 sm:px-5">
          <DemoAction><Button size="sm" variant="outline" onClick={() => void saveProxy()} disabled={proxySaving}>
            {proxySaving ? <Loader2 className="animate-spin" /> : <Save />}
            保存代理
          </Button></DemoAction>
        </div>

        <div className="flex flex-wrap gap-2 border-b-0 border-border/60 px-4 py-3 sm:px-5">
          <DemoAction><Button size="sm" variant="outline" onClick={check} disabled={checking}>
            {checking ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            检查更新
          </Button></DemoAction>
        </div>

        {info?.ok && (
          <Alert variant="default" className={cn("!w-auto mx-4 my-4 sm:mx-5", info.hasUpdate && "border-primary/35 bg-primary/[0.06]")}>
            {info.hasUpdate && <ArrowUpCircle className="text-primary" />}
            <AlertDescription className="space-y-2">
              <AlertTitle className={cn(info.hasUpdate && "text-primary")}>{info.hasUpdate ? "发现新版本" : "更新检查完成"}</AlertTitle>
              <div className="text-sm">
                {info.hasUpdate
                  ? `发现新版本 v${info.latest}（当前 v${info.current}）`
                  : `已是最新版本 v${info.current}`}
                {info.releaseName && <span className="text-muted-foreground"> · {info.releaseName}</span>}
              </div>
              {info.hasUpdate && (
                <DemoAction>
                  {snapshot.phase === "readyToRestart" ? (
                    <Button size="sm" onClick={() => void restartNow()} disabled={restarting}>
                      {restarting ? <Loader2 className="animate-spin" /> : <RefreshCw />}
                      {restarting ? "正在重启…" : "重启以完成升级"}
                    </Button>
                  ) : (
                    <Button size="sm" onClick={() => setInstallOpen(true)}>
                      <ArrowUpCircle />
                      立即升级
                    </Button>
                  )}
                </DemoAction>
              )}
              {info.releaseUrl && (
                <DemoAction><Button
                  variant="link"
                  size="sm"
                  className="h-auto p-0"
                  onClick={() => void openReleaseUrl(info.releaseUrl)}
                >
                  打开 GitHub Release
                </Button></DemoAction>
              )}
            </AlertDescription>
          </Alert>
        )}
        <UpdateInstallDialog open={installOpen} onOpenChange={setInstallOpen} />
      </CardContent>
    </SettingsGroup>
  );
}

/** 开机自启（仅桌面端渲染）：开关直接反映系统自启注册状态，切换立即生效。 */
function StartupCard() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void api
      .getLaunchAtLoginEnabled()
      .then((value) => {
        if (!cancelled) setEnabled(value);
      })
      .catch((e) => {
        if (!cancelled) toast.error("开机自启状态读取失败", { description: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onToggle(value: boolean) {
    if (busy || enabled === null) return;
    const previous = enabled;
    setBusy(true);
    try {
      // 后端回读 OS 权威状态；即使与请求一致，也以回读值显示。
      const authoritative = await api.setLaunchAtLoginEnabled(value);
      setEnabled(authoritative);
      toast.success(authoritative ? "已开启开机自启" : "已关闭开机自启");
    } catch (e) {
      // 失败时恢复到最后一次确认的状态，并显示可读错误。
      setEnabled(previous);
      toast.error("开机自启设置失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <SettingsGroup
      id="settings-startup"
      title="启动设置"
    >
      <CardContent className="space-y-0 p-0">
        <SettingsFieldRow
          className="border-b-0"
          label="开机时静默启动到托盘"
          description="开关直接反映系统登录项状态；之后可从托盘「打开主界面」恢复"
          htmlFor="startup-silent"
          operational
        >
          <Switch
            id="startup-silent"
            checked={enabled ?? false}
            disabled={busy || enabled === null}
            onCheckedChange={(v) => void onToggle(v)}
            aria-label="开机时静默启动到托盘"
          />
        </SettingsFieldRow>
      </CardContent>
    </SettingsGroup>
  );
}

/** 桌面版 Agent Companion：状态以宿主后端的持久化结果为准。 */
function CompanionCard() {
  const { enabled, busy, error } = useCompanionEnabled();

  async function onToggle(next: boolean) {
    try {
      const confirmed = await changeCompanionEnabled(next);
      toast.success(confirmed ? "已启用 Agent Companion 悬浮窗" : "已关闭 Agent Companion 悬浮窗");
    } catch (cause) {
      toast.error("悬浮窗设置失败", { description: api.asError(cause) });
    }
  }

  async function openSettings() {
    try {
      await api.openCompanionSettings();
    } catch (error) {
      toast.error("打开悬浮窗设置失败", { description: api.asError(error) });
    }
  }

  return (
    <SettingsGroup id="settings-companion" title="Agent Companion">
      <CardContent className="space-y-0 p-0">
        <SettingsFieldRow
          className="border-b-0"
          label="启用会话悬浮窗"
          description="启用后显示悬浮栏；开机静默启动时也会显示，可从托盘临时隐藏"
          htmlFor="companion-enabled"
        >
          <div className="flex items-center gap-2">
            {error && enabled === null ? (
              <Button size="sm" variant="outline" onClick={() => void reloadCompanionEnabled()}>重试</Button>
            ) : null}
            {enabled ? (
              <Button size="sm" variant="outline" onClick={() => void openSettings()}>悬浮窗设置</Button>
            ) : null}
            <Switch
              id="companion-enabled"
              checked={enabled ?? false}
              disabled={busy || enabled === null}
              onCheckedChange={(value) => void onToggle(value)}
              aria-label="启用会话悬浮窗"
            />
          </div>
        </SettingsFieldRow>
      </CardContent>
    </SettingsGroup>
  );
}

/** 外观：主题选择（持久化到 localStorage）。 */
const NOTIFICATION_LEVEL_LABEL: Record<AppNotification["level"], string> = {
  success: "成功",
  error: "错误",
  warning: "警告",
  info: "提示",
};

const NOTIFICATION_LEVEL_DOT: Record<AppNotification["level"], string> = {
  success: "bg-primary",
  error: "bg-destructive",
  warning: "bg-amber-500",
  info: "bg-muted-foreground/60",
};

/** 通知时间：当天只显示时分秒，更早显示完整时间。 */
function formatNotificationTime(at: number): string {
  const date = new Date(at);
  const sameDay = date.toDateString() === new Date().toDateString();
  return sameDay
    ? date.toLocaleTimeString("zh-CN", { hour12: false })
    : date.toLocaleString("zh-CN", { hour12: false });
}

/** 通知历史：最近 100 条应用内提示，供事后核对。 */
function NotificationHistoryCard() {
  /** 手风琴展开的面板：应用内提示存档，默认收起。 */
  const [openSections, setOpenSections] = useState<string[]>([]);
  const open = openSections.includes("history");
  const [items, setItems] = useState<AppNotification[] | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setError("");
    api
      .listNotifications()
      .then((res) => {
        if (!cancelled) setItems(res.items);
      })
      .catch((e) => {
        if (cancelled) return;
        setItems(null);
        setError(api.asError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  async function clearHistory() {
    try {
      await api.clearNotifications();
      setItems([]);
      toast.success("通知历史已清空");
    } catch (e) {
      toast.error("清空通知历史失败", { description: api.asError(e) });
    }
  }

  return (
    <SettingsGroup id="settings-notifications" title="通知历史">
      <CardContent className="space-y-0 p-0">
        <Accordion type="multiple" value={openSections} onValueChange={setOpenSections}>
          <AccordionSettingsRow
            value="history"
            label="应用内提示存档"
            description="保留最近 100 条，便于事后核对；本机明文保存，可能含账号昵称与本地路径。"
            divider={false}
            actions={
              <AlertDialog>
                <AlertDialogTrigger asChild>
                  <Button variant="ghost" size="sm" disabled={!items || items.length === 0}>
                    清空
                  </Button>
                </AlertDialogTrigger>
                <AlertDialogContent>
                  <AlertDialogHeader>
                    <AlertDialogTitle>清空通知历史？</AlertDialogTitle>
                    <AlertDialogDescription>
                      将删除本机保存的全部提示存档，无法恢复。
                    </AlertDialogDescription>
                  </AlertDialogHeader>
                  <AlertDialogFooter>
                    <AlertDialogCancel>取消</AlertDialogCancel>
                    <AlertDialogAction onClick={clearHistory}>清空</AlertDialogAction>
                  </AlertDialogFooter>
                </AlertDialogContent>
              </AlertDialog>
            }
          >
            <div className={INSET_PANEL}>
              {error ? (
                <p className="py-2 text-xs text-destructive">{error}</p>
              ) : !items ? (
                <p className="py-2 text-xs text-muted-foreground">正在读取…</p>
              ) : items.length === 0 ? (
                <p className="py-2 text-xs text-muted-foreground">还没有记录到任何提示。</p>
              ) : (
                <ul className="max-h-72 divide-y divide-border/40 overflow-auto">
                  {items.map((item, index) => (
                    <li key={`${item.at}-${index}`} className="py-1.5">
                      <div className="flex items-center gap-1.5 text-[11px] leading-4 text-muted-foreground">
                        <span
                          className={cn(
                            "size-1.5 shrink-0 rounded-full",
                            NOTIFICATION_LEVEL_DOT[item.level],
                          )}
                          aria-hidden
                        />
                        <span>{NOTIFICATION_LEVEL_LABEL[item.level]}</span>
                        <span aria-hidden>·</span>
                        <span>{formatNotificationTime(item.at)}</span>
                      </div>
                      <div className="mt-0.5 text-xs leading-5">{item.title}</div>
                      {item.description && (
                        <div className="mt-0.5 break-all text-xs leading-5 text-muted-foreground">
                          {item.description}
                        </div>
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </AccordionSettingsRow>
        </Accordion>
      </CardContent>
    </SettingsGroup>
  );
}

/** 错误日志：前端崩溃与未捕获错误的落盘位置（排障用；与通知历史同为事后核对入口）。 */
function ErrorLogCard() {
  const [path, setPath] = useState<string | null>(null);
  const [pathError, setPathError] = useState(false);
  const [revealing, setRevealing] = useState(false);
  // 浏览器演示页没有 Tauri，`isWebui()` 也为真；截图仍要看到路径和按钮。
  const showReveal = canPersistErrorLog() || api.isDemoMode();

  useEffect(() => {
    let cancelled = false;
    void api
      .getErrorLogPath()
      .then((value) => {
        if (!cancelled) setPath(value);
      })
      .catch(() => {
        if (!cancelled) {
          setPath("");
          setPathError(true);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function revealLog() {
    setRevealing(true);
    try {
      await api.revealErrorLog();
    } catch (e) {
      toast.error("打开日志位置失败", { description: api.asError(e) });
    } finally {
      setRevealing(false);
    }
  }

  const pathText =
    path === null
      ? "正在读取…"
      : path
        ? path
        : pathError
          ? "未能读取错误日志路径"
          : "浏览器模式下不会写入本机错误日志";

  return (
    <SettingsGroup id="settings-error-log" title="错误日志">
      <CardContent className="space-y-0 p-0">
        <div className="border-b border-border/50 px-4 py-3 text-xs leading-5 text-muted-foreground sm:px-5">
          {showReveal
            ? "界面崩溃与未捕获的错误会记录在这里（最多保留最近 200 条），反馈问题时可直接附上。"
            : "浏览器模式下，界面错误只会在页面上提示，不会写入本机错误日志。"}
        </div>
        <div
          className={cn(
            "break-all bg-foreground/[0.04] px-4 py-3 font-mono text-[11px] leading-5 text-muted-foreground sm:px-5",
            showReveal && "border-b border-border/50",
          )}
        >
          {pathText}
        </div>
        {showReveal && (
          <div className="flex flex-wrap gap-2 px-4 py-3 sm:px-5">
            <DemoAction>
              <Button
                size="sm"
                variant="outline"
                disabled={revealing || !path}
                onClick={() => void revealLog()}
              >
                {revealing ? <Loader2 className="animate-spin" /> : null}打开日志位置
              </Button>
            </DemoAction>
          </div>
        )}
      </CardContent>
    </SettingsGroup>
  );
}

function AppearanceCard() {
  const [theme, setTheme] = useState<ThemePreference>(getThemePreference);

  function onThemeChange(value: string) {
    if (value !== "system" && value !== "light" && value !== "dark") return;
    setThemePreference(value);
    setTheme(value);
  }

  return (
    <SettingsGroup
      id="settings-appearance"
      title="外观"
    >
      <CardContent className="space-y-0 p-0">
        <SettingsFieldRow
          className="border-b-0"
          label="主题"
          description="选择浅色、深色，或跟随系统外观自动切换"
          htmlFor="appearance-theme"
        >
          <Select value={theme} onValueChange={onThemeChange}>
            <SelectTrigger id="appearance-theme" size="sm" className="w-full sm:w-40" aria-label="主题">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="system">系统</SelectItem>
              <SelectItem value="light">浅色</SelectItem>
              <SelectItem value="dark">深色</SelectItem>
            </SelectContent>
          </Select>
        </SettingsFieldRow>
      </CardContent>
    </SettingsGroup>
  );
}

/** 限额监听：总开关 + hook 接入状态（CLI / WorkBuddy 实时上报，IDE 仍走日志扫描）。 */
function RateLimitCard() {
  const [config, setConfig] = useState<RateLimitConfig | null>(null);
  const [status, setStatus] = useState<RateLimitHookStatus | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void Promise.all([api.getRateLimitConfig(), api.getRateLimitHookStatus()])
      .then(([cfg, hook]) => {
        if (cancelled) return;
        setConfig(cfg);
        setStatus(hook);
      })
      .catch((e) => {
        if (!cancelled) toast.error("限额监听配置加载失败", { description: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onToggle(enabled: boolean) {
    if (!config || busy) return;
    const previous = config;
    setConfig({ ...config, enabled });
    setBusy(true);
    try {
      // 整个配置一起提交：只带 enabled 会把「卸载过」标记冲掉，重启后 hook 又被自动装回。
      setConfig(await api.saveRateLimitConfig({ ...config, enabled }));
      toast.success(enabled ? "限额监听已开启" : "限额监听已关闭");
    } catch (e) {
      setConfig(previous);
      toast.error("限额监听设置保存失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 「扫描 CodeBuddy IDE 日志」独立开关：只关两个 IDE 的日志来源（IDE 的 429 不触发任何
   * 事件，日志是它唯一的数据源），CLI / WorkBuddy 的 hook 通路不受影响。
   */
  async function onToggleIdeLogs(scanIdeLogs: boolean) {
    if (!config || busy) return;
    const previous = config;
    setConfig({ ...config, scanIdeLogs });
    setBusy(true);
    try {
      // 与总开关一样整份提交：只带 scanIdeLogs 会把 enabled / hookOptOut 冲成默认值。
      setConfig(await api.saveRateLimitConfig({ ...config, scanIdeLogs }));
      toast.success(
        scanIdeLogs
          ? "已开启 IDE 日志扫描"
          : "已关闭 IDE 日志扫描：两个 CodeBuddy IDE 的限额不再显示",
      );
    } catch (e) {
      setConfig(previous);
      toast.error("限额监听设置保存失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 回读限额配置：装 / 卸 hook 无论成败都会写「接入 / 卸载」意图（部分目标失败也算），
   * 本地标记不能只靠乐观更新，否则随后拨总开关会把过期值写回磁盘。
   */
  function refreshHookConfig() {
    return api
      .getRateLimitConfig()
      .then(setConfig)
      .catch(() => {
        /* 读不到就保持本地值，下次进设置页会重新拉 */
      });
  }

  async function onInstall() {
    if (busy) return;
    setBusy(true);
    try {
      setStatus(await api.installRateLimitHook());
      toast.success("已接入限额监听", {
        description: "CodeBuddy CLI / WorkBuddy 的 429 会实时上报（原配置已备份，可随时卸载还原）",
      });
    } catch (e) {
      toast.error("接入 hook 失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
      void refreshHookConfig();
    }
  }

  async function onUninstall() {
    if (busy) return;
    setBusy(true);
    try {
      setStatus(await api.uninstallRateLimitHook());
      toast.success("已卸载 hook", {
        description: "客户端配置恢复原状，之后不会再自动接入（限额改由日志扫描发现，可随时点「接入 hook」恢复）",
      });
    } catch (e) {
      toast.error("卸载 hook 失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
      void refreshHookConfig();
    }
  }

  const existingTargets = status?.targets.filter((target) => target.exists) ?? [];
  const installedCount = existingTargets.filter((target) => target.installed).length;
  // IDE 的限额只有日志一条来源：扫描开关关闭时文案不能再说「仍按日志扫描」。
  const ideNote =
    config?.scanIdeLogs === false
      ? "CodeBuddy IDE 的日志扫描已关闭"
      : "CodeBuddy IDE 无事件，仍按日志扫描";
  const hookDescription = status
    ? existingTargets.length === 0
      ? `未检测到 CodeBuddy CLI / WorkBuddy 客户端：没有可接入的配置（${ideNote}）`
      : status.installed
        ? `${installedCount} / ${existingTargets.length} 个已安装客户端已接入：429 当轮实时上报（秒级）；${ideNote}`
        : config?.hookOptOut
          ? "已卸载：不会再自动接入，限额改由日志扫描发现；点「接入 hook」可恢复实时上报"
          : "未接入：限额仅靠定期扫描日志发现（最多滞后数分钟）"
    : "加载中…";

  return (
    <SettingsGroup id="settings-rate-limit" title="限额监听">
      <CardContent className="space-y-0 p-0">
        <SettingsFieldRow
          label="启用限额监听"
          description="关闭后不扫描日志、账号卡片不显示限额标记；重新开启后恢复"
          htmlFor="rl-enabled"
          operational
        >
          <Switch
            id="rl-enabled"
            checked={config?.enabled ?? true}
            disabled={busy || !config}
            onCheckedChange={(v) => void onToggle(v)}
            aria-label="启用限额监听"
          />
        </SettingsFieldRow>

        <SettingsFieldRow
          label="扫描 CodeBuddy IDE 日志"
          description="IDE 的限额只有日志一条来源，关掉后不再显示；CodeBuddy CLI / WorkBuddy 的实时上报不受影响"
          htmlFor="rl-ide-scan"
          operational
        >
          <Switch
            id="rl-ide-scan"
            checked={config?.scanIdeLogs ?? true}
            disabled={busy || !config}
            onCheckedChange={(v) => void onToggleIdeLogs(v)}
            aria-label="扫描 CodeBuddy IDE 日志"
          />
        </SettingsFieldRow>

        <SettingsFieldRow
          className="border-b-0"
          label="接入客户端 hook"
          description={hookDescription}
          operational
        >
          {status?.installed ? (
            <Button
              id="rl-hook"
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={() => void onUninstall()}
            >
              {busy ? <Loader2 className="animate-spin" /> : null}卸载 hook
            </Button>
          ) : (
            <Button
              id="rl-hook"
              size="sm"
              disabled={busy || !status || existingTargets.length === 0}
              onClick={() => void onInstall()}
            >
              {busy ? <Loader2 className="animate-spin" /> : null}接入 hook
            </Button>
          )}
        </SettingsFieldRow>
      </CardContent>
    </SettingsGroup>
  );
}

/** 设置页：演示模式不渲染自动签到；Agent Companion 只在桌面正式版显示。 */
export default function SettingsPage() {
  return (
    <div className="mx-auto min-w-0 w-full max-w-3xl px-4 py-6 sm:px-6 sm:py-8">
      <header className="mb-10 sm:mb-12">
        <h1 className="text-2xl font-semibold tracking-tight">设置</h1>
        <p className="mt-2 text-sm leading-6 text-muted-foreground">
          {api.isDemoMode() ? "限额监听、权限检测与自动更新配置。" : "自动签到、限额监听、权限检测与自动更新配置。"}
        </p>
      </header>

      <div className="min-w-0 space-y-12">
        <AppearanceCard />
        <PermissionCheckCard />
        {api.isDemoMode() ? null : <AutoCheckinCard />}
        <AutoRotateCard />
        <RateLimitCard />
        {api.isDesktop() && !api.isDemoMode() ? <CompanionCard /> : null}
        {api.isDesktop() || api.isDemoMode() ? <StartupCard /> : null}
        <NotificationHistoryCard />
        <ErrorLogCard />
        {api.isWebui() && !api.isDemoMode() ? null : <UpdateCard />}
      </div>
    </div>
  );
}
