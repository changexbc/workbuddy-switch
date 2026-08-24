import { useEffect, useState } from "react";
import { BrowserRouter, Navigate, NavLink, Outlet, Route, Routes } from "react-router-dom";
import { Toaster } from "sonner";
import { ArrowUpCircle, ChartBar, Loader2, Settings, User } from "lucide-react";

import { cn } from "@/lib/utils";
import * as api from "@/lib/api";
import type { UpdateInfo } from "@/lib/types";
import AccountsPage from "@/pages/AccountsPage";
import CreditStatsPage from "@/pages/CreditStatsPage";
import SettingsPage from "@/pages/SettingsPage";
import { StatusDot, WorkBuddyMark } from "@/components/product-marks";
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
      <section className="mt-auto border-t border-sidebar-border px-2 pt-3 text-xs">
        <div className="flex items-center gap-1.5 text-[13px] text-sidebar-foreground">
          <StatusDot on={Boolean(running)} />
          <span>WorkBuddy：{running ? "运行中" : "未运行"}</span>
          {checking && <Loader2 className="size-3 animate-spin text-sidebar-foreground/40" aria-label="检查更新中" />}
        </div>
        <div className="mt-1 text-sidebar-foreground/50">v{version || "?"}</div>
        {hasUpdate && (
          <button
            type="button"
            className="mt-3 flex w-full cursor-pointer items-center gap-1.5 rounded-md py-1 text-left text-[13px] text-sidebar-foreground/70 transition-colors hover:text-sidebar-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring/50"
            onClick={() => setDialogOpen(true)}
          >
            <ArrowUpCircle className="size-3.5" />
            发现新版本 v{info?.latest}
          </button>
        )}
      </section>
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
    <div className="flex h-screen min-h-0 overflow-hidden bg-background">
      <aside className="flex min-h-0 w-[220px] shrink-0 flex-col border-r border-sidebar-border bg-sidebar px-3 py-4">
        <div className="flex items-center gap-2.5 px-1 pb-6">
          <WorkBuddyMark size={32} />
          <div className="min-w-0 truncate text-[13px] font-semibold tracking-tight">workbuddy-switch</div>
        </div>
        <nav className="flex min-h-0 flex-1 flex-col gap-0.5" aria-label="主导航">
          <NavLink
            to="/"
            end
            className={({ isActive }) =>
              cn(
                "flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring/50",
                isActive
                  ? "bg-foreground/[0.06] font-medium text-foreground"
                  : "text-muted-foreground hover:bg-foreground/[0.04] hover:text-foreground",
              )
            }
          >
            <User className="size-4" />
            账号管理
          </NavLink>
          <NavLink
            to="/credit-stats"
            className={({ isActive }) =>
              cn(
                "flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring/50",
                isActive
                  ? "bg-foreground/[0.06] font-medium text-foreground"
                  : "text-muted-foreground hover:bg-foreground/[0.04] hover:text-foreground",
              )
            }
          >
            <ChartBar className="size-4" />
            积分统计
          </NavLink>
          <NavLink
            to="/settings"
            className={({ isActive }) =>
              cn(
                "flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring/50",
                isActive
                  ? "bg-foreground/[0.06] font-medium text-foreground"
                  : "text-muted-foreground hover:bg-foreground/[0.04] hover:text-foreground",
              )
            }
          >
            <Settings className="size-4" />
            设置
          </NavLink>
        </nav>
        {api.isWebui() ? null : <UpdateCenter running={running} />}
      </aside>
      <main className="min-w-0 flex-1 overflow-y-auto bg-background overscroll-contain">
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
          <Route path="/credit-stats" element={<CreditStatsPage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
      <Toaster richColors position="bottom-right" />
    </BrowserRouter>
  );
}
