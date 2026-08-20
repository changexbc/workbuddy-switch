import { useEffect, useState } from "react";
import { BrowserRouter, Navigate, NavLink, Outlet, Route, Routes } from "react-router-dom";
import { ArrowUpCircle, Loader2, RefreshCw, Settings } from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import * as api from "@/lib/api";
import type { UpdateInfo } from "@/lib/types";
import { Separator } from "@/components/ui/separator";
import AccountsPage from "@/pages/AccountsPage";
import SettingsPage from "@/pages/SettingsPage";
import { UpdateInstallDialog } from "@/components/update-install-dialog";
import { useCreditAutoRefresh } from "@/lib/use-credit-auto-refresh";
import { useAccountsStore } from "@/stores/accounts";

function UpdateCenter({ running }: { running: boolean | undefined }) {
  const version = useAccountsStore((s) => s.status?.version);
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [checking, setChecking] = useState(false);
  const [dialogOpen, setDialogOpen] = useState(false);

  useEffect(() => {
    let disposed = false;

    async function checkForUpdate() {
      setChecking(true);
      try {
        const result = await api.checkUpdate();
        if (!disposed) setInfo(result.ok ? result : null);
      } catch {
        // 左下角只展示可操作的升级状态，网络错误不打扰正常使用。
      } finally {
        if (!disposed) setChecking(false);
      }
    }

    void checkForUpdate();
    const timer = window.setInterval(() => void checkForUpdate(), 30 * 60 * 1000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, []);

  const hasUpdate = Boolean(info?.ok && info.hasUpdate && info.latest);

  return (
    <>
      <div className="border-t px-3 py-3 text-xs text-muted-foreground">
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0">
            <div>WorkBuddy：{running ? "运行中" : "未运行"}</div>
            <div className="mt-1 font-mono text-[11px]">v{version || "?"}</div>
          </div>
          {hasUpdate ? (
            <Button
              variant="outline"
              size="sm"
              className="h-7 shrink-0 px-2 text-xs"
              onClick={() => setDialogOpen(true)}
            >
              <ArrowUpCircle />
              升级
            </Button>
          ) : checking ? (
            <Loader2 className="mt-0.5 size-4 animate-spin" />
          ) : null}
        </div>
        {hasUpdate && (
          <button
            type="button"
            className="mt-2 flex w-full items-center gap-1.5 rounded-md bg-primary/10 px-2 py-1.5 text-left text-primary transition-colors hover:bg-primary/15"
            onClick={() => setDialogOpen(true)}
          >
            <ArrowUpCircle className="size-3.5" />
            发现新版本 v{info?.latest}
          </button>
        )}
      </div>
      <UpdateInstallDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        update={info}
      />
    </>
  );
}

function Layout() {
  const running = useAccountsStore((s) => s.status?.running);
  useCreditAutoRefresh();

  return (
    <div className="flex h-screen overflow-hidden">
      <aside className="flex w-52 shrink-0 flex-col border-r bg-sidebar">
        <div className="flex items-center gap-2 px-4 pt-5 pb-4">
          <span className="flex size-8 items-center justify-center rounded-lg bg-primary text-primary-foreground">
            <RefreshCw className="size-4" />
          </span>
          <div className="leading-tight">
            <div className="text-sm font-semibold">workbuddy-switch</div>
            <div className="text-xs text-muted-foreground">WorkBuddy / CodeBuddy 账号</div>
          </div>
        </div>
        <Separator />
        <nav className="flex flex-1 flex-col gap-1 px-2 py-3">
          <NavLink
            to="/"
            className={({ isActive }) =>
              cn(
                "rounded-md px-3 py-2 text-sm transition-colors",
                isActive
                  ? "bg-accent font-medium text-accent-foreground"
                  : "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
              )
            }
          >
            WorkBuddy / CodeBuddy
          </NavLink>
          <NavLink
            to="/settings"
            className={({ isActive }) =>
              cn(
                "flex items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors",
                isActive
                  ? "bg-accent font-medium text-accent-foreground"
                  : "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
              )
            }
          >
            <Settings className="size-4" />
            设置
          </NavLink>
        </nav>
        {api.isWebui() ? null : <UpdateCenter running={running} />}
      </aside>
      <main className="flex-1 overflow-y-auto">
        <Outlet />
      </main>
    </div>
  );
}

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route element={<Layout />}>
          <Route path="/" element={<AccountsPage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
