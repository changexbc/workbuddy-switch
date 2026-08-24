import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import {
  CalendarCheck,
  Download,
  FileDown,
  FileUp,
  Loader2,
  QrCode,
  RefreshCw,
  Terminal,
} from "lucide-react";

import { AccountCard } from "@/components/account-card";
import { CodeBuddyMark, StatusDot, WorkBuddyMark } from "@/components/product-marks";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
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
import * as api from "@/lib/api";
import type { AccountMeta, AppStatus, CheckinConfig, CodeBuddyCliStatus, CreditExpiry } from "@/lib/types";
import { useAccountsStore } from "@/stores/accounts";

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

function formatCredits(value: number): string {
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(value);
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

export default function AccountsPage() {
  const {
    accounts,
    status,
    loading,
    error,
    fetchAll,
    deleteAccount,
    importLocal,
    creditMap,
    creditLoadingMap,
    refreshingCredits,
    ensureCredits,
    refreshCredits,
  } = useAccountsStore();
  const [oauthOpen, setOauthOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [switchAccount, setSwitchAccount] = useState<AccountMeta | null>(null);
  const [importing, setImporting] = useState(false);
  const [notice, setNotice] = useState<{ type: "ok" | "err"; text: string } | null>(null);
  const [autoCheckinConfig, setAutoCheckinConfig] = useState<CheckinConfig | null>(null);
  const [autoCheckinSaving, setAutoCheckinSaving] = useState(false);
  /** 账号 id -> 今日是否已签到（undefined=查询中/未知） */
  const [checkinMap, setCheckinMap] = useState<Record<string, boolean>>({});
  const [refreshingCheckin, setRefreshingCheckin] = useState(false);
  const [codebuddyCli, setCodebuddyCli] = useState<CodeBuddyCliStatus | null>(null);
  const [codebuddyCliSwitchingId, setCodebuddyCliSwitchingId] = useState<string | null>(null);
  const [installingCodebuddyCli, setInstallingCodebuddyCli] = useState(false);
  /** 接入/升级 CLI helper 确认框 */
  const [installConfirmOpen, setInstallConfirmOpen] = useState(false);
  /** 删除账号确认目标（null=关闭） */
  const [deleteTarget, setDeleteTarget] = useState<AccountMeta | null>(null);

  useEffect(() => {
    void fetchAll();
  }, [fetchAll]);

  useEffect(() => {
    let cancelled = false;
    void api
      .getAutoCheckinConfig()
      .then((config) => {
        if (!cancelled) setAutoCheckinConfig(config);
      })
      .catch((e) => {
        if (!cancelled) {
          setNotice({ type: "err", text: `自动签到配置加载失败：${api.asError(e)}` });
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  /** 首次启动自动导入本机账号（本会话只尝试一次，无本机账号时静默） */
  const autoImportTried = useRef(false);
  useEffect(() => {
    if (autoImportTried.current || loading || accounts.length > 0) return;
    autoImportTried.current = true;
    void importLocal()
      .then(() => void fetchAll())
      .catch(() => {
        /* 本机无 WorkBuddy 登录态时静默，不打扰用户 */
      });
  }, [accounts.length, loading, importLocal, fetchAll]);

  async function refreshCodebuddyCliStatus() {
    try {
      setCodebuddyCli(await api.getCodebuddyCliStatus());
    } catch {
      setCodebuddyCli(null);
    }
  }

  useEffect(() => {
    void refreshCodebuddyCliStatus();
  }, [accounts.length]);

  // 账号列表变化后并行查询各账号今日签到状态
  useEffect(() => {
    if (!accounts.length) return;
    let cancelled = false;
    void fetchTodayCheckinMap(
      accounts.map((account) => account.id),
      () => cancelled,
    ).then((next) => {
      if (!cancelled && Object.keys(next).length > 0) {
        setCheckinMap((prev) => ({ ...prev, ...next }));
      }
    });
    return () => {
      cancelled = true;
    };
  }, [accounts]);

  // 只给尚未缓存的账号拉积分；切回首页不重复请求。点「刷新积分」才强制更新。
  useEffect(() => {
    if (!accounts.length) return;
    void ensureCredits(accounts.map((account) => account.id));
  }, [accounts, ensureCredits]);

  async function onImport() {
    setImporting(true);
    setNotice(null);
    try {
      const acc = await importLocal();
      setNotice({ type: "ok", text: `已导入：${acc.nickname || acc.email || acc.id}` });
    } catch (e) {
      setNotice({ type: "err", text: api.asError(e) });
    } finally {
      setImporting(false);
    }
  }

  async function onAutoCheckinChange(enabled: boolean) {
    if (!autoCheckinConfig || autoCheckinSaving) return;
    const previous = autoCheckinConfig;
    const next = { ...previous, enabled };
    setAutoCheckinConfig(next);
    setAutoCheckinSaving(true);
    setNotice(null);
    try {
      setAutoCheckinConfig(await api.saveAutoCheckinConfig(next));
    } catch (e) {
      setAutoCheckinConfig(previous);
      setNotice({ type: "err", text: `自动签到设置保存失败：${api.asError(e)}` });
    } finally {
      setAutoCheckinSaving(false);
    }
  }

  /** 导出完成提示（含安全提醒）。 */
  function onExported(count: number) {
    const text = `已导出 ${count} 个账号。文件含登录 token，等同密码，请勿上传网盘或发送给他人。`;
    toast.success("导出成功", { description: text });
    setNotice({ type: "ok", text });
  }

  /** 导入完成提示：计数 + token 可能过期提醒，并刷新列表。 */
  function onImported(result: { imported: number; skipped: number; overwritten: number }) {
    void fetchAll();
    const overwriteText = result.overwritten > 0 ? `（覆盖 ${result.overwritten} 个）` : "";
    const text = `已导入 ${result.imported} 个${overwriteText}，跳过 ${result.skipped} 个。token 可能已过期，切换后可能需要重新登录。`;
    toast.success("导入成功", { description: text });
    setNotice({ type: "ok", text });
  }

  async function onDelete(a: AccountMeta) {
    // 桌面 App（Tauri WebView）不支持 window.confirm，改用 Dialog 确认
    setDeleteTarget(a);
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    const a = deleteTarget;
    setDeleteTarget(null);
    setNotice(null);
    try {
      await deleteAccount(a.id);
      setNotice({ type: "ok", text: "已删除" });
    } catch (e) {
      setNotice({ type: "err", text: api.asError(e) });
    }
  }

  async function onCheckin(a: AccountMeta) {
    setNotice(null);
    try {
      const res = await api.checkin(a.id);
      const label =
        res.result === "success"
          ? "签到成功"
          : res.result === "already"
            ? "今天已签到"
            : "签到失败";
      setNotice({
        type: res.result === "error" ? "err" : "ok",
        text: `${a.nickname || a.email || a.id}：${label}${res.error ? `（${res.error}）` : ""}`,
      });
      // 刷新该账号的今日签到状态
      try {
        const st = await api.getCheckinStatus(a.id);
        if (st.ok) setCheckinMap((prev) => ({ ...prev, [a.id]: st.todayCheckedIn }));
      } catch {
        /* ignore */
      }
      void fetchAll();
    } catch (e) {
      setNotice({ type: "err", text: api.asError(e) });
    }
  }

  async function onRefresh(a: AccountMeta) {
    setNotice(null);
    try {
      const res = await api.refreshAccountToken(a.id);
      const label = a.nickname || a.email || a.id;
      if (res.needsRelogin) {
        setNotice({
          type: "err",
          text: `${label}：刷新失败，需重新登录${res.needsReloginReason ? `（${res.needsReloginReason}）` : ""}`,
        });
      } else {
        setNotice({ type: "ok", text: `${label}：token 已刷新` });
      }
      void fetchAll();
    } catch (e) {
      setNotice({ type: "err", text: api.asError(e) });
    }
  }

  async function onRefreshCredits() {
    if (!accounts.length || refreshingCredits) return;
    setNotice(null);
    await refreshCredits(accounts.map((account) => account.id));
    setNotice({ type: "ok", text: "积分到期情况已刷新" });
  }

  async function onRefreshCheckin() {
    if (!accounts.length || refreshingCheckin) return;
    setRefreshingCheckin(true);
    setNotice(null);
    try {
      const next = await fetchTodayCheckinMap(accounts.map((account) => account.id));
      setCheckinMap((prev) => ({ ...prev, ...next }));
      setNotice({ type: "ok", text: "签到状态已刷新" });
    } catch (e) {
      setNotice({ type: "err", text: api.asError(e) });
    } finally {
      setRefreshingCheckin(false);
    }
  }

  async function onSwitchCodebuddyCli(account: AccountMeta) {
    setCodebuddyCliSwitchingId(account.id);
    setNotice(null);
    try {
      const result = await api.switchCodebuddyCliAccount(account.id);
      setNotice({
        type: "ok",
        text: `${account.nickname || account.email || account.id}：${result.message || "CodeBuddy CLI 已切换"}`,
      });
      await refreshCodebuddyCliStatus();
    } catch (error) {
      setNotice({ type: "err", text: api.asError(error) });
    } finally {
      setCodebuddyCliSwitchingId(null);
    }
  }

  async function onInstallCodebuddyCli() {
    // 桌面 App（Tauri WebView）不支持 window.confirm，改用 Dialog 确认
    setInstallConfirmOpen(true);
  }

  async function confirmInstallCodebuddyCli() {
    setInstallConfirmOpen(false);
    setInstallingCodebuddyCli(true);
    setNotice(null);
    try {
      const result = await api.installCodebuddyCliHelper();
      setNotice({ type: "ok", text: result.message || "CodeBuddy CLI helper 已更新" });
      await refreshCodebuddyCliStatus();
    } catch (error) {
      setNotice({ type: "err", text: api.asError(error) });
    } finally {
      setInstallingCodebuddyCli(false);
    }
  }

  const current = status?.current;
  const creditOrderingReady =
    accounts.length > 0 &&
    accounts.every((account) => Boolean(creditMap[account.id]) && !creditLoadingMap[account.id]);
  const orderedAccounts = creditOrderingReady
    ? accounts
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
    : accounts;
  const urgentCreditAccounts = orderedAccounts.filter((account) => {
    const credit = creditMap[account.id];
    return credit?.ok && (credit.expired || credit.expiringSoon);
  });
  const priorityAccountId =
    creditOrderingReady
      ? orderedAccounts.find((account) => hasExpiringSoonCredits(creditMap[account.id]))?.id
      : undefined;
  const cliCurrentAccountId = codebuddyCli?.activeAccountId;
  const workbuddyCurrentName = current
    ? current.nickname || current.email || current.uid || "未知账号"
    : "未登录";
  const codebuddyCurrentName = codebuddyCli?.configured
    ? codebuddyCli.activeAccountName || "未检测到"
    : "尚未接入";
  const toolbarBtn = "h-10 rounded-lg px-4";

  return (
    <div className="mx-auto w-full max-w-[1080px] px-6 py-8 sm:px-8 sm:py-9">
      <header className="mb-6">
        <h1 className="text-[28px] font-semibold tracking-tight">账号管理</h1>
        <p className="mt-2 text-sm leading-6 text-muted-foreground">
          统一管理 WorkBuddy 与 CodeBuddy CLI 账号、积分和签到状态。
        </p>
        <div className="mt-5 flex items-center gap-3">
          <label htmlFor="accounts-auto-checkin" className="text-sm">
            自动签到
          </label>
          <Switch
            id="accounts-auto-checkin"
            checked={autoCheckinConfig?.enabled ?? false}
            disabled={!autoCheckinConfig || autoCheckinSaving}
            onCheckedChange={(enabled) => void onAutoCheckinChange(enabled)}
            aria-label="自动签到"
          />
          {autoCheckinSaving && <Loader2 className="size-3.5 animate-spin text-muted-foreground" />}
        </div>
        <Card className="mt-6 min-w-0 gap-0 overflow-hidden rounded-[12px] py-0 shadow-none">
          <CardContent className="min-w-0 p-0">
            <div className="flex min-w-0 items-center justify-between gap-3 border-b px-5 py-4">
              <div className="flex min-w-0 items-center gap-3">
                <WorkBuddyMark size={36} />
                <div className="min-w-0">
                  <div className="text-sm font-semibold">WorkBuddy</div>
                  <div className="mt-0.5 truncate text-sm text-muted-foreground">
                    当前账号：{workbuddyCurrentName}
                  </div>
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-1.5 text-sm text-muted-foreground">
                <StatusDot on={Boolean(status?.running)} />
                {status?.running ? "运行中" : "未运行"}
              </div>
            </div>
            <div className="flex min-w-0 items-center justify-between gap-3 px-5 py-4">
              <div className="flex min-w-0 items-center gap-3">
                <CodeBuddyMark size={36} />
                <div className="min-w-0">
                  <div className="text-sm font-semibold">CodeBuddy CLI</div>
                  <div className="mt-0.5 truncate text-sm text-muted-foreground">
                    当前账号：{codebuddyCurrentName}
                  </div>
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-1.5 text-sm text-muted-foreground">
                <StatusDot on={Boolean(codebuddyCli?.configured)} />
                {codebuddyCli?.configured ? "已接入" : "未接入"}
              </div>
            </div>
          </CardContent>
        </Card>
      </header>

      <div className="mb-6 flex flex-wrap items-center gap-2.5">
        <Button className={toolbarBtn} onClick={() => setOauthOpen(true)}>
          <QrCode />
          OAuth 扫码登录
        </Button>
        <Button className={toolbarBtn} onClick={onImport} disabled={importing} variant="outline">
          {importing ? <Loader2 className="animate-spin" /> : <Download />}
          导入本机账号
        </Button>
        <Button
          className={toolbarBtn}
          onClick={() => setExportOpen(true)}
          disabled={accounts.length === 0}
          variant="outline"
        >
          <FileDown />
          导出账号
        </Button>
        <Button className={toolbarBtn} onClick={() => setImportOpen(true)} variant="outline">
          <FileUp />
          导入账号
        </Button>
        <Button
          className={toolbarBtn}
          onClick={onRefreshCredits}
          disabled={refreshingCredits || accounts.length === 0}
          variant="outline"
        >
          <RefreshCw className={refreshingCredits ? "animate-spin" : undefined} />
          刷新积分
        </Button>
        <Button
          className={toolbarBtn}
          onClick={() => void onRefreshCheckin()}
          disabled={refreshingCheckin || accounts.length === 0}
          variant="outline"
        >
          {refreshingCheckin ? <Loader2 className="animate-spin" /> : <CalendarCheck />}
          刷新签到状态
        </Button>
      </div>

      {notice && (
        <Alert variant={notice.type === "err" ? "destructive" : "default"} className="mb-4">
          <AlertTitle>{notice.type === "err" ? "操作失败" : "成功"}</AlertTitle>
          <AlertDescription>{notice.text}</AlertDescription>
        </Alert>
      )}
      {error && (
        <Alert variant="destructive" className="mb-4">
          <AlertTitle>加载失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
      {codebuddyCli && (!codebuddyCli.configured || !codebuddyCli.helperSupportsAccountIds) && (
        <Alert className="mb-4">
          <Terminal />
          <AlertTitle>CodeBuddy CLI 接入</AlertTitle>
          <AlertDescription>
            <p>
              {codebuddyCli.configured
                ? "当前 helper 仍按旧索引读取账号；升级后将按账号 ID 独立切换，账号增删也不会错位。"
                : "WorkBuddy 账号与积分功能可正常使用；如需从这里切换 CodeBuddy CLI 账号，点击下方按钮一键接入（自动完成配置，无需手动操作）。"}
            </p>
            <Button
              className="mt-2"
              size="sm"
              variant="outline"
              onClick={() => void onInstallCodebuddyCli()}
              disabled={installingCodebuddyCli}
            >
              {installingCodebuddyCli && <Loader2 className="animate-spin" />}
              {codebuddyCli.configured ? "升级 CLI helper" : "接入 CLI"}
            </Button>
          </AlertDescription>
        </Alert>
      )}
      {urgentCreditAccounts.length > 0 && (
        <Alert className="mb-4 border-amber-300 bg-amber-50 text-amber-950">
          <AlertTitle>积分即将到期</AlertTitle>
          <AlertDescription>
            以下账号有积分将在 7 天内到期，建议尽快使用：{" "}
            {urgentCreditAccounts.map((account) => {
              const credit = creditMap[account.id];
              const amount = expiringSoonAmount(credit);
              const name = account.nickname || account.email || account.uid || account.id;
              return amount > 0 ? `${name}（${formatCredits(amount)} 积分）` : `${name}（有已到期资源）`;
            }).join("、")}
            。具体资源和时间已标注在账号卡片上。
          </AlertDescription>
        </Alert>
      )}

      <section className="mt-7 min-w-0" aria-labelledby="accounts-list-title">
        <h2 id="accounts-list-title" className="mb-3 text-base font-semibold">
          账号
        </h2>
        {loading && accounts.length === 0 ? (
          <div className="flex items-center gap-2 py-16 text-sm text-muted-foreground">
            <Loader2 className="animate-spin" />
            加载账号…
          </div>
        ) : accounts.length === 0 ? (
          <div className="rounded-xl border border-dashed px-4 py-16 text-center text-sm text-muted-foreground">
            暂无账号。点击上方按钮导入本机账号或扫码登录。
          </div>
        ) : (
          <Card className="min-w-0 gap-0 overflow-hidden rounded-[12px] py-0 shadow-none">
            {orderedAccounts.map((a) => (
              <AccountCard
                key={a.id}
                account={a}
                onDelete={onDelete}
                onSwitch={setSwitchAccount}
                onCheckin={onCheckin}
                onRefresh={onRefresh}
                todayCheckedIn={checkinMap[a.id]}
                credit={creditMap[a.id]}
                creditLoading={creditLoadingMap[a.id]}
                creditPriority={a.id === priorityAccountId}
                workbuddyActive={isWorkbuddyCurrent(a, current)}
                codebuddyCliConfigured={codebuddyCli?.configured}
                codebuddyCliActive={a.id === cliCurrentAccountId}
                onSwitchCodebuddyCli={onSwitchCodebuddyCli}
                codebuddyCliLoading={codebuddyCliSwitchingId === a.id}
                featuresDisabled={false}
              />
            ))}
          </Card>
        )}
      </section>

      <OAuthLoginDialog open={oauthOpen} onOpenChange={setOauthOpen} />
      <ExportAccountsDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        accounts={accounts}
        onExported={onExported}
      />
      <ImportAccountsDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        onImported={onImported}
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
        }}
      />

      {/* 接入/升级 CLI helper 确认（桌面 App 不支持 window.confirm） */}
      <Dialog open={installConfirmOpen} onOpenChange={setInstallConfirmOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{codebuddyCli?.configured ? "升级 CodeBuddy CLI helper" : "接入 CodeBuddy CLI"}</DialogTitle>
            <DialogDescription>
              {codebuddyCli?.configured ? "升级" : "接入"}会自动写入
              <code className="mx-1 rounded bg-muted px-1">~/.codebuddy-rotate/helper.cjs</code>
              并更新
              <code className="mx-1 rounded bg-muted px-1">~/.codebuddy/settings.json</code>
              的 apiKeyHelper 配置，是否继续？
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
