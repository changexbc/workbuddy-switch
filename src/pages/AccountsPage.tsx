import { useEffect, useRef, useState } from "react";
import { AppWindow, Download, Loader2, Plus, QrCode, RefreshCw, Terminal } from "lucide-react";

import { AccountCard } from "@/components/account-card";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ManualAddDialog } from "@/components/manual-add-dialog";
import { OAuthLoginDialog } from "@/components/oauth-login-dialog";
import { SwitchAccountDialog } from "@/components/switch-account-dialog";
import * as api from "@/lib/api";
import type { AccountMeta, AppStatus, CodeBuddyCliStatus, CreditExpiry } from "@/lib/types";
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
  const [manualOpen, setManualOpen] = useState(false);
  const [switchAccount, setSwitchAccount] = useState<AccountMeta | null>(null);
  const [importing, setImporting] = useState(false);
  const [notice, setNotice] = useState<{ type: "ok" | "err"; text: string } | null>(null);
  /** 账号 id -> 今日是否已签到（undefined=查询中/未知） */
  const [checkinMap, setCheckinMap] = useState<Record<string, boolean>>({});
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
    Promise.all(
      accounts.map(async (a) => {
        try {
          const res = await api.getCheckinStatus(a.id);
          if (!cancelled && res.ok) {
            setCheckinMap((prev) => ({ ...prev, [a.id]: res.todayCheckedIn }));
          }
        } catch {
          /* 查询失败忽略，保持未知 */
        }
      }),
    );
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
    : "未检测到当前登录账号";
  const codebuddyCurrentName = codebuddyCli?.configured
    ? codebuddyCli.activeAccountName || "已接入，未检测到当前账号"
    : "尚未接入 CodeBuddy CLI";

  return (
    <div className="mx-auto max-w-4xl px-6 py-6">
      <header className="mb-6">
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-xl font-semibold">账号管理</h1>
          {codebuddyCli?.configured && <Badge variant="success">CodeBuddy CLI 已接入</Badge>}
        </div>
        <p className="mt-1 text-sm text-muted-foreground">
          WorkBuddy 与 CodeBuddy CLI 共用同一份账号库和积分；两侧当前账号互不影响，可分别切换。
        </p>
        <div className="mt-4 grid gap-3 md:grid-cols-2">
          <div className="rounded-xl border bg-card p-4">
            <div className="flex items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <span className="flex size-7 items-center justify-center rounded-md bg-sky-100 text-sky-700">
                  <AppWindow className="size-4" />
                </span>
                <span className="text-sm font-semibold">WorkBuddy</span>
              </div>
              <Badge variant={current ? "success" : "outline"}>{current ? "已登录" : "未登录"}</Badge>
            </div>
            <div className="mt-3 truncate text-sm font-medium">{workbuddyCurrentName}</div>
            <div className="mt-1 truncate text-xs text-muted-foreground">
              {status?.authFile ? `认证文件：${status.authFile}` : "通过本应用切换账号"}
            </div>
          </div>
          <div className="rounded-xl border bg-card p-4">
            <div className="flex items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <span className="flex size-7 items-center justify-center rounded-md bg-violet-100 text-violet-700">
                  <Terminal className="size-4" />
                </span>
                <span className="text-sm font-semibold">CodeBuddy CLI</span>
              </div>
              <Badge variant={codebuddyCli?.configured ? "success" : "outline"}>
                {codebuddyCli?.configured ? "已接入" : "未接入"}
              </Badge>
            </div>
            <div className="mt-3 truncate text-sm font-medium">{codebuddyCurrentName}</div>
            <div className="mt-1 text-xs text-muted-foreground">
              {codebuddyCli?.configured
                ? "在账号卡片上点击「切换 CodeBuddy CLI」生效"
                : "接入 apiKeyHelper 后即可从本应用切换"}
            </div>
          </div>
        </div>
      </header>

      <div className="mb-5 flex flex-wrap items-center gap-2">
        <Button onClick={onImport} disabled={importing} variant="outline">
          {importing ? <Loader2 className="animate-spin" /> : <Download />}
          导入本机账号
        </Button>
        <Button onClick={() => setOauthOpen(true)}>
          <QrCode />
          OAuth 扫码登录
        </Button>
        <Button onClick={() => setManualOpen(true)} variant="outline">
          <Plus />
          手动添加
        </Button>
        <Button onClick={onRefreshCredits} disabled={refreshingCredits || accounts.length === 0} variant="outline">
          <RefreshCw className={refreshingCredits ? "animate-spin" : undefined} />
          刷新积分
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

      {loading && accounts.length === 0 ? (
        <div className="flex items-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="animate-spin" />
          加载账号…
        </div>
      ) : accounts.length === 0 ? (
        <div className="rounded-lg border border-dashed py-16 text-center text-sm text-muted-foreground">
          暂无账号。点击上方按钮导入本机账号或扫码登录。
        </div>
      ) : (
        <div className="space-y-3">
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
        </div>
      )}

      <OAuthLoginDialog open={oauthOpen} onOpenChange={setOauthOpen} />
      <ManualAddDialog open={manualOpen} onOpenChange={setManualOpen} />
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
