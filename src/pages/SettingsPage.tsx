import { useEffect, useState, type ReactElement, type ReactNode } from "react";
import { ArrowUpCircle, CircleCheck, ExternalLink, Loader2, RefreshCw, Save } from "lucide-react";
import { toast } from "sonner";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { TimePicker } from "@/components/ui/time-picker";
import * as api from "@/lib/api";
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

interface CollapsibleSettingsRowProps {
  label: ReactNode;
  description?: ReactNode;
  open: boolean;
  onToggle: () => void;
  /** 展开/收起之外的附加操作（如通知历史的「清空」）。 */
  actions?: ReactNode;
  /** 位于卡片中段的行保留收起态分隔线；末行（默认）不需要。 */
  divider?: boolean;
  children: ReactNode;
}

/**
 * 折叠行：label/description + 查看/收起（+ 可选附加操作），展开内容在下方。
 *
 * 沿用通知历史既有的「Button + 条件渲染」而非 Collapsible 原语：折叠行内可能含
 * Switch 等交互控件，用 Trigger 包整行会产生嵌套交互元素。展开按钮是纯展示交互，
 * 不套 DemoAction（演示模式下仍可展开查看）。
 */
function CollapsibleSettingsRow({
  label,
  description,
  open,
  onToggle,
  actions,
  divider = false,
  children,
}: CollapsibleSettingsRowProps) {
  return (
    <>
      <SettingsFieldRow
        className={open || divider ? undefined : "border-b-0"}
        label={label}
        description={description}
      >
        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" onClick={onToggle}>
            {open ? "收起" : "查看"}
          </Button>
          {actions}
        </div>
      </SettingsFieldRow>
      {open && <div className="border-t border-border/50 px-4 py-1.5 sm:px-5">{children}</div>}
    </>
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
 * 签到时间段的非法组合说明（只提示、不阻止保存：后端按“不限制”处理）。
 */
function checkinWindowIssue(start: string, end: string): string | null {
  if (!start && !end) return null;
  if (!start || !end) return "开始与结束时间需同时填写，否则按不限制处理";
  const startMinutes = clockMinutes(start);
  const endMinutes = clockMinutes(end);
  if (startMinutes === null || endMinutes === null) return "时间格式应为 HH:MM";
  if (startMinutes >= endMinutes) return "结束时间需晚于开始时间（不支持跨午夜），否则按不限制处理";
  return null;
}

/** 自动签到配置 + 一键签到 + 日志（含自动旅行行）。 */
function AutoCheckinCard() {
  /** 自动旅行与自动签到同卡，按档位决定是否渲染该行（国际版无成长中心）。 */
  const variant = useAccountsStore((s) => s.variant);
  const accounts = useAccountsStore((s) => s.accounts);
  const fetchAll = useAccountsStore((s) => s.fetchAll);
  const [cfg, setCfg] = useState<CheckinConfig | null>(null);
  /** 最近一次落盘的配置：逐账号开关只提交它 + 新名单，不带上未保存的参数草稿。 */
  const [savedCfg, setSavedCfg] = useState<CheckinConfig | null>(null);
  /** 参数区（时间段 / 保活阈值 / 惰性刷新）默认收起；开关行与操作行常驻。 */
  const [expanded, setExpanded] = useState(false);
  /** 逐账号开关列表默认收起。 */
  const [excludedOpen, setExcludedOpen] = useState(false);
  const [excludedSaving, setExcludedSaving] = useState(false);
  const [logsOpen, setLogsOpen] = useState(false);
  const [logs, setLogs] = useState<CheckinLog[] | null>(null);
  const [logsError, setLogsError] = useState("");
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

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
      setCfg(loaded);
      setSavedCfg(loaded);
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    }
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

  async function save() {
    if (!cfg || saving || excludedSaving) return;
    setSaving(true);
    setMsg(null);
    try {
      const saved = await api.saveAutoCheckinConfig(cfg);
      setCfg(saved);
      setSavedCfg(saved);
      setMsg({ type: "ok", text: "配置已保存" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setSaving(false);
    }
  }

  async function checkinAllNow() {
    if (busy || saving || excludedSaving) return;
    setBusy(true);
    setMsg(null);
    try {
      const res = await api.checkinAll();
      if (res.status === "skipped" && res.reason === "already_running") {
        setMsg({ type: "err", text: "签到任务正在进行，请稍后再试" });
        return;
      }
      const ok = res.accounts.filter((a) => a.result === "success").length;
      const already = res.accounts.filter((a) => a.result === "already").length;
      const err = res.accounts.filter((a) => a.result === "error").length;
      const detail = res.accounts
        .filter((a) => a.result === "error")
        .map((a) => `${a.email}（${a.error}）`)
        .join("；");
      setMsg({
        type: err > 0 ? "err" : "ok",
        text: `签到完成：成功 ${ok}，已签 ${already}，失败 ${err}${detail ? `。${detail}` : ""}`,
      });
      void loadConfig();
      if (logsOpen) void loadLogs();
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 逐账号开关：切换即落盘，提交「上次落盘的配置 + 新名单」。
   *
   * 不带上当前草稿的时间段/阈值，避免把未保存的参数一起写盘；成功后只回填名单，
   * 失败则回滚开关状态并提示。
   */
  async function onAutoCheckinChange(account: AccountMeta, allowed: boolean) {
    if (!savedCfg || saving || excludedSaving) return;
    const previous = savedCfg;
    const excluded = new Set(previous.excluded_account_ids ?? []);
    if (allowed) excluded.delete(account.id);
    else excluded.add(account.id);
    const next = [...excluded];
    setCfg((current) => (current ? { ...current, excluded_account_ids: next } : current));
    setMsg(null);
    setExcludedSaving(true);
    try {
      const saved = await api.saveAutoCheckinConfig({ ...previous, excluded_account_ids: next });
      setSavedCfg(saved);
      setCfg((current) =>
        current ? { ...current, excluded_account_ids: saved.excluded_account_ids ?? [] } : current,
      );
    } catch (e) {
      setCfg((current) =>
        current ? { ...current, excluded_account_ids: previous.excluded_account_ids ?? [] } : current,
      );
      toast.error("账号自动签到设置保存失败", { description: api.asError(e) });
    } finally {
      setExcludedSaving(false);
    }
  }

  function setNum(key: keyof CheckinConfig, value: string) {
    if (!cfg) return;
    setCfg({ ...cfg, [key]: Number(value) });
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
        {cfg ? (
          <>
            <SettingsFieldRow
              label="启用自动签到"
              description="为允许自动签到的账号核验状态并补签；可在下方按账号关闭"
              htmlFor="ac-enabled"
            >
              <div className="flex items-center gap-2">
                {/* 开关是业务操作（需保存配置生效），展开按钮是纯展示交互，故只包开关。 */}
                <DemoAction>
                  <Switch
                    id="ac-enabled"
                    checked={cfg.enabled}
                    onCheckedChange={(v) => setCfg({ ...cfg, enabled: v })}
                  />
                </DemoAction>
                <Button variant="outline" size="sm" onClick={() => setExpanded((value) => !value)}>
                  {expanded ? "收起" : "展开"}
                </Button>
              </div>
            </SettingsFieldRow>

            {/* 逐账号开关：切换即落盘，与同卡片「保存配置」的参数草稿互不影响。 */}
            <CollapsibleSettingsRow
              label="不参与自动签到的账号"
              description="关闭后，后台轮次与刷新积分时跳过该账号；仍可手动签到。"
              open={excludedOpen}
              onToggle={() => setExcludedOpen((value) => !value)}
              divider
            >
              {checkinAccounts.length === 0 ? (
                <p className="py-2 text-xs text-muted-foreground">暂无可签到的账号</p>
              ) : (
                <div className="py-0.5">
                  {checkinAccounts.map((account) => {
                    const name = account.nickname || account.email || account.uid || account.id;
                    return (
                      <div
                        key={account.id}
                        className="flex min-w-0 items-center justify-between gap-3 border-b border-border/60 py-2 last:border-b-0"
                      >
                        <span className="min-w-0 flex-1 truncate text-[13px] leading-4" title={name}>
                          {name}
                        </span>
                        <DemoAction>
                          <Switch
                            checked={!excludedIds.has(account.id)}
                            disabled={saving || excludedSaving}
                            onCheckedChange={(allowed) => void onAutoCheckinChange(account, allowed)}
                            aria-label={`${name}参与自动签到`}
                          />
                        </DemoAction>
                      </div>
                    );
                  })}
                </div>
              )}
            </CollapsibleSettingsRow>

            {expanded && (
              <>
                <SettingsFieldRow
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
                          onChange={(v) => setCfg({ ...cfg, checkin_start: v })}
                        />
                      </DemoAction>
                      <span className="shrink-0 text-xs text-muted-foreground">至</span>
                      <DemoAction className="min-w-0 flex-1 sm:flex-none">
                        <TimePicker
                          className="min-w-0 flex-1 sm:flex-none"
                          value={cfg.checkin_end}
                          hourLabel="签到结束时间（小时）"
                          minuteLabel="签到结束时间（分钟）"
                          onChange={(v) => setCfg({ ...cfg, checkin_end: v })}
                        />
                      </DemoAction>
                      {(cfg.checkin_start || cfg.checkin_end) && (
                        <DemoAction>
                          <Button
                            size="sm"
                            variant="ghost"
                            className="shrink-0"
                            onClick={() => setCfg({ ...cfg, checkin_start: "", checkin_end: "" })}
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

                <SettingsFieldRow
                  label="保活阈值"
                  description="天；0 表示每天无条件刷新"
                  htmlFor="ac-keep"
                  operational
                >
                  <Input
                    id="ac-keep"
                    className="w-full sm:w-48"
                    type="number"
                    min={0}
                    max={90}
                    value={cfg.keepalive_days}
                    onChange={(e) => setNum("keepalive_days", e.target.value)}
                  />
                </SettingsFieldRow>
                <SettingsFieldRow label="惰性刷新" description="小时" htmlFor="ac-lazy" operational>
                  <Input
                    id="ac-lazy"
                    className="w-full sm:w-48"
                    type="number"
                    min={1}
                    max={72}
                    value={cfg.lazy_refresh_hours}
                    onChange={(e) => setNum("lazy_refresh_hours", e.target.value)}
                  />
                </SettingsFieldRow>
              </>
            )}

            <div className="flex flex-wrap gap-2 border-b-0 border-border/60 px-4 py-3 sm:px-5">
              <DemoAction><Button size="sm" onClick={save} disabled={saving || excludedSaving}>
                {saving ? <Loader2 className="animate-spin" /> : <Save />}保存配置
              </Button></DemoAction>
              <DemoAction><Button size="sm" variant="outline" onClick={checkinAllNow} disabled={busy || saving || excludedSaving}>
                {busy ? <Loader2 className="animate-spin" /> : <CircleCheck />}全部立即签到
              </Button></DemoAction>
            </div>
          </>
        ) : (
          <p className="px-4 py-3 text-sm text-muted-foreground sm:px-5">加载配置中…</p>
        )}

        {/* 成长中心（派猫猫旅行）仅国内版开放：国际版不渲染该行，也不请求其配置。
            行本身独立于签到配置的加载状态，签到配置读取失败也不影响开关。 */}
        {variantSupportsTravel(variant) ? <AutoTravelRow /> : null}

        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}

        <CollapsibleSettingsRow
          label="签到日志"
          description="保留最近 30 天；本机明文保存，可能含账号昵称。"
          open={logsOpen}
          onToggle={() => setLogsOpen((value) => !value)}
        >
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
                    className="flex items-center justify-between border-b border-border/60 py-2 text-xs last:border-b-0"
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
        </CollapsibleSettingsRow>
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
  /** 参数区（间隔 / 冷却 / 阈值）与说明默认收起；开关行、操作行与日志行常驻。 */
  const [expanded, setExpanded] = useState(false);
  const [logsOpen, setLogsOpen] = useState(false);
  const [logs, setLogs] = useState<RotateLog[] | null>(null);
  const [logsError, setLogsError] = useState("");
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

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
      setCfg(c);
      setStatus(s);
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
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

  async function save() {
    if (!cfg) return;
    setSaving(true);
    setMsg(null);
    try {
      const saved = await api.saveAutoRotateConfig(cfg);
      setCfg(saved);
      setMsg({ type: "ok", text: "配置已保存" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setSaving(false);
    }
  }

  async function runNow() {
    setBusy(true);
    setMsg(null);
    try {
      const res = await api.runRotate();
      // webui 没有事件通道：手动检查的推迟提示只能从返回值里取（桌面端由
      // `rotate-deferred` 事件统一弹出，避免同一件事弹两次）。
      if (api.isWebui() && res.notify?.body) {
        toast.warning("自动轮换已推迟", { description: res.notify.body, duration: 10_000 });
      }
      setMsg({
        type: res.status === "error" ? "err" : "ok",
        text:
          res.status === "switched"
            ? `已切换到 ${res.to ?? "目标账号"}`
            : res.status === "disabled"
              ? "自动轮换未启用（请在下方开启后重试）"
              : (res.reason ?? `检查完成：${res.status}`),
      });
      void loadConfig();
      if (logsOpen) void loadLogs();
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  function setNum(key: keyof AutoRotateConfig, value: string) {
    if (!cfg) return;
    setCfg({ ...cfg, [key]: Number(value) });
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

        {cfg ? (
          <>
            <SettingsFieldRow
              label="启用自动轮换"
              description="开启后按设定的间隔自动检查并切换 CodeBuddy CLI 账号"
              htmlFor="ar-enabled"
            >
              <div className="flex items-center gap-2">
                {/* 开关是业务操作（需保存配置生效），展开按钮是纯展示交互，故只包开关。 */}
                <DemoAction>
                  <Switch
                    id="ar-enabled"
                    checked={cfg.enabled}
                    onCheckedChange={(v) => setCfg({ ...cfg, enabled: v })}
                  />
                </DemoAction>
                <Button variant="outline" size="sm" onClick={() => setExpanded((value) => !value)}>
                  {expanded ? "收起" : "展开"}
                </Button>
              </div>
            </SettingsFieldRow>

            {expanded && (
              <>
                <SettingsFieldRow label="检查间隔" description="分钟" htmlFor="ar-interval" operational>
                  <Input
                    id="ar-interval"
                    className="w-full sm:w-48"
                    type="number"
                    min={1}
                    max={1440}
                    value={cfg.check_interval_minutes}
                    onChange={(e) => setNum("check_interval_minutes", e.target.value)}
                  />
                </SettingsFieldRow>
                <SettingsFieldRow label="切换冷却" description="分钟" htmlFor="ar-cooldown" operational>
                  <Input
                    id="ar-cooldown"
                    className="w-full sm:w-48"
                    type="number"
                    min={1}
                    max={1440}
                    value={cfg.cooldown_minutes}
                    onChange={(e) => setNum("cooldown_minutes", e.target.value)}
                  />
                </SettingsFieldRow>
                <SettingsFieldRow label="到期差异阈值" description="小时" htmlFor="ar-gap" operational>
                  <Input
                    id="ar-gap"
                    className="w-full sm:w-48"
                    type="number"
                    min={0}
                    max={720}
                    value={cfg.min_gap_hours}
                    onChange={(e) => setNum("min_gap_hours", e.target.value)}
                  />
                </SettingsFieldRow>
                <SettingsFieldRow label="到期紧迫阈值" description="小时" htmlFor="ar-urgency" operational>
                  <Input
                    id="ar-urgency"
                    className="w-full sm:w-48"
                    type="number"
                    min={0}
                    max={720}
                    value={cfg.min_urgency_hours}
                    onChange={(e) => setNum("min_urgency_hours", e.target.value)}
                  />
                </SettingsFieldRow>
                <SettingsFieldRow label="最小剩余积分" description="低于此值时不切换" htmlFor="ar-min" operational>
                  <Input
                    id="ar-min"
                    className="w-full sm:w-48"
                    type="number"
                    min={0}
                    value={cfg.min_remaining_credits}
                    onChange={(e) => setNum("min_remaining_credits", e.target.value)}
                  />
                </SettingsFieldRow>
                <p className="border-b border-border/60 px-4 py-3 text-[13px] leading-5 text-muted-foreground sm:px-5">
                  切换时机：目标账号剩余到期时间少于「紧迫阈值」且比当前账号早超过「差异阈值」，且目标剩余积分不低于「最小剩余积分」。检测到有 CodeBuddy CLI 会话在运行时，本次轮换会跳过并在当日最多提示 5 次；重启 CLI 后新账号才会生效。
                </p>
              </>
            )}

            <div className="flex flex-wrap gap-2 border-b-0 border-border/60 px-4 py-3 sm:px-5">
              <DemoAction><Button size="sm" onClick={save} disabled={saving}>
                {saving ? <Loader2 className="animate-spin" /> : <Save />}保存配置
              </Button></DemoAction>
              <DemoAction><Button size="sm" variant="outline" onClick={runNow} disabled={busy}>
                {busy ? <Loader2 className="animate-spin" /> : <RefreshCw />}立即检查一次
              </Button></DemoAction>
            </div>
          </>
        ) : (
          <p className="px-4 py-3 text-sm text-muted-foreground sm:px-5">加载配置中…</p>
        )}

        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}

        <CollapsibleSettingsRow
          label="轮换日志"
          description="保留最近 200 条；本机明文保存，可能含账号昵称。"
          open={logsOpen}
          onToggle={() => setLogsOpen((value) => !value)}
        >
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
                    className="flex items-center justify-between border-b border-border/60 py-2 text-xs last:border-b-0"
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
        </CollapsibleSettingsRow>
      </CardContent>
    </SettingsGroup>
  );
}

/** 权限检测卡片：确认本 App 是否有权写入 WorkBuddy 认证文件（探针与展示路径同档位）。 */
function PermissionCheckCard() {
  const authFile = useAuthFile();
  const variant = useAccountsStore((s) => s.variant);
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<null | { ok: boolean; text: string }>(null);

  async function runCheck() {
    setChecking(true);
    setResult(null);
    try {
      const res = await api.checkAuthPermission(variant);
      setResult({
        ok: res.ok,
        text: res.ok
          ? res.message ?? "认证目录可写，权限正常"
          : `${res.error}（${res.dir ?? ""}）`,
      });
    } catch (e) {
      setResult({ ok: false, text: api.asError(e) });
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

        {result && (
          <Alert variant={result.ok ? "default" : "destructive"} className="!w-auto mx-4 my-4 sm:mx-5">
            <AlertDescription>{result.text}</AlertDescription>
          </Alert>
        )}
        {result && !result.ok && (
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
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [checking, setChecking] = useState(false);
  const [installOpen, setInstallOpen] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);
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
        if (!cancelled) setMsg({ type: "err", text: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function check() {
    setChecking(true);
    setMsg(null);
    try {
      const r = await api.checkUpdate(proxyUrl, true);
      setInfo(r);
      if (!r.ok) {
        setMsg({ type: "err", text: r.message || r.error || "检查失败" });
      }
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setChecking(false);
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
        setMsg({ type: "err", text: "代理地址格式不正确，请填写 HTTP/HTTPS 地址，例如 http://127.0.0.1:7897" });
        return;
      }
    }

    setProxySaving(true);
    setMsg(null);
    try {
      const saved = await api.saveGithubConfig({ ...githubConfig, proxy: value });
      setGithubConfig(saved);
      setProxyUrl(saved.proxy ?? "");
      setMsg({ type: "ok", text: value ? "更新代理已保存" : "已关闭更新代理" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
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
                <DemoAction><Button size="sm" onClick={() => setInstallOpen(true)}>
                  <ArrowUpCircle />
                  立即升级
                </Button></DemoAction>
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
        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}
        <UpdateInstallDialog
          open={installOpen}
          onOpenChange={setInstallOpen}
          update={info}
        />
      </CardContent>
    </SettingsGroup>
  );
}

/** 开机自启（仅桌面端渲染）：开关直接反映系统自启注册状态，切换立即生效。 */
function StartupCard() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    let cancelled = false;
    void api
      .getLaunchAtLoginEnabled()
      .then((value) => {
        if (!cancelled) setEnabled(value);
      })
      .catch((e) => {
        if (!cancelled) setMsg({ type: "err", text: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onToggle(value: boolean) {
    if (busy || enabled === null) return;
    const previous = enabled;
    setBusy(true);
    setMsg(null);
    try {
      // 后端回读 OS 权威状态；即使与请求一致，也以回读值显示。
      const authoritative = await api.setLaunchAtLoginEnabled(value);
      setEnabled(authoritative);
      setMsg({ type: "ok", text: authoritative ? "已开启开机自启" : "已关闭开机自启" });
    } catch (e) {
      // 失败时恢复到最后一次确认的状态，并显示可读错误。
      setEnabled(previous);
      setMsg({ type: "err", text: api.asError(e) });
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

        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}
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
  const [open, setOpen] = useState(false);
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
        <CollapsibleSettingsRow
          label="应用内提示存档"
          description="保留最近 100 条，便于事后核对；本机明文保存，可能含账号昵称与本地路径。"
          open={open}
          onToggle={() => setOpen((value) => !value)}
          actions={
            <Button
              variant="ghost"
              size="sm"
              disabled={!items || items.length === 0}
              onClick={clearHistory}
            >
              清空
            </Button>
          }
        >
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
                  <div className="mt-0.5 text-[13px] leading-5">{item.title}</div>
                  {item.description && (
                    <div className="mt-0.5 break-all text-xs leading-5 text-muted-foreground">
                      {item.description}
                    </div>
                  )}
                </li>
              ))}
            </ul>
          )}
        </CollapsibleSettingsRow>
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
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    let cancelled = false;
    void Promise.all([api.getRateLimitConfig(), api.getRateLimitHookStatus()])
      .then(([cfg, hook]) => {
        if (cancelled) return;
        setConfig(cfg);
        setStatus(hook);
      })
      .catch((e) => {
        if (!cancelled) setMsg({ type: "err", text: api.asError(e) });
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
    setMsg(null);
    try {
      // 整个配置一起提交：只带 enabled 会把「卸载过」标记冲掉，重启后 hook 又被自动装回。
      setConfig(await api.saveRateLimitConfig({ ...config, enabled }));
      setMsg({ type: "ok", text: enabled ? "限额监听已开启" : "限额监听已关闭" });
    } catch (e) {
      setConfig(previous);
      setMsg({ type: "err", text: api.asError(e) });
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
    setMsg(null);
    try {
      // 与总开关一样整份提交：只带 scanIdeLogs 会把 enabled / hookOptOut 冲成默认值。
      setConfig(await api.saveRateLimitConfig({ ...config, scanIdeLogs }));
      setMsg({
        type: "ok",
        text: scanIdeLogs
          ? "已开启 IDE 日志扫描"
          : "已关闭 IDE 日志扫描：两个 CodeBuddy IDE 的限额不再显示",
      });
    } catch (e) {
      setConfig(previous);
      setMsg({ type: "err", text: api.asError(e) });
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
    setMsg(null);
    try {
      setStatus(await api.installRateLimitHook());
      setMsg({
        type: "ok",
        text: "已接入限额监听：CodeBuddy CLI / WorkBuddy 的 429 会实时上报（原配置已备份，可随时卸载还原）",
      });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setBusy(false);
      void refreshHookConfig();
    }
  }

  async function onUninstall() {
    if (busy) return;
    setBusy(true);
    setMsg(null);
    try {
      setStatus(await api.uninstallRateLimitHook());
      setMsg({
        type: "ok",
        text: "已卸载 hook：客户端配置恢复原状，之后不会再自动接入（限额改由日志扫描发现，可随时点「接入 hook」恢复）",
      });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
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
          htmlFor="rl-hook"
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

        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}
      </CardContent>
    </SettingsGroup>
  );
}

/** 设置页：外观 / 权限检测 / 自动签到（含自动旅行）/ 自动轮换 / 限额监听 / 更新配置。 */
export default function SettingsPage() {
  return (
    <div className="mx-auto min-w-0 w-full max-w-3xl px-4 py-6 sm:px-6 sm:py-8">
      <header className="mb-10 sm:mb-12">
        <h1 className="text-2xl font-semibold tracking-tight">设置</h1>
        <p className="mt-2 text-sm leading-6 text-muted-foreground">自动签到、限额监听、权限检测与自动更新配置。</p>
      </header>

      <div className="min-w-0 space-y-12">
        <AppearanceCard />
        <PermissionCheckCard />
        <AutoCheckinCard />
        <AutoRotateCard />
        <RateLimitCard />
        {api.isDesktop() || api.isDemoMode() ? <StartupCard /> : null}
        <NotificationHistoryCard />
        {api.isWebui() && !api.isDemoMode() ? null : <UpdateCard />}
      </div>
    </div>
  );
}
