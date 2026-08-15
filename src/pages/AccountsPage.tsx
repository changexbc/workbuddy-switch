import { useEffect, useState } from "react";
import { Download, Loader2, QrCode, Plus } from "lucide-react";

import { AccountCard } from "@/components/account-card";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { ManualAddDialog } from "@/components/manual-add-dialog";
import { OAuthLoginDialog } from "@/components/oauth-login-dialog";
import { SwitchAccountDialog } from "@/components/switch-account-dialog";
import * as api from "@/lib/api";
import type { AccountMeta } from "@/lib/types";
import { useAccountsStore } from "@/stores/accounts";

export default function AccountsPage() {
  const { accounts, status, loading, error, fetchAll, deleteAccount, importLocal } =
    useAccountsStore();
  const [oauthOpen, setOauthOpen] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const [switchAccount, setSwitchAccount] = useState<AccountMeta | null>(null);
  const [importing, setImporting] = useState(false);
  const [notice, setNotice] = useState<{ type: "ok" | "err"; text: string } | null>(null);
  /** 账号 id -> 今日是否已签到（undefined=查询中/未知） */
  const [checkinMap, setCheckinMap] = useState<Record<string, boolean>>({});

  useEffect(() => {
    void fetchAll();
  }, [fetchAll]);

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
    const label = a.nickname || a.email || a.id;
    if (!window.confirm(`确定删除账号「${label}」？`)) return;
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

  const current = status?.current;

  return (
    <div className="mx-auto max-w-4xl px-6 py-6">
      <header className="mb-6">
        <h1 className="text-xl font-semibold">账号切换</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          {current
            ? `当前登录：${current.nickname || current.email || current.uid || "未知"}`
            : "当前未检测到 WorkBuddy 登录账号"}
          {status?.authFile && ` · ${status.authFile}`}
        </p>
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
          {accounts.map((a) => (
            <AccountCard
              key={a.id}
              account={a}
              onDelete={onDelete}
              onSwitch={setSwitchAccount}
              onCheckin={onCheckin}
              onRefresh={onRefresh}
              todayCheckedIn={checkinMap[a.id]}
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
        onDone={() => void fetchAll()}
      />
    </div>
  );
}
