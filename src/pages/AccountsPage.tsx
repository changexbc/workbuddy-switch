import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  Columns3,
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
import { AccountInfoDialog } from "@/components/account-info-dialog";
import { JetbrainsSwitchDialog } from "@/components/jetbrains-switch-dialog";
import { CodebuddyIdeSwitchAccountDialog } from "@/components/codebuddy-ide-switch-account-dialog";
import { DemoAction } from "@/components/demo-action";
import {
  CodeBuddyAiIdeMark,
  CodeBuddyCnIdeMark,
  CodeBuddyMark,
  JetbrainsMark,
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
import { useT } from "@/lib/i18n";
import { useVisibleInterval } from "@/lib/use-visible-interval";
import {
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
import { useSupportedTools } from "@/lib/supported-tools";
import type { AccountMeta, AppStatus, CheckinConfig, CreditExpiry, RateLimitEntry, TravelConfig, TravelStatus } from "@/lib/types";
import { displayName } from "@/lib/account-display";
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
/**
 * CodeBuddy CLI 认证状态的重读间隔。
 *
 * 保活刷新会先批量改写账号库里的 token、再把新 token 同步回
 * `~/.codebuddy/settings.json`。这个窗口里状态判定会短暂认为「认证已脱节」。
 * 后端在刷新前后各广播一次 `codebuddy-cli-updated`（见 lib.rs 保活循环），
 * 这里再挂一个可见时轮询兜底：即使事件因窗口未挂载而错过，横幅也会自行收掉。
 */
const CLI_STATUS_REFRESH_INTERVAL_MS = 30 * 1000;

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
        if (isStale?.() || !res.ok || typeof res.todayCheckedIn !== "boolean") return null;
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
  const t = useT();
  const {
    accounts,
    variant,
    setVariant,
    status,
    loading,
    error,
    fetchAll,
    deleteAccount,
    creditMap,
    creditLoadingMap,
    creditUpdatedAtMap,
    refreshingCredits,
    ensureCredits,
    refreshCredits,
    clientStatus,
    setClientStatus,
  } = useAccountsStore();
  const [oauthOpen, setOauthOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [switchAccount, setSwitchAccount] = useState<AccountMeta | null>(null);
  /** 「账号信息」弹框目标账号（查看信息 / 编辑备注 / 选择显示字段）。 */
  const [infoTarget, setInfoTarget] = useState<AccountMeta | null>(null);
  /**
   * 自动签到配置（只读）：只用于决定账号卡片是否展示「自动签到已关闭」chip，
   * 以及状态查询、刷新时跳过哪些账号。控制入口在设置页。
   */
  const [autoCheckinConfig, setAutoCheckinConfig] = useState<CheckinConfig | null>(null);
  /** 配置是否已读取完毕（成功或失败）：区分「尚未读到」与「读取失败」。 */
  const [autoCheckinSettled, setAutoCheckinSettled] = useState(false);
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
  /**
   * 各客户端状态（CLI / CodeBuddy IDE / VS Code / JetBrains）放在 store 里：
   * 账号页每次进入都会重挂载，局部 state 会被重置为 `null`，界面先按「未接入」
   * 渲染、等状态探测回来才改口（issue #84）。store 里则先按上次结果渲染。
   */
  const { codebuddyCli, codebuddyCnIde, vscodeExt, jetbrains } = clientStatus;
  const [codebuddyCliSwitchingId, setCodebuddyCliSwitchingId] = useState<string | null>(null);
  /** CodeBuddy IDE 切换弹窗目标（null=关闭）；切换与可选会话复制/同步在弹窗内完成（国内版 / 国际版共用）。 */
  const [codebuddyIdeSwitchAccount, setCodebuddyIdeSwitchAccount] = useState<AccountMeta | null>(null);
  /** VS Code 扩展切换弹窗目标（null=关闭）；切换与可选会话复制在弹窗内完成。 */
  const [vscodeSwitchAccount, setVscodeSwitchAccount] = useState<AccountMeta | null>(null);
  /** JetBrains 切换弹窗目标（null=关闭）；切换与目标 IDE 选择在弹窗内完成。 */
  const [jetbrainsSwitchTarget, setJetbrainsSwitchTarget] = useState<AccountMeta | null>(null);
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
  const refreshCreditsLabel = checkinAvailable
    ? t("accounts.list.refreshCreditsAndCheckin")
    : t("accounts.list.refreshCredits");
  /** 关闭自动签到的账号 id（配置未读到/读取失败 = 空名单）。 */
  const excludedCheckinIds = useMemo(
    () => new Set(autoCheckinConfig?.excluded_account_ids ?? []),
    [autoCheckinConfig],
  );
  /** 今日签到状态只查未关闭自动签到的账号；配置就绪前不发请求。 */
  const autoCheckinAccountIds = useMemo(() => {
    if (!checkinAvailable || !autoCheckinSettled) return [];
    return visibleAccounts
      .filter((account) => !excludedCheckinIds.has(account.id))
      .map((account) => account.id);
  }, [visibleAccounts, checkinAvailable, autoCheckinSettled, excludedCheckinIds]);
  /** 紧凑模式：卡片更小、同屏更多列；默认开启，持久化到 localStorage */
  const [compact, setCompact] = useState<boolean>(() => {
    try {
      return localStorage.getItem("wb-switch.compact") !== "0";
    } catch {
      return true;
    }
  });

  /**
   * 支持工具开关（设置页）：关闭的端不渲染入口、不轮询状态。
   * 缺省 = 现有四端开、JetBrains 关（与 `src/lib/supported-tools.ts` 的默认值一致）。
   */
  const enabledTools = useSupportedTools();

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

  // 自动签到配置（只读）：读取失败静默回落为空名单（照常查询状态），不打扰用户。
  useEffect(() => {
    let cancelled = false;
    void api
      .getAutoCheckinConfig()
      .then((config) => {
        if (!cancelled) setAutoCheckinConfig(config);
      })
      .catch(() => {
        /* 配置读取失败：按空名单处理 */
      })
      .finally(() => {
        if (!cancelled) setAutoCheckinSettled(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function refreshCodebuddyCliStatus() {
    try {
      setClientStatus({ codebuddyCli: await api.getCodebuddyCliStatus() });
    } catch {
      setClientStatus({ codebuddyCli: null });
    }
  }

  async function refreshCodebuddyCnIdeStatus() {
    try {
      setClientStatus({
        codebuddyCnIde: variantUsesIntlCodebuddyIde(variant)
          ? await api.getCodebuddyIdeStatus()
          : await api.getCodebuddyCnIdeStatus(),
      });
    } catch {
      setClientStatus({ codebuddyCnIde: null });
    }
  }

  async function refreshVscodeExtStatus() {
    try {
      setClientStatus({ vscodeExt: await api.getVscodeExtStatus() });
    } catch {
      setClientStatus({ vscodeExt: null });
    }
  }

  async function refreshJetbrainsStatus() {
    try {
      setClientStatus({ jetbrains: await api.getJetbrainsStatus() });
    } catch {
      setClientStatus({ jetbrains: null });
    }
  }

  useEffect(() => {
    let cancelled = false;
    // 支持工具关闭的端：既不探测也不轮询（与入口隐藏保持一致，省掉无谓请求）。
    if (enabledTools.codebuddyCli) void refreshCodebuddyCliStatus();

    /**
     * 读各端状态（安装 / 运行 / 当前账号）：只读本地状态文件与进程，不碰钥匙串，
     * 因此不必等下面的本机登录探测。
     */
    async function refreshClientStatuses() {
      if (cancelled) return;
      if (enabledTools.codebuddyIde) await refreshCodebuddyCnIdeStatus();
      if (cancelled) return;
      if (enabledTools.vscodeExt) await refreshVscodeExtStatus();
      if (cancelled) return;
      if (enabledTools.jetbrains) await refreshJetbrainsStatus();
    }

    void (async () => {
      // 状态刷新与登录探测并行起跑：探测要读钥匙串 / Safe Storage / 注册表 / 进程
      // （macOS 可能等待系统授权、Windows 走 PowerShell，耗时可达数秒），排在状态
      // 前面会让「已接入」迟迟不显示（issue #84）。
      const statuses = refreshClientStatuses();
      if (!api.isDemoMode()) {
        if (enabledTools.codebuddyIde) {
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
        }
        if (enabledTools.vscodeExt) {
          try {
            await api.detectVscodeExtAccount();
          } catch {
            /* VS Code 未登录或 Safe Storage 不可用时静默 */
          }
        }
        if (enabledTools.jetbrains) {
          try {
            await api.detectJetbrainsAccount();
          } catch {
            /* JetBrains 插件未登录时静默 */
          }
        }
      }
      await statuses;
      // 探测命中账号时后端会把「当前账号」写回本地状态，再读一次让高亮跟上。
      if (!cancelled) await refreshClientStatuses();
    })();
    return () => {
      cancelled = true;
    };
  }, [accounts.length, variant, enabledTools]);

  // 配置就绪后查询未关闭自动签到的账号；国际版没有签到接口，不查询状态。
  useEffect(() => {
    if (!autoCheckinAccountIds.length) return;
    let cancelled = false;
    void fetchTodayCheckinMap(
      autoCheckinAccountIds,
      () => cancelled,
    ).then((next) => {
      if (!cancelled && Object.keys(next).length > 0) {
        setCheckinMap((prev) => ({ ...prev, ...next }));
      }
    });
    return () => {
      cancelled = true;
    };
  }, [autoCheckinAccountIds]);

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

  // 事件监听与定时器都需要「最新」的刷新函数：直接闭包捕获会在状态更新后仍然
  // 指向旧引用，导致拉回的仍是挂载时的旧判断。
  const refreshCodebuddyCliStatusRef = useRef(refreshCodebuddyCliStatus);
  refreshCodebuddyCliStatusRef.current = refreshCodebuddyCliStatus;

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

  /**
   * CLI 认证状态变化（保活刷新前后、切换账号、接入 helper）→ 立即重读。
   *
   * 没有这一路时，页面只在挂载时读一次状态：若恰好落在保活刷新的中间态，
   * 判出来的「认证已脱节」会一直留在页面上，用户点什么都要等下次重挂载。
   */
  useEffect(() => {
    if (api.isWebui()) return;
    let unlisten: (() => void) | undefined;
    void listen("codebuddy-cli-updated", () => {
      void refreshCodebuddyCliStatusRef.current();
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  // 兜底轮询：事件可能因窗口尚未挂载而错过（例如后台刷新先于页面加载完成），
  // 仅主窗口可见时执行，保证横幅最终一定会自愈。
  useVisibleInterval(
    () => void refreshCodebuddyCliStatusRef.current(),
    CLI_STATUS_REFRESH_INTERVAL_MS,
    true,
  );

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

  /** 导出完成提示（含安全提醒）。 */
  function onExported(count: number) {
    const text = t("accounts.toast.exportedDescription", { count });
    toast.success(t("accounts.toast.exported"), { description: text });
  }

  /** 导入完成提示：计数 + token 可能过期提醒（含加密凭据能力限制），并刷新列表。 */
  function onImported(result: {
    imported: number;
    skipped: number;
    overwritten: number;
    encrypted: number;
  }) {
    void fetchAll();
    const overwriteText =
      result.overwritten > 0 ? t("accounts.toast.importedOverwritten", { count: result.overwritten }) : "";
    const encryptedText =
      result.encrypted > 0
        ? t("accounts.toast.importedEncrypted", { count: result.encrypted })
        : "";
    const text = [
      t("accounts.toast.importedDescription", { imported: result.imported, skipped: result.skipped }),
      overwriteText,
      encryptedText,
      t("accounts.toast.importedTokenWarning"),
    ]
      .filter(Boolean)
      .join(" ");
    toast.success(t("accounts.toast.imported"), { description: text });
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
      toast.success(t("accounts.toast.deleted"));
    } catch (e) {
      toast.error(t("accounts.toast.deleteFailed"), { description: api.asError(e) });
    }
  }

  async function onCheckin(a: AccountMeta) {
    try {
      const res = await api.checkin(a.id);
      const label =
        res.result === "success"
          ? t("accounts.toast.checkinSuccess")
          : res.result === "already"
            ? t("accounts.toast.checkinAlready")
            : t("accounts.toast.checkinFailed");
      const description = res.error
        ? t("accounts.toast.accountError", { account: displayName(a), error: res.error })
        : displayName(a);
      if (res.result === "error") toast.error(label, { description });
      else toast.success(label, { description });
      // 手动签到已完成状态核验，直接使用回执，避免为已关闭账号再触发展示查询。
      if (res.result === "success" || res.result === "already") {
        setCheckinMap((prev) => ({ ...prev, [a.id]: true }));
      }
      void fetchAll();
      // 签到成功/已签到会带来积分变动，force 刷新该账号积分
      if (res.result !== "error") void refreshCredits([a.id]);
    } catch (e) {
      toast.error(t("accounts.toast.checkinFailed"), { description: api.asError(e) });
    }
  }

  async function onRefresh(a: AccountMeta) {
    try {
      const res = await api.refreshAccountToken(a.id);
      const label = displayName(a);
      if (res.needsRelogin) {
        toast.error(t("accounts.toast.tokenRefreshFailed"), {
          description: res.needsReloginReason
            ? t("accounts.toast.needsReloginWithReason", { account: label, reason: res.needsReloginReason })
            : t("accounts.toast.needsRelogin", { account: label }),
        });
      } else {
        toast.success(t("accounts.toast.tokenRefreshed"), { description: label });
      }
      void fetchAll();
    } catch (e) {
      toast.error(t("accounts.toast.tokenRefreshFailed"), { description: api.asError(e) });
    }
  }

  /** 刷新附带的签到遵守账号开关与签到时间段；所有账号照常刷新积分，提示实际忽略/未到时间段数量。 */
  async function onRefreshCredits() {
    if (!visibleAccounts.length || refreshingCredits || checkinAllRunning) return;
    setCheckinAllRunning(true);
    const ids = visibleAccounts.map((account) => account.id);
    let summary = "";
    let notify = toast.success;
    let title = t("accounts.toast.creditsRefreshed");
    try {
      if (checkinAvailable) {
        try {
          const res = await api.checkinAll(variant, true);
          const entries = res.accounts ?? [];
          const success = entries.filter((e) => e.result === "success").length;
          const already = entries.filter((e) => e.result === "already").length;
          const failed = entries.filter((e) => e.result === "error").length;
          const inactive = entries.filter((e) => e.inactive === true || e.result === "inactive").length;
          const skipped = entries.filter((e) => e.result === "skipped" && e.reason === "auto_checkin_disabled").length;
          const outsideWindow = entries.filter((e) => e.result === "skipped" && e.reason === "outside_checkin_window").length;
          const parts: string[] = [];
          if (success > 0) parts.push(t("accounts.toast.checkinPartSuccess", { count: success }));
          if (already > 0) parts.push(t("accounts.toast.checkinPartAlready", { count: already }));
          if (inactive > 0) parts.push(t("accounts.toast.checkinPartInactive", { count: inactive }));
          if (failed > 0) parts.push(t("accounts.toast.checkinPartFailed", { count: failed }));
          if (skipped > 0) parts.push(t("accounts.toast.checkinPartSkipped", { count: skipped }));
          if (outsideWindow > 0) parts.push(t("accounts.toast.checkinPartOutsideWindow", { count: outsideWindow }));
          summary = res.status === "skipped" && res.reason === "already_running"
            ? t("accounts.toast.checkinRunning")
            : parts.length > 0 ? parts.join(", ") : t("accounts.toast.checkinNone");
          const allFailed = entries.length > 0 && failed === entries.length;
          const allSkippedOrInactive =
            res.status === "skipped" ||
            entries.length === 0 ||
            (success === 0 && already === 0 && failed === 0);
          if (allFailed) {
            // 全部失败：没有成功、已签、未开放或忽略的账号。
            notify = toast.error;
            title = t("accounts.toast.creditsRefreshedCheckinError");
          } else if (allSkippedOrInactive) {
            // 全部被跳过或官方未开放签到活动：既不算成功也不算失败，不呈现为绿色成功。
            notify = toast.info;
          }
          // 只重查实际处理过的账号状态；被跳过的账号本次未发请求，状态保持未知。
          const next = await fetchTodayCheckinMap(entries.filter((e) => e.result !== "skipped").map((e) => e.accountId));
          if (Object.keys(next).length > 0) {
            setCheckinMap((prev) => ({ ...prev, ...next }));
          }
        } catch (e) {
          notify = toast.error;
          title = t("accounts.toast.creditsRefreshedCheckinError");
          summary = api.asError(e);
        }
      }
      await refreshCredits(ids);
      if (autoTravelEnabled) await loadTravelMap(ids);
      notify(title, { description: summary || undefined });
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
    const toastId = toast.loading(t("accounts.toast.cliSwitching"), {
      description: t("accounts.toast.cliSwitchingDescription", { account: displayName(account) }),
    });
    try {
      // 后端一律先关闭正在运行的 CLI 再写状态（`closeRunningCli` 入参已废弃）。
      const result = await api.switchCodebuddyCliAccount(account.id);
      await refreshCodebuddyCliStatus();
      toast.success(t("accounts.toast.cliSwitchSuccess"), {
        id: toastId,
        description: t("accounts.toast.cliSwitchDescription", {
          account: displayName(account),
          message: result.message || t("accounts.toast.configUpdated"),
        }),
      });
    } catch (error) {
      toast.error(t("accounts.toast.cliSwitchFailed"), {
        id: toastId,
        description: api.asError(error),
      });
    } finally {
      setCodebuddyCliSwitchingId(null);
    }
  }

  async function onSwitchCodebuddyCnIde(account: AccountMeta) {
    if (codebuddyIdeSwitchAccount !== null) return;
    // 国内版与国际版共用同一弹窗（关联会话 / 复制会话两个 tab），只有数据源与切换接口按档位分流；
    // 弹窗本身承担确认职责（不勾选时行为与一键切换一致），不再另设轻量确认框。
    setCodebuddyIdeSwitchAccount(account);
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
      toast.success(t("accounts.toast.cliInstallSuccess"), { description: result.message });
      await refreshCodebuddyCliStatus();
    } catch (error) {
      toast.error(t("accounts.toast.cliInstallFailed"), { description: api.asError(error) });
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
  const cliSwitchAccountLabel = cliSwitchTarget ? displayName(cliSwitchTarget) : "";
  const workbuddyCurrentName = current ? displayName(current) : t("accounts.status.signedOut");
  const codebuddyCurrentName = codebuddyCli?.configured
    ? codebuddyCli.activeAccountName || t("accounts.status.notFound")
    : t("accounts.status.notConnected");
  const cnIdeCurrentAccountId = codebuddyCnIde?.activeAccountId;
  const cnIdeCurrentName = codebuddyCnIde?.installed
    ? codebuddyCnIde.activeAccountName || t("accounts.status.notFound")
    : t("accounts.status.notInstalled");
  const vscodeExtCurrentAccountId = vscodeExt?.activeAccountId;
  const vscodeExtCurrentName = vscodeExt?.installed
    ? vscodeExt.activeAccountName || t("accounts.status.notFound")
    : t("accounts.status.notConnected");
  const jetbrainsCurrentAccountId = jetbrains?.activeAccountId;
  const jetbrainsCurrentName = jetbrains?.installed
    ? jetbrains.activeAccountName || t("accounts.status.notFound")
    : t("accounts.status.notConnected");
  const codebuddyUsesSettingsEnv = codebuddyCli?.authMode === "settings-env";
  return (
    <div className="mx-auto w-full max-w-[1180px] px-6 py-8 sm:px-8 sm:py-9">
      <header className="mb-6">
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h1 className="text-[28px] font-semibold tracking-tight">{t("accounts.page.title")}</h1>
            <p className="mt-2 text-sm leading-6 text-muted-foreground">
              {t("accounts.page.description")}
            </p>
            <Tabs
              className="mt-4 gap-0"
              value={variant}
              onValueChange={(value) => setVariant(normalizeVariant(value))}
            >
              <TabsList aria-label={t("accounts.page.variantTabsLabel")}>
                <TabsTrigger value="cn">{variantLabel("cn")}</TabsTrigger>
                <TabsTrigger value="ai">{variantLabel("ai")}</TabsTrigger>
              </TabsList>
            </Tabs>
          </div>
          <div className="flex shrink-0 items-center gap-4 pt-1">
            <div className="flex items-center gap-2.5">
{enabledTools.workbuddy && (
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
                  {t("accounts.status.tooltip", {
                    product: appName,
                    state: status?.running ? t("accounts.status.running") : t("accounts.status.notRunning"),
                    account: workbuddyCurrentName,
                  })}
                </span>
              </span>
            )}
{enabledTools.codebuddyIde && (
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
                  {t("accounts.status.tooltip", {
                    product: variantCodebuddyIdeName(variant),
                    state: codebuddyCnIde?.installed
                      ? codebuddyCnIde.running
                        ? t("accounts.status.running")
                        : t("accounts.status.connected")
                      : t("accounts.status.notConnected"),
                    account: cnIdeCurrentName,
                  })}
                </span>
              </span>
            )}
{enabledTools.vscodeExt && (
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
                  {t("accounts.status.tooltip", {
                    product: t("accounts.status.vscodeProduct"),
                    state: !vscodeExt?.installed
                      ? t("accounts.status.vscodeMissing")
                      : !vscodeExt.extensionInstalled
                        ? t("accounts.status.extensionMissing")
                        : vscodeExt.running
                          ? t("accounts.status.running")
                          : t("accounts.status.connected"),
                    account: vscodeExtCurrentName,
                  })}
                </span>
              </span>
            )}
{enabledTools.jetbrains && (
              <span className="group relative inline-flex cursor-default">
                <span
                  className={
                    jetbrains?.installed && jetbrains?.pluginInstalled
                      ? "inline-flex rounded-[22%] bg-primary p-[2px] shadow-sm shadow-primary/40"
                      : "inline-flex rounded-[22%] bg-muted-foreground/30 p-[2px]"
                  }
                >
                  <JetbrainsMark size={28} />
                </span>
                <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 hidden whitespace-nowrap rounded-md bg-popover px-2.5 py-1.5 text-xs text-popover-foreground shadow-lg ring-1 ring-black/5 group-hover:block">
                  {t("accounts.status.tooltip", {
                    product: t("accounts.status.jetbrainsProduct"),
                    state: !jetbrains?.installed
                      ? t("accounts.status.jetbrainsMissing")
                      : !jetbrains.pluginInstalled
                        ? t("accounts.status.extensionMissing")
                        : jetbrains.running
                          ? t("accounts.status.running")
                          : t("accounts.status.connected"),
                    account: jetbrainsCurrentName,
                  })}
                </span>
              </span>
            )}
{enabledTools.codebuddyCli && (
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
                  {t("accounts.status.tooltip", {
                    product: "CodeBuddy CLI",
                    state: codebuddyCli?.migrationRequired
                      ? t("accounts.status.upgradeRequired")
                      : codebuddyCli?.configured
                        ? t("accounts.status.connected")
                        : t("accounts.status.notConnected"),
                    account: codebuddyCurrentName,
                  })}
                </span>
              </span>
            )}
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
            <h2 className="text-sm font-semibold text-foreground">{t("accounts.add.title")}</h2>
            <p className="mt-1 text-xs leading-5 text-muted-foreground">
              {variant === "ai"
                ? t("accounts.add.descriptionInternational")
                : t("accounts.add.description")}
            </p>
          </div>
          <div className="flex flex-wrap items-center gap-2.5">
            <DemoAction>
              <Button
                className="h-10 bg-primary px-4 text-primary-foreground shadow-sm hover:bg-primary/90"
                onClick={() => setOauthOpen(true)}
              >
                {variant === "ai" ? <ExternalLink /> : <QrCode />}
                {variant === "ai" ? t("accounts.add.oauthLogin") : t("accounts.add.oauthQr")}
              </Button>
            </DemoAction>
          </div>
          <div className="flex items-center gap-1">
            <DemoAction>
              <Button variant="ghost" size="sm" className="h-9 px-2.5" onClick={() => setImportOpen(true)} title={t("accounts.add.importTitle")}>
                <FileUp />{t("accounts.add.import")}
              </Button>
            </DemoAction>
            <DemoAction>
              <Button variant="ghost" size="sm" className="h-9 px-2.5" onClick={() => setExportOpen(true)} disabled={visibleAccounts.length === 0} title={t("accounts.add.exportTitle")}>
                <FileDown />{t("accounts.add.export")}
              </Button>
            </DemoAction>
          </div>
        </div>
      </div>

      {error && (
        <Alert variant="destructive" className="mb-4">
          <AlertTitle>{t("accounts.page.loadFailed")}</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {codebuddyCli &&
        (!codebuddyCli.configured ||
          (!codebuddyUsesSettingsEnv && !codebuddyCli.helperSupportsAccountIds) ||
          codebuddyCli.migrationRequired ||
          codebuddyCli.syncPending ||
          codebuddyCli.syncInProgress) && (
        <Alert className="mb-4">
          <Terminal />
          <AlertTitle>{t("accounts.cli.title")}</AlertTitle>
          <AlertDescription>
            <p>
              {codebuddyUsesSettingsEnv
                ? codebuddyCli.environmentOverride
                  ? t("accounts.cli.envOverride")
                  : codebuddyCli.syncPending
                    ? t("accounts.cli.syncPending")
                    : codebuddyCli.syncInProgress
                      ? t("accounts.cli.syncInProgress")
                      : codebuddyCli.migrationRequired
                        ? t("accounts.cli.migrationSettingsEnv")
                        : t("accounts.cli.settingsEnvInfo")
                : codebuddyCli.migrationRequired
                  ? t("accounts.cli.migrationHelper")
                  : codebuddyCli.configured
                    ? t("accounts.cli.indexedHelper")
                    : t("accounts.cli.notConfigured")}
            </p>
            {/* 同步进行中是正常的中间态：给状态说明但不逼用户点按钮，
                否则用户会在刷新未完成时重复触发写入。 */}
            {!codebuddyCli.syncInProgress && (
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
                    ? codebuddyCli.configured ? t("accounts.cli.updateAuth") : t("accounts.cli.connect")
                    : codebuddyCli.configured || codebuddyCli.migrationRequired ? t("accounts.cli.upgradeHelper") : t("accounts.cli.connect")}
                </Button>
              </DemoAction>
            )}
          </AlertDescription>
        </Alert>
      )}
      <section className="mt-7 min-w-0" aria-labelledby="accounts-list-title">
        <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <h2 id="accounts-list-title" className="text-base font-semibold tracking-tight">{t("accounts.list.title")}</h2>
            <Badge
              variant="secondary"
              className="h-6 min-w-6 rounded-full border-0 px-1.5 text-[11px] tabular-nums text-muted-foreground shadow-none"
              aria-label={t("accounts.list.countLabel", { count: visibleAccounts.length, variant: variantLabel(variant) })}
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
                    aria-label={compact ? t("accounts.list.comfortable") : t("accounts.list.compact")}
                  >
                    {compact ? <Rows3 /> : <Columns3 />}
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="top">{compact ? t("accounts.list.comfortable") : t("accounts.list.compact")}</TooltipContent>
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
                <TooltipContent side="top">{api.isDemoMode() ? t("accounts.list.demoDisabled") : refreshCreditsLabel}</TooltipContent>
              </Tooltip>
            </div>
          </TooltipProvider>
        </div>
        {loading && visibleAccounts.length === 0 ? (
          <div className="flex items-center gap-2 py-16 text-sm text-muted-foreground">
            <Loader2 className="animate-spin" />
            {t("accounts.list.loading")}
          </div>
        ) : visibleAccounts.length === 0 ? (
          <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
            {variant === "ai" ? (
              <>
                <p>{t("accounts.empty.internationalTitle")}</p>
                <p className="mt-2 text-xs leading-5">
                  {t("accounts.empty.internationalHint", {
                    app: appName,
                    domain: variantDownloadDomain(variant),
                  })}
                </p>
              </>
            ) : (
              t("accounts.empty.domestic")
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
                onShowInfo={setInfoTarget}
                onCheckin={checkinAvailable ? onCheckin : undefined}
                onRefresh={onRefresh}
                todayCheckedIn={checkinMap[a.id]}
                autoCheckinAllowed={checkinAvailable && autoCheckinSettled ? !excludedCheckinIds.has(a.id) : undefined}
                travelStatus={autoTravelEnabled ? travelMap[a.id] : undefined}
                rateLimits={rateLimitEnabled ? rateLimitMap[a.id] : undefined}
                credit={creditMap[a.id]}
                creditLoading={creditLoadingMap[a.id]}
                creditUpdatedAt={creditUpdatedAtMap[a.id]}
                creditPriority={a.id === priorityAccountId}
                workbuddyActive={enabledTools.workbuddy && isWorkbuddyCurrent(a, current)}
                codebuddyCliConfigured={codebuddyCli?.configured && !codebuddyCli.migrationRequired && !codebuddyCli.syncPending && !codebuddyCli.syncInProgress}
                codebuddyCliActive={enabledTools.codebuddyCli && a.id === cliCurrentAccountId}
                codebuddyCliBusy={codebuddyCliSwitchingId !== null}
                onSwitchCodebuddyCli={onSwitchCodebuddyCli}
                codebuddyCliLoading={codebuddyCliSwitchingId === a.id}
                codebuddyCnIdeAvailable={Boolean(codebuddyCnIde?.installed)}
                codebuddyCnIdeActive={enabledTools.codebuddyIde && a.id === cnIdeCurrentAccountId}
                codebuddyCnIdeBusy={codebuddyIdeSwitchAccount !== null}
                onSwitchCodebuddyCnIde={onSwitchCodebuddyCnIde}
                vscodeExtInstalled={Boolean(vscodeExt?.installed)}
                vscodeExtExtensionInstalled={Boolean(vscodeExt?.extensionInstalled)}
                vscodeExtAvailable={Boolean(vscodeExt?.installed && vscodeExt?.extensionInstalled)}
                vscodeExtActive={enabledTools.vscodeExt && a.id === vscodeExtCurrentAccountId}
                vscodeExtBusy={vscodeSwitchAccount !== null}
                onSwitchVscodeExt={setVscodeSwitchAccount}
                jetbrainsInstalled={Boolean(jetbrains?.installed)}
                jetbrainsPluginInstalled={Boolean(jetbrains?.pluginInstalled)}
                jetbrainsAvailable={Boolean(jetbrains?.installed && jetbrains?.pluginInstalled)}
                jetbrainsActive={enabledTools.jetbrains && a.id === jetbrainsCurrentAccountId}
                jetbrainsBusy={jetbrainsSwitchTarget !== null}
                onSwitchJetbrains={setJetbrainsSwitchTarget}
                enabledTools={enabledTools}
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
      <AccountInfoDialog
        open={infoTarget !== null}
        onOpenChange={(o) => {
          if (!o) setInfoTarget(null);
        }}
        account={infoTarget}
        onSaved={() => {
          void fetchAll();
        }}
      />
      <CodebuddyIdeSwitchAccountDialog
        open={codebuddyIdeSwitchAccount !== null}
        onOpenChange={(o) => {
          if (!o) setCodebuddyIdeSwitchAccount(null);
        }}
        account={codebuddyIdeSwitchAccount}
        variant={variant}
        ideStatus={codebuddyCnIde}
        onDone={() => {
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
      <JetbrainsSwitchDialog
        open={jetbrainsSwitchTarget !== null}
        onOpenChange={(o) => {
          if (!o) setJetbrainsSwitchTarget(null);
        }}
        account={jetbrainsSwitchTarget}
        jetbrainsStatus={jetbrains}
        onDone={() => {
          void refreshJetbrainsStatus();
        }}
      />

      {/* 接入/升级 CLI 认证确认（桌面 App 不支持 window.confirm） */}
      <Dialog open={installConfirmOpen} onOpenChange={setInstallConfirmOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {codebuddyUsesSettingsEnv
                ? t("accounts.dialog.installUpdateAuthTitle")
                : codebuddyCli?.configured || codebuddyCli?.migrationRequired
                  ? t("accounts.dialog.installUpgradeHelperTitle")
                  : t("accounts.dialog.installConnectTitle")}
            </DialogTitle>
            <DialogDescription>
              {codebuddyUsesSettingsEnv ? (
                <>
                  {t("accounts.dialog.settingsEnvLead")}
                  <code className="mx-1 rounded bg-muted px-1">~/.codebuddy/settings.json</code>
                  {t("accounts.dialog.settingsEnvMiddle")}
                  <code className="mx-1 rounded bg-muted px-1">env.CODEBUDDY_AUTH_TOKEN</code>
                  {t("accounts.dialog.settingsEnvTail")}
                </>
              ) : (
                <>
                  {codebuddyCli?.configured || codebuddyCli?.migrationRequired
                    ? t("accounts.dialog.helperLeadUpgrade")
                    : t("accounts.dialog.helperLeadConnect")}
                  <code className="mx-1 rounded bg-muted px-1">~/.codebuddy-rotate/helper.cjs</code>
                  {t("accounts.dialog.helperMiddle")}
                  <code className="mx-1 rounded bg-muted px-1">~/.codebuddy/settings.json</code>
                  {t("accounts.dialog.helperTail")}
                </>
              )}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setInstallConfirmOpen(false)}>
              {t("common.cancel")}
            </Button>
            <Button onClick={() => void confirmInstallCodebuddyCli()}>{t("accounts.dialog.continue")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 切换 CodeBuddy CLI 确认（桌面 App 不支持 window.confirm） */}
      <Dialog open={cliSwitchTarget !== null} onOpenChange={(open) => !open && setCliSwitchTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("accounts.dialog.cliSwitchTitle")}</DialogTitle>
            <DialogDescription>
              {t("accounts.dialog.cliSwitchDescription", { account: cliSwitchAccountLabel })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCliSwitchTarget(null)}>
              {t("common.cancel")}
            </Button>
            <Button onClick={() => void confirmSwitchCodebuddyCli()}>
              {t("accounts.dialog.cliSwitchConfirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 删除账号确认 */}
      <Dialog open={deleteTarget !== null} onOpenChange={(o) => !o && setDeleteTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("accounts.card.deleteAccount")}</DialogTitle>
            <DialogDescription>
              {t("accounts.dialog.deleteDescription", {
                account: deleteTarget ? displayName(deleteTarget) : "",
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              {t("common.cancel")}
            </Button>
            <Button variant="destructive" onClick={() => void confirmDelete()}>
              {t("common.delete")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
