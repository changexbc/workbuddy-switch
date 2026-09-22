import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  Columns3,
  Download,
  ExternalLink,
  FileDown,
  FileUp,
  Loader2,
  QrCode,
  RefreshCw,
  Rows3,
  Terminal,
} from "lucide-react";

import { AccountCard } from "@/components/account-card";
import { DemoAction } from "@/components/demo-action";
import { DiscoverAccountsBanner } from "@/components/discover-accounts-banner";
import {
  CodeBuddyAiIdeMark,
  CodeBuddyCnIdeMark,
  CodeBuddyMark,
  VscodeExtMark,
  WorkBuddyAiMark,
  WorkBuddyMark,
} from "@/components/product-marks";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ExportAccountsDialog } from "@/components/export-accounts-dialog";
import { ImportAccountsDialog } from "@/components/import-accounts-dialog";
import { OAuthLoginDialog } from "@/components/oauth-login-dialog";
import { SwitchAccountDialog } from "@/components/switch-account-dialog";
import { VscodeSwitchAccountDialog } from "@/components/vscode-switch-account-dialog";
import * as api from "@/lib/api";
import { useVisibleInterval } from "@/lib/use-visible-interval";
import {
  DEFAULT_VARIANT,
  accountVariant,
  normalizeVariant,
  variantAppName,
  variantCodebuddyIdeName,
  variantDownloadDomain,
  variantLabel,
  variantSupportsCheckin,
  variantSupportsTravel,
  variantUsesIntlCodebuddyIde,
} from "@/lib/variant";
import type { AccountMeta, AppStatus, CodeBuddyCliStatus, CodeBuddyCnIdeStatus, CreditExpiry, RateLimitEntry, TravelConfig, TravelStatus, VscodeExtStatus } from "@/lib/types";
import { cn } from "@/lib/utils";
import { useAccountsStore } from "@/stores/accounts";

/**
 * 账号页两个轮询的间隔（都经 `useVisibleInterval` 门控，仅主窗口可见时执行）。
 *
 * - 旅行：后台派发/领取循环最快 15 分钟变一次状态，1 分钟用于及时反映"到期领取"后的显示；
 * - 限额：CLI / WorkBuddy 由后端 hook 信号实时入账并推送（`rate-limits-updated`），
 *   这里只兜底 IDE 日志扫描；后端按同一间隔节流扫描，前端再按 payload 的 `scannedAt`
 *   判断「距上次扫描 ≥ 5 分钟」才发起，避免可见性切换/页面重挂载把扫描打散。
 */
const TRAVEL_REFRESH_INTERVAL_MS = 60 * 1000;
const RATE_LIMIT_REFRESH_INTERVAL_MS = 5 * 60 * 1000;

function expiringSoonAmount(credit?: CreditExpiry): number {
  return credit?.ok ? credit.expiringSoonRemaining ?? 0 : 0;
}

function hasExpiringSoonCredits(credit?: CreditExpiry): boolean {
  return credit?.ok === true && expiringSoonAmount(credit) > 0;
}

function soonestRelevantExpiry(credit?: CreditExpiry): number {
  const soonestExpiringCredit = (credit?.resources ?? [])
    .filter((resource) => resource.remaining > 0 && resource.expiringSoon && resource.expireAt != null)
    .map((resource) => resource.expireAt as number)
    .reduce((soonest, expireAt) => Math.min(soonest, expireAt), Number.POSITIVE_INFINITY);
  return Number.isFinite(soonestExpiringCredit)
    ? soonestExpiringCredit
    : credit?.soonestExpireAt ?? Number.POSITIVE_INFINITY;
}

function creditPriorityRank(credit?: CreditExpiry): number {
  if (!credit?.ok) return 3;
  if (hasExpiringSoonCredits(credit)) return 0;
  if (credit.expired) return 1;
  return 2;
}

function isWorkbuddyCurrent(account: AccountMeta, current: AppStatus["current"] | undefined): boolean {
  if (!current) return false;
  return Boolean(
    (current.uid && (account.uid === current.uid || account.id === current.uid)) ||
      (current.email && account.email === current.email),
  );
}

/** 并行查询今日签到；失败的账号不写入，由调用方保留原值。 */
async function fetchTodayCheckinMap(
  accountIds: string[],
  isStale?: () => boolean,
): Promise<Record<string, boolean>> {
  const entries = await Promise.all(
    accountIds.map(async (id) => {
      try {
        const res = await api.getCheckinStatus(id);
        if (isStale?.() || !res.ok) return null;
        return [id, res.todayCheckedIn] as const;
      } catch {
        return null;
      }
    }),
  );
  const next: Record<string, boolean> = {};
  for (const entry of entries) {
    if (entry) next[entry[0]] = entry[1];
  }
  return next;
}

/** 并行查询各账号今日旅行状态；失败的账号不写入，由调用方保留原值。 */
async function fetchTravelMap(
  accountIds: string[],
  isStale?: () => boolean,
): Promise<Record<string, TravelStatus>> {
  const entries = await Promise.all(
    accountIds.map(async (id) => {
      try {
        const res = await api.getTravelStatus(id);
        if (isStale?.()) return null;
        return [id, res] as const;
      } catch {
        return null;
      }
    }),
  );
  const next: Record<string, TravelStatus> = {};
  for (const entry of entries) {
    if (entry) next[entry[0]] = entry[1];
  }
  return next;
}

export default function AccountsPage() {
  const {
    accounts,
    variant,
    setVariant,
    status,
    loading,
    error,
    fetchAll,
    deleteAccount,
    importLocal,
    creditMap,
    creditLoadingMap,
    creditUpdatedAtMap,
    refreshingCredits,
    ensureCredits,
    refreshCredits,
  } = useAccountsStore();
  const [oauthOpen, setOauthOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [switchAccount, setSwitchAccount] = useState<AccountMeta | null>(null);
  const [importing, setImporting] = useState(false);
  /** 账号 id -> 今日是否已签到（undefined=查询中/未知） */
  const [checkinMap, setCheckinMap] = useState<Record<string, boolean>>({});
  /** 账号 id -> 今日旅行状态（undefined=查询中/未知） */
  const [travelMap, setTravelMap] = useState<Record<string, TravelStatus>>({});
  /**
   * 自动旅行配置（只读）：未开启时不渲染账号卡片的旅行 chip、也不轮询旅行状态。
   * `null` = 配置尚未读到，按未开启处理。
   */
  const [autoTravelConfig, setAutoTravelConfig] = useState<TravelConfig | null>(null);
  /** 账号 id -> 当前受限的模型（数据源 = 后端限额台账：hook 信号 + 日志扫描） */
  const [rateLimitMap, setRateLimitMap] = useState<Record<string, RateLimitEntry[]>>({});
  /**
   * 「限额监听」开关（设置页）：关闭后不扫描、不渲染限额 chip。
   * `null` = 配置尚未读到，不得按默认 true 先扫一轮（关闭开关后进账号页会闪 chip / 误请求）。
   */
  const [rateLimitEnabled, setRateLimitEnabled] = useState<boolean | null>(null);
  const [codebuddyCli, setCodebuddyCli] = useState<CodeBuddyCliStatus | null>(null);
  const [codebuddyCliSwitchingId, setCodebuddyCliSwitchingId] = useState<string | null>(null);
  const [codebuddyCnIde, setCodebuddyCnIde] = useState<CodeBuddyCnIdeStatus | null>(null);
  const [codebuddyCnIdeSwitchingId, setCodebuddyCnIdeSwitchingId] = useState<string | null>(null);
  const [vscodeExt, setVscodeExt] = useState<VscodeExtStatus | null>(null);
  /** VS Code 扩展切换弹窗目标（null=关闭）；切换与可选会话复制在弹窗内完成。 */
  const [vscodeSwitchAccount, setVscodeSwitchAccount] = useState<AccountMeta | null>(null);
  const [installingCodebuddyCli, setInstallingCodebuddyCli] = useState(false);
  /** 刷新按钮触发的批量签到进行中 */
  const [checkinAllRunning, setCheckinAllRunning] = useState(false);
  /** 接入/升级 CLI helper 确认框 */
  const [installConfirmOpen, setInstallConfirmOpen] = useState(false);
  /** 切换 CodeBuddy CLI 确认目标（null=关闭） */
  const [cliSwitchTarget, setCliSwitchTarget] = useState<AccountMeta | null>(null);
  /** 删除账号确认目标（null=关闭） */
  const [deleteTarget, setDeleteTarget] = useState<AccountMeta | null>(null);
  /** 当前档位下的账号：列表、计数、签到、积分等一律只作用于当前档位。 */
  const visibleAccounts = useMemo(
    () => accounts.filter((account) => accountVariant(account) === variant),
    [accounts, variant],
  );
  const appName = variantAppName(variant);
  const travelAvailable = variantSupportsTravel(variant);
  const checkinAvailable = variantSupportsCheckin(variant);
  /** 旅行 chip 与旅行状态轮询只在自动旅行开启后生效（配置未读到 = 未开启）。 */
  const autoTravelEnabled = travelAvailable && autoTravelConfig?.enabled === true;
  /** 刷新按钮文案：国际版没有签到接口，只刷新积分。 */
  const refreshCreditsLabel = checkinAvailable ? "签到并刷新全部账号积分" : "刷新全部账号积分";
  /** 紧凑模式：卡片更小、同屏更多列；默认开启，持久化到 localStorage */
  const [compact, setCompact] = useState<boolean>(() => {
    try {
      return localStorage.getItem("wb-switch.compact") !== "0";
    } catch {
      return true;
    }
  });

  function toggleCompact() {
    setCompact((value) => {
      const next = !value;
      try {
        localStorage.setItem("wb-switch.compact", next ? "1" : "0");
      } catch {
        /* 存储不可用时静默 */
      }
      return next;
    });
  }

  useEffect(() => {
    void fetchAll();
  }, [fetchAll]);

  /**
   * 首次启动自动导入本机账号（本会话只尝试一次，无本机账号时静默）。
   * 仅限默认档位：切到国际版时不静默写入账号，改由空状态引导显式导入或浏览器授权登录。
   */
  const autoImportTried = useRef(false);
  useEffect(() => {
    if (variant !== DEFAULT_VARIANT) return;
    if (autoImportTried.current || loading || visibleAccounts.length > 0) return;
    autoImportTried.current = true;
    void importLocal()
      .then(() => void fetchAll())
      .catch(() => {
        /* 本机无 WorkBuddy 登录态时静默，不打扰用户 */
      });
  }, [variant, visibleAccounts.length, loading, importLocal, fetchAll]);

  async function refreshCodebuddyCliStatus() {
    try {
      setCodebuddyCli(await api.getCodebuddyCliStatus());
    } catch {
      setCodebuddyCli(null);
    }
  }

  async function refreshCodebuddyCnIdeStatus() {
    try {
      setCodebuddyCnIde(
        variantUsesIntlCodebuddyIde(variant)
          ? await api.getCodebuddyIdeStatus()
          : await api.getCodebuddyCnIdeStatus(),
      );
    } catch {
      setCodebuddyCnIde(null);
    }
  }

  async function refreshVscodeExtStatus() {
    try {
      setVscodeExt(await api.getVscodeExtStatus());
    } catch {
      setVscodeExt(null);
    }
  }

  useEffect(() => {
    let cancelled = false;
    void refreshCodebuddyCliStatus();
    void (async () => {
      if (!api.isDemoMode()) {
        try {
          // 国际版探测 CodeBuddy.app 钥匙串；国内版探测 CodeBuddy CN。不要交叉读。
          if (variantUsesIntlCodebuddyIde(variant)) {
            await api.detectCodebuddyIdeAccount();
          } else {
            await api.detectCodebuddyCnIdeAccount();
          }
        } catch {
          /* 未登录或钥匙串拒绝时静默，下面仍拉安装/运行状态 */
        }
        try {
          await api.detectVscodeExtAccount();
        } catch {
          /* VS Code 未登录或 Safe Storage 不可用时静默 */
        }
      }
      if (!cancelled) {
        await refreshCodebuddyCnIdeStatus();
        await refreshVscodeExtStatus();
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [accounts.length, variant]);

  // 当前档位账号列表变化后并行查询各账号今日签到状态
  // 国际版没有签到接口：不查询状态（后端也不发请求）。
  useEffect(() => {
    if (!visibleAccounts.length || !checkinAvailable) return;
    let cancelled = false;
    void fetchTodayCheckinMap(
      visibleAccounts.map((account) => account.id),
      () => cancelled,
    ).then((next) => {
      if (!cancelled && Object.keys(next).length > 0) {
        setCheckinMap((prev) => ({ ...prev, ...next }));
      }
    });
    return () => {
      cancelled = true;
    };
  }, [visibleAccounts, checkinAvailable]);

  // 自动旅行配置（只读）：只用于决定账号卡片是否展示旅行 chip、是否轮询旅行状态。
  // 读取失败静默按未开启处理（不展示 chip、不发状态请求），不打扰用户。
  useEffect(() => {
    let cancelled = false;
    void api
      .getAutoTravelConfig()
      .then((config) => {
        if (!cancelled) setAutoTravelConfig(config);
      })
      .catch(() => {
        /* 配置读取失败：按未开启处理 */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function loadTravelMap(accountIds: string[], isStale?: () => boolean) {
    const next = await fetchTravelMap(accountIds, isStale);
    if (!isStale?.() && Object.keys(next).length > 0) {
      setTravelMap((prev) => ({ ...prev, ...next }));
    }
  }

  // 当前档位账号列表变化后并行查询旅行状态；之后按 TRAVEL_REFRESH_INTERVAL_MS 周期刷新，
  // 以反映后台派发/领取循环带来的状态变化。仅主窗口可见时轮询，隐藏时暂停。
  // 成长中心仅国内版开放，国际版不发请求也不展示标签；自动旅行关闭时不展示状态、不查询。
  const travelAccountIds = useMemo(
    () => visibleAccounts.map((account) => account.id),
    [visibleAccounts],
  );
  useVisibleInterval(
    () => void loadTravelMap(travelAccountIds),
    TRAVEL_REFRESH_INTERVAL_MS,
    autoTravelEnabled && travelAccountIds.length > 0,
  );

  /**
   * 模型限额台账（后端合并两条通路）：一次返回全部账号，这里转成「账号 id -> 受限模型」。
   *
   * 容错：老版本后端没有该命令、或扫描失败时按「无受限模型」处理（清空映射），
   * 不弹错误、不影响账号页其它功能。
   */
  const lastRateLimitScanRef = useRef(0);

  async function loadRateLimits(options?: { force?: boolean }) {
    if (rateLimitEnabled !== true) return;
    const scannedAt = lastRateLimitScanRef.current;
    if (
      !options?.force &&
      scannedAt > 0 &&
      Date.now() - scannedAt < RATE_LIMIT_REFRESH_INTERVAL_MS
    ) {
      return;
    }
    try {
      const payload = await api.getRateLimits();
      // `scannedAt` 是后端最近一次真实日志扫描的时刻：下一次扫描要等它满 5 分钟。
      lastRateLimitScanRef.current = payload.scannedAt || Date.now();
      const next: Record<string, RateLimitEntry[]> = {};
      for (const entry of payload.accounts ?? []) {
        if (entry.limited?.length) next[entry.accountId] = entry.limited;
      }
      setRateLimitMap(next);
    } catch {
      setRateLimitMap({});
    }
  }

  const loadRateLimitsRef = useRef(loadRateLimits);
  loadRateLimitsRef.current = loadRateLimits;

  // 兜底轮询：页面可见且距上次扫描 ≥ 5 分钟时拉一次（IDE 日志扫描在后端按同一间隔节流）。
  // 图标何时消失由卡片本地按 `resetAt` 每秒判定（跨过官方重置时刻自动消失），不依赖这里的轮询。
  useVisibleInterval(
    () => void loadRateLimits(),
    RATE_LIMIT_REFRESH_INTERVAL_MS,
    rateLimitEnabled === true,
  );

  // 后端入账 hook 事件（CLI / WorkBuddy 的 429 当轮）后推送 → 立即拉取，秒级更新。
  // 这一路不看节流：新状态已经在后端，前端只做拉取。
  useEffect(() => {
    if (api.isWebui()) return;
    let unlisten: (() => void) | undefined;
    void listen("rate-limits-updated", () => {
      void loadRateLimitsRef.current({ force: true });
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  // 「限额监听」开关（设置页）：关闭后不再发起扫描；开关状态来自后端配置文件，
  // 设置页改完返回账号页会重新挂载并读到新值。
  useEffect(() => {
    let cancelled = false;
    void api
      .getRateLimitConfig()
      .then((config) => {
        if (!cancelled) setRateLimitEnabled(config.enabled);
      })
      .catch(() => {
        // 旧版本后端没有该命令：按默认开启，不影响账号页其它功能。
        if (!cancelled) setRateLimitEnabled(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // 只给尚未缓存的账号拉积分；切回首页不重复请求。点「刷新积分」才强制更新。
  useEffect(() => {
    if (!visibleAccounts.length) return;
    void ensureCredits(visibleAccounts.map((account) => account.id));
  }, [visibleAccounts, ensureCredits]);

  async function onImport() {
    setImporting(true);
    try {
      const acc = await importLocal();
      toast.success("账号已导入", { description: acc.nickname || acc.email || acc.id });
    } catch (e) {
      toast.error("导入失败", { description: api.asError(e) });
    } finally {
      setImporting(false);
    }
  }

  /** 导出完成提示（含安全提醒）。 */
  function onExported(count: number) {
    const text = `已导出 ${count} 个账号。文件含登录 token，等同密码，请勿上传网盘或发送给他人。`;
    toast.success("导出成功", { description: text });
  }

  /** 导入完成提示：计数 + token 可能过期提醒，并刷新列表。 */
  function onImported(result: { imported: number; skipped: number; overwritten: number }) {
    void fetchAll();
    const overwriteText = result.overwritten > 0 ? `（覆盖 ${result.overwritten} 个）` : "";
    const text = `已导入 ${result.imported} 个${overwriteText}，跳过 ${result.skipped} 个。token 可能已过期，切换后可能需要重新登录。`;
    toast.success("导入成功", { description: text });
  }

  async function onDelete(a: AccountMeta) {
    // 桌面 App（Tauri WebView）不支持 window.confirm，改用 Dialog 确认
    setDeleteTarget(a);
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    const a = deleteTarget;
    setDeleteTarget(null);
    try {
      await deleteAccount(a.id);
      toast.success("账号已删除");
    } catch (e) {
      toast.error("删除失败", { description: api.asError(e) });
    }
  }

  async function onCheckin(a: AccountMeta) {
    try {
      const res = await api.checkin(a.id);
      const label =
        res.result === "success"
          ? "签到成功"
          : res.result === "already"
            ? "今天已签到"
            : "签到失败";
      const description = `${a.nickname || a.email || a.id}${res.error ? `：${res.error}` : ""}`;
      if (res.result === "error") toast.error(label, { description });
      else toast.success(label, { description });
      // 刷新该账号的今日签到状态
      try {
        const st = await api.getCheckinStatus(a.id);
        if (st.ok) setCheckinMap((prev) => ({ ...prev, [a.id]: st.todayCheckedIn }));
      } catch {
        /* ignore */
      }
      void fetchAll();
      // 签到成功/已签到会带来积分变动，force 刷新该账号积分
      if (res.result !== "error") void refreshCredits([a.id]);
    } catch (e) {
      toast.error("签到失败", { description: api.asError(e) });
    }
  }

  async function onRefresh(a: AccountMeta) {
    try {
      const res = await api.refreshAccountToken(a.id);
      const label = a.nickname || a.email || a.id;
      if (res.needsRelogin) {
        toast.error("Token 刷新失败", { description: `${label}：需重新登录${res.needsReloginReason ? `（${res.needsReloginReason}）` : ""}` });
      } else {
        toast.success("Token 已刷新", { description: label });
      }
      void fetchAll();
    } catch (e) {
      toast.error("Token 刷新失败", { description: api.asError(e) });
    }
  }

  /**
   * 当前档位的批量签到：后端一次调用完成（保留并发保护），只处理支持签到的
   * 账号；国际版没有签到接口，不参与批量签到。
   */
  async function runBatchCheckin() {
    const res = await api.checkinAll(variant);
    return res.accounts ?? [];
  }

  /**
   * 刷新按钮：先跑一轮当前档位的批量签到并重查今日签到状态，再强制刷新全部积分。
   * 国际版没有签到接口：跳过整块签到逻辑，只刷新积分。
   */
  async function onRefreshCredits() {
    if (!visibleAccounts.length || refreshingCredits || checkinAllRunning) return;
    setCheckinAllRunning(true);
    const ids = visibleAccounts.map((account) => account.id);
    try {
      if (checkinAvailable) {
        try {
          const entries = await runBatchCheckin();
          const success = entries.filter((e) => e.result === "success").length;
          const already = entries.filter((e) => e.result === "already").length;
          const failed = entries.filter((e) => e.result === "error").length;
          const inactive = entries.filter((e) => e.inactive === true || e.result === "inactive").length;
          const parts: string[] = [];
          if (success > 0) parts.push(`${success} 个签到成功`);
          if (already > 0) parts.push(`${already} 个已签到`);
          if (inactive > 0) parts.push(`${inactive} 个未开放签到`);
          if (failed > 0) parts.push(`${failed} 个失败`);
          const summary = parts.length > 0 ? parts.join("，") : "无账号需要签到";
          const counted = success + already + failed;
          if (failed > 0 && counted === failed) {
            toast.error("签到失败", { description: summary });
          } else if (counted === 0 && inactive > 0) {
            // 全部是 inactive（官方未开放签到活动）：既不算成功也不算失败，
            // 不得呈现为绿色成功（design D8）。
            toast.info("签到未开放", { description: summary });
          } else {
            toast.success("签到完成", { description: summary });
          }
          // 批量签到后重查当前档位账号的今日签到状态，无需切换页面即反映最新结果
          const next = await fetchTodayCheckinMap(ids);
          if (Object.keys(next).length > 0) {
            setCheckinMap((prev) => ({ ...prev, ...next }));
          }
        } catch (e) {
          toast.error("批量签到失败", { description: api.asError(e) });
        }
      }
      await refreshCredits(ids);
      if (autoTravelEnabled) await loadTravelMap(ids);
      toast.success("积分到期情况已刷新");
    } finally {
      setCheckinAllRunning(false);
    }
  }

  async function onSwitchCodebuddyCli(account: AccountMeta) {
    if (codebuddyCliSwitchingId !== null) return;
    setCliSwitchTarget(account);
  }

  async function confirmSwitchCodebuddyCli() {
    const account = cliSwitchTarget;
    if (!account || codebuddyCliSwitchingId !== null) return;
    setCliSwitchTarget(null);
    setCodebuddyCliSwitchingId(account.id);
    const toastId = toast.loading("正在切换 CodeBuddy CLI…", {
      description: `正在将默认账号设为 ${account.nickname || account.email || account.id}`,
    });
    try {
      // 后端一律先关闭正在运行的 CLI 再写状态（`closeRunningCli` 入参已废弃）。
      const result = await api.switchCodebuddyCliAccount(account.id);
      await refreshCodebuddyCliStatus();
      toast.success("CodeBuddy CLI 默认账号已更新", {
        id: toastId,
        description: `${account.nickname || account.email || account.id}：${result.message || "配置已更新"}`,
      });
    } catch (error) {
      toast.error("CodeBuddy CLI 切换失败", {
        id: toastId,
        description: api.asError(error),
      });
    } finally {
      setCodebuddyCliSwitchingId(null);
    }
  }

  async function onSwitchCodebuddyCnIde(account: AccountMeta) {
    if (codebuddyCnIdeSwitchingId !== null) return;
    setCodebuddyCnIdeSwitchingId(account.id);
    const toastId = toast.loading("正在切换 CodeBuddy IDE…", {
      description: "将注入凭证并重启 CodeBuddy IDE",
    });
    try {
      const result = variantUsesIntlCodebuddyIde(variant)
        ? await api.switchCodebuddyIdeAccount(account.id, true)
        : await api.switchCodebuddyCnIdeAccount(account.id, true);
      await refreshCodebuddyCnIdeStatus();
      toast.success("CodeBuddy IDE 已切换", {
        id: toastId,
        description: result.message || result.account,
      });
    } catch (error) {
      toast.error("CodeBuddy IDE 切换失败", {
        id: toastId,
        description: api.asError(error),
      });
    } finally {
      setCodebuddyCnIdeSwitchingId(null);
    }
  }

  async function onInstallCodebuddyCli() {
    // 桌面 App（Tauri WebView）不支持 window.confirm，改用 Dialog 确认
    setInstallConfirmOpen(true);
  }

  async function confirmInstallCodebuddyCli() {
    setInstallConfirmOpen(false);
    setInstallingCodebuddyCli(true);
    try {
      const result = await api.installCodebuddyCliHelper();
      toast.success("CodeBuddy CLI 接入已更新", { description: result.message });
      await refreshCodebuddyCliStatus();
    } catch (error) {
      toast.error("CodeBuddy CLI 接入失败", { description: api.asError(error) });
    } finally {
      setInstallingCodebuddyCli(false);
    }
  }

  const current = status?.current;
  const creditOrderingReady =
    visibleAccounts.length > 0 &&
    visibleAccounts.every((account) => Boolean(creditMap[account.id]) && !creditLoadingMap[account.id]);
  const orderedAccounts = creditOrderingReady
    ? visibleAccounts
        .map((account, index) => ({ account, index }))
        .sort((left, right) => {
          const leftCredit = creditMap[left.account.id];
          const rightCredit = creditMap[right.account.id];
          const rankDifference = creditPriorityRank(leftCredit) - creditPriorityRank(rightCredit);
          if (rankDifference !== 0) return rankDifference;

          const leftExpiry = soonestRelevantExpiry(leftCredit);
          const rightExpiry = soonestRelevantExpiry(rightCredit);
          if (leftExpiry !== rightExpiry) return leftExpiry - rightExpiry;

          const amountDifference = expiringSoonAmount(rightCredit) - expiringSoonAmount(leftCredit);
          if (amountDifference !== 0) return amountDifference;
          return left.index - right.index;
        })
        .map(({ account }) => account)
    : visibleAccounts;
  const priorityAccountId =
    creditOrderingReady
      ? orderedAccounts.find((account) => hasExpiringSoonCredits(creditMap[account.id]))?.id
      : undefined;
  const cliCurrentAccountId = codebuddyCli?.activeAccountId;
  const cliSwitchAccountLabel = cliSwitchTarget
    ? cliSwitchTarget.nickname || cliSwitchTarget.email || cliSwitchTarget.id
    : "";
  const workbuddyCurrentName = current
    ? current.nickname || current.email || current.uid || "未知账号"
    : "未登录";
  const codebuddyCurrentName = codebuddyCli?.configured
    ? codebuddyCli.activeAccountName || "未检测到"
    : "尚未接入";
  const cnIdeCurrentAccountId = codebuddyCnIde?.activeAccountId;
  const cnIdeCurrentName = codebuddyCnIde?.installed
    ? codebuddyCnIde.activeAccountName || "未检测到"
    : "未安装";
  const vscodeExtCurrentAccountId = vscodeExt?.activeAccountId;
  const vscodeExtCurrentName = vscodeExt?.installed
    ? vscodeExt.activeAccountName || "未检测到"
    : "未接入";
  const codebuddyUsesSettingsEnv = codebuddyCli?.authMode === "settings-env";
  return (
    <div className="mx-auto w-full max-w-[1180px] px-6 py-8 sm:px-8 sm:py-9">
      <header className="mb-6">
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h1 className="text-[28px] font-semibold tracking-tight">账号管理</h1>
            <p className="mt-2 text-sm leading-6 text-muted-foreground">
              统一管理 WorkBuddy、CodeBuddy IDE、CodeBuddy CLI 与 VS Code CodeBuddy 插件账号、积分和签到状态。
            </p>
            <Tabs
              className="mt-4 gap-0"
              value={variant}
              onValueChange={(value) => setVariant(normalizeVariant(value))}
            >
              <TabsList aria-label="WorkBuddy 档位">
                <TabsTrigger value="cn">国内版</TabsTrigger>
                <TabsTrigger value="ai">国际版</TabsTrigger>
              </TabsList>
            </Tabs>
          </div>
          <div className="flex shrink-0 items-center gap-4 pt-1">
            <div className="flex items-center gap-2.5">
              <span className="group relative inline-flex cursor-default">
                <span
                  className={
                    status?.running
                      ? "inline-flex rounded-[22%] bg-primary p-[2px] shadow-sm shadow-primary/40"
                      : "inline-flex rounded-[22%] bg-muted-foreground/30 p-[2px]"
                  }
                >
                  {variant === "ai" ? <WorkBuddyAiMark size={28} /> : <WorkBuddyMark size={28} />}
                </span>
                <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 hidden whitespace-nowrap rounded-md bg-popover px-2.5 py-1.5 text-xs text-popover-foreground shadow-lg ring-1 ring-black/5 group-hover:block">
                  {appName}：{status?.running ? "运行中" : "未运行"} · 当前账号：{workbuddyCurrentName}
                </span>
              </span>
              <span className="group relative inline-flex cursor-default">
                <span
                  className={
                    codebuddyCnIde?.installed
                      ? "inline-flex rounded-[22%] bg-primary p-[2px] shadow-sm shadow-primary/40"
                      : "inline-flex rounded-[22%] bg-muted-foreground/30 p-[2px]"
                  }
                >
                  {variantUsesIntlCodebuddyIde(variant) ? (
                    <CodeBuddyAiIdeMark size={28} />
                  ) : (
                    <CodeBuddyCnIdeMark size={28} />
                  )}
                </span>
                <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 hidden whitespace-nowrap rounded-md bg-popover px-2.5 py-1.5 text-xs text-popover-foreground shadow-lg ring-1 ring-black/5 group-hover:block">
                  {variantCodebuddyIdeName(variant)}：{codebuddyCnIde?.installed ? (codebuddyCnIde.running ? "运行中" : "已接入") : "未接入"} · 当前账号：{cnIdeCurrentName}
                </span>
              </span>
              <span className="group relative inline-flex cursor-default">
                <span
                  className={
                    vscodeExt?.installed && vscodeExt?.extensionInstalled
                      ? "inline-flex rounded-[22%] bg-primary p-[2px] shadow-sm shadow-primary/40"
                      : "inline-flex rounded-[22%] bg-muted-foreground/30 p-[2px]"
                  }
                >
                  <VscodeExtMark size={28} />
                </span>
                <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 hidden whitespace-nowrap rounded-md bg-popover px-2.5 py-1.5 text-xs text-popover-foreground shadow-lg ring-1 ring-black/5 group-hover:block">
                  VS Code CodeBuddy 插件：{!vscodeExt?.installed ? "未检测到 VS Code" : !vscodeExt.extensionInstalled ? "未安装插件" : vscodeExt.running ? "运行中" : "已接入"} · 当前账号：{vscodeExtCurrentName}
                </span>
              </span>
              <span className="group relative inline-flex cursor-default">
                <span
                  className={
                    codebuddyCli?.configured
                      ? "inline-flex rounded-[22%] bg-primary p-[2px] shadow-sm shadow-primary/40"
                      : "inline-flex rounded-[22%] bg-muted-foreground/30 p-[2px]"
                  }
                >
                  <CodeBuddyMark size={28} />
                </span>
                <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 hidden whitespace-nowrap rounded-md bg-popover px-2.5 py-1.5 text-xs text-popover-foreground shadow-lg ring-1 ring-black/5 group-hover:block">
                  CodeBuddy CLI：{codebuddyCli?.migrationRequired ? "需升级" : codebuddyCli?.configured ? "已接入" : "未接入"} · 当前账号：{codebuddyCurrentName}
                </span>
              </span>
            </div>
          </div>
        </div>
      </header>

      <div className="relative mb-6 overflow-visible rounded-2xl border border-border bg-muted/30 px-5 py-5 shadow-[0_6px_20px_rgba(15,23,42,.025)]">
        <div className="pointer-events-none absolute inset-0 overflow-hidden rounded-2xl">
          <div className="absolute -right-12 -top-20 size-44 rounded-full border-[28px] border-slate-400/[0.035]" />
        </div>
        <div className="relative flex flex-wrap items-center gap-x-5 gap-y-4">
          <div className="min-w-[190px] flex-1">
            <h2 className="text-sm font-semibold text-foreground">添加与迁移账号</h2>
            <p className="mt-1 text-xs leading-5 text-muted-foreground">
              {variant === "ai"
                ? "快速接入国际版账号，或从已有环境恢复"
                : "快速接入新账号，或从已有环境恢复"}
            </p>
          </div>
          <div className="flex flex-wrap items-center gap-2.5">
            <DemoAction>
              <Button
                className="h-10 bg-primary px-4 text-primary-foreground shadow-sm hover:bg-primary/90"
                onClick={() => setOauthOpen(true)}
              >
                {variant === "ai" ? <ExternalLink /> : <QrCode />}
                {variant === "ai" ? "OAuth 登录" : "OAuth 扫码添加"}
              </Button>
            </DemoAction>
            <DemoAction>
              <Button className="h-10 px-4" onClick={onImport} disabled={importing} variant="outline">
                {importing ? <Loader2 className="animate-spin" /> : <Download />}
                {variant === "ai" ? "导入本机国际版账号" : "导入本机账号"}
              </Button>
            </DemoAction>
          </div>
          <div className="flex items-center gap-1">
            <DemoAction>
              <Button variant="ghost" size="sm" className="h-9 px-2.5" onClick={() => setImportOpen(true)} title="从备份文件导入账号">
                <FileUp />导入备份
              </Button>
            </DemoAction>
            <DemoAction>
              <Button variant="ghost" size="sm" className="h-9 px-2.5" onClick={() => setExportOpen(true)} disabled={visibleAccounts.length === 0} title="导出账号备份">
                <FileDown />导出
              </Button>
            </DemoAction>
          </div>
        </div>
      </div>

      {error && (
        <Alert variant="destructive" className="mb-4">
          <AlertTitle>加载失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      <DiscoverAccountsBanner onAdopted={() => void fetchAll()} />

      {codebuddyCli &&
        (!codebuddyCli.configured ||
          (!codebuddyUsesSettingsEnv && !codebuddyCli.helperSupportsAccountIds) ||
          codebuddyCli.migrationRequired ||
          codebuddyCli.syncPending) && (
        <Alert className="mb-4">
          <Terminal />
          <AlertTitle>CodeBuddy CLI 接入</AlertTitle>
          <AlertDescription>
            <p>
              {codebuddyUsesSettingsEnv
                ? codebuddyCli.environmentOverride
                  ? "检测到进程环境变量 CODEBUDDY_AUTH_TOKEN。它会覆盖 settings.json；请先从 Windows 用户或系统环境变量中删除它，再重启本应用与 CodeBuddy CLI。"
                  : codebuddyCli.syncPending
                    ? "Windows CLI 认证配置与当前账号 Token 已脱节。点击更新认证后写入最新 Token；当前运行会话不会切换，请由 ACP 重新加载会话或重启 CLI 后生效。"
                    : codebuddyCli.migrationRequired
                      ? "检测到旧版 Windows helper 配置。接入后会改用 settings.json 的 env.CODEBUDDY_AUTH_TOKEN，不再执行 helper。"
                      : "Windows 使用 CodeBuddy settings.json 中的认证 Token。保活刷新只更新后续启动使用的 Token；切换账号会先关闭正在运行的 CodeBuddy CLI，重新打开 CLI 后即用新账号。"
                : codebuddyCli.migrationRequired
                  ? "检测到旧版 helper，请先升级；升级前不会将 CLI 切换显示为已验证。"
                  : codebuddyCli.configured
                    ? "当前 helper 仍按旧索引读取账号；升级后将按账号 ID 独立切换，账号增删也不会错位。"
                    : "WorkBuddy 账号与积分功能可正常使用；如需从这里切换 CodeBuddy CLI 账号，点击下方按钮一键接入。"}
            </p>
            <DemoAction>
              <Button
                className="mt-2"
                size="sm"
                variant="outline"
                onClick={() => void onInstallCodebuddyCli()}
                disabled={installingCodebuddyCli}
              >
                {installingCodebuddyCli && <Loader2 className="animate-spin" />}
                {codebuddyUsesSettingsEnv
                  ? codebuddyCli.configured ? "更新 CLI 认证" : "接入 CLI"
                  : codebuddyCli.configured || codebuddyCli.migrationRequired ? "升级 CLI helper" : "接入 CLI"}
              </Button>
            </DemoAction>
          </AlertDescription>
        </Alert>
      )}
      <section className="mt-7 min-w-0" aria-labelledby="accounts-list-title">
        <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <h2 id="accounts-list-title" className="text-base font-semibold tracking-tight">账号</h2>
            <Badge
              variant="secondary"
              className="h-6 min-w-6 rounded-full border-0 px-1.5 text-[11px] tabular-nums text-muted-foreground shadow-none"
              aria-label={`${visibleAccounts.length} 个${variantLabel(variant)}账号`}
            >
              {visibleAccounts.length}
            </Badge>
          </div>
          <TooltipProvider delayDuration={400}>
            <div className="ml-auto flex items-center gap-1">
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    className={cn("size-9 rounded-lg", compact && "bg-accent text-accent-foreground")}
                    onClick={toggleCompact}
                    aria-label={compact ? "切换为宽松模式" : "切换为紧凑模式"}
                  >
                    {compact ? <Rows3 /> : <Columns3 />}
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="top">{compact ? "切换为宽松模式" : "切换为紧凑模式"}</TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span>
                    <DemoAction>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-9 rounded-lg"
                        disabled={refreshingCredits || checkinAllRunning || visibleAccounts.length === 0}
                        onClick={() => void onRefreshCredits()}
                        aria-label={refreshCreditsLabel}
                      >
                        <RefreshCw className={refreshingCredits || checkinAllRunning ? "animate-spin" : undefined} />
                      </Button>
                    </DemoAction>
                  </span>
                </TooltipTrigger>
                <TooltipContent side="top">{api.isDemoMode() ? "演示模式下不可操作" : refreshCreditsLabel}</TooltipContent>
              </Tooltip>
            </div>
          </TooltipProvider>
        </div>
        {loading && visibleAccounts.length === 0 ? (
          <div className="flex items-center gap-2 py-16 text-sm text-muted-foreground">
            <Loader2 className="animate-spin" />
            加载账号…
          </div>
        ) : visibleAccounts.length === 0 ? (
          <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
            {variant === "ai" ? (
              <>
                <p>暂无国际版账号。</p>
                <p className="mt-2 text-xs leading-5">
                  请确认本机已安装 {appName}（客户端下载域名 {variantDownloadDomain(variant)}）并登录，
                  再点击上方「导入本机国际版账号」；也可以直接「OAuth 登录」添加国际版账号。
                </p>
              </>
            ) : (
              "暂无账号。点击上方按钮导入本机账号或扫码登录。"
            )}
          </div>
        ) : (
          <div className={cn("grid min-w-0 gap-5", compact ? "grid-cols-[repeat(auto-fit,minmax(min(100%,300px),1fr))]" : "grid-cols-[repeat(auto-fit,minmax(min(100%,340px),1fr))]")}>
            {/* 不要给这个网格加 items-start：它会覆盖 Grid 默认的 stretch，让同排卡片因内容长度不同而
                高低参差。卡片内部 article 是 flex-col、内容区是 flex-1，会自动吸收差额、footer 自动贴底对齐。 */}
            {orderedAccounts.map((a) => (
              <AccountCard
                key={a.id}
                account={a}
                compact={compact}
                onDelete={onDelete}
                onSwitch={setSwitchAccount}
                onCheckin={onCheckin}
                onRefresh={onRefresh}
                todayCheckedIn={checkinMap[a.id]}
                travelStatus={autoTravelEnabled ? travelMap[a.id] : undefined}
                rateLimits={rateLimitEnabled ? rateLimitMap[a.id] : undefined}
                credit={creditMap[a.id]}
                creditLoading={creditLoadingMap[a.id]}
                creditUpdatedAt={creditUpdatedAtMap[a.id]}
                creditPriority={a.id === priorityAccountId}
                workbuddyActive={isWorkbuddyCurrent(a, current)}
                codebuddyCliConfigured={codebuddyCli?.configured && !codebuddyCli.migrationRequired && !codebuddyCli.syncPending}
                codebuddyCliActive={a.id === cliCurrentAccountId}
                codebuddyCliBusy={codebuddyCliSwitchingId !== null}
                onSwitchCodebuddyCli={onSwitchCodebuddyCli}
                codebuddyCliLoading={codebuddyCliSwitchingId === a.id}
                codebuddyCnIdeAvailable={Boolean(codebuddyCnIde?.installed)}
                codebuddyCnIdeActive={a.id === cnIdeCurrentAccountId}
                codebuddyCnIdeBusy={codebuddyCnIdeSwitchingId !== null}
                codebuddyCnIdeLoading={codebuddyCnIdeSwitchingId === a.id}
                onSwitchCodebuddyCnIde={onSwitchCodebuddyCnIde}
                vscodeExtInstalled={Boolean(vscodeExt?.installed)}
                vscodeExtExtensionInstalled={Boolean(vscodeExt?.extensionInstalled)}
                vscodeExtAvailable={Boolean(vscodeExt?.installed && vscodeExt?.extensionInstalled)}
                vscodeExtActive={a.id === vscodeExtCurrentAccountId}
                vscodeExtBusy={vscodeSwitchAccount !== null}
                onSwitchVscodeExt={setVscodeSwitchAccount}
                featuresDisabled={false}
              />
            ))}
          </div>
        )}
      </section>

      <OAuthLoginDialog open={oauthOpen} onOpenChange={setOauthOpen} variant={variant} />
      <ExportAccountsDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        accounts={visibleAccounts}
        onExported={onExported}
      />
      <ImportAccountsDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        onImported={onImported}
        variant={variant}
      />
      <SwitchAccountDialog
        open={switchAccount !== null}
        onOpenChange={(o) => {
          if (!o) setSwitchAccount(null);
        }}
        account={switchAccount}
        onDone={() => {
          void fetchAll();
          void refreshCodebuddyCliStatus();
          void refreshCodebuddyCnIdeStatus();
        }}
      />
      <VscodeSwitchAccountDialog
        open={vscodeSwitchAccount !== null}
        onOpenChange={(o) => {
          if (!o) setVscodeSwitchAccount(null);
        }}
        account={vscodeSwitchAccount}
        vscodeExtStatus={vscodeExt}
        onDone={() => {
          void refreshVscodeExtStatus();
        }}
      />

      {/* 接入/升级 CLI 认证确认（桌面 App 不支持 window.confirm） */}
      <Dialog open={installConfirmOpen} onOpenChange={setInstallConfirmOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {codebuddyUsesSettingsEnv
                ? "更新 CodeBuddy CLI 认证"
                : codebuddyCli?.configured || codebuddyCli?.migrationRequired
                  ? "升级 CodeBuddy CLI helper"
                  : "接入 CodeBuddy CLI"}
            </DialogTitle>
            <DialogDescription>
              {codebuddyUsesSettingsEnv ? (
                <>
                  将把当前账号的认证 Token 写入
                  <code className="mx-1 rounded bg-muted px-1">~/.codebuddy/settings.json</code>
                  的 <code className="mx-1 rounded bg-muted px-1">env.CODEBUDDY_AUTH_TOKEN</code>。
                  其他配置会保留；更新只影响后续加载的会话，当前运行会话不会切换。是否继续？
                </>
              ) : (
                <>
                  {codebuddyCli?.configured || codebuddyCli?.migrationRequired ? "升级" : "接入"}会自动写入
                  <code className="mx-1 rounded bg-muted px-1">~/.codebuddy-rotate/helper.cjs</code>
                  并更新
                  <code className="mx-1 rounded bg-muted px-1">~/.codebuddy/settings.json</code>
                  的 apiKeyHelper 配置，是否继续？
                </>
              )}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setInstallConfirmOpen(false)}>
              取消
            </Button>
            <Button onClick={() => void confirmInstallCodebuddyCli()}>继续</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 切换 CodeBuddy CLI 确认（桌面 App 不支持 window.confirm） */}
      <Dialog open={cliSwitchTarget !== null} onOpenChange={(open) => !open && setCliSwitchTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>切换 CodeBuddy CLI</DialogTitle>
            <DialogDescription>
              将把 CodeBuddy CLI 默认账号设为「{cliSwitchAccountLabel}」。
              确认后会关闭正在运行的 CodeBuddy CLI 会话，当前会话会中断；重新打开 CLI 后新账号才会生效。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCliSwitchTarget(null)}>
              取消
            </Button>
            <Button onClick={() => void confirmSwitchCodebuddyCli()}>
              关闭 CLI 并切换
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 删除账号确认 */}
      <Dialog open={deleteTarget !== null} onOpenChange={(o) => !o && setDeleteTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>删除账号</DialogTitle>
            <DialogDescription>
              确定删除账号「{deleteTarget?.nickname || deleteTarget?.email || deleteTarget?.id}」？
              此操作不可撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              取消
            </Button>
            <Button variant="destructive" onClick={() => void confirmDelete()}>
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
