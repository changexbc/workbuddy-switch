import { useEffect, useState } from "react";
import { Download, ExternalLink, Loader2, RefreshCw } from "lucide-react";

import { DemoAction } from "@/components/demo-action";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import * as api from "@/lib/api";
import { useT } from "@/lib/i18n";
import { GITHUB_RELEASE_URL, openReleaseUrl } from "@/lib/update";
import { useUpdateState } from "@/lib/use-update-state";

interface UpdateInstallDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/**
 * 统一更新弹窗：只展示 Rust 更新状态机（与托盘菜单同源）的阶段与进度。
 *
 * 阶段与进度来自 `update-state` 订阅，不再自己调用 `@tauri-apps/plugin-updater`；
 * 下载 / 重启都只是把动作交给后端（`updateDownload` / `updateRestart`），状态由后端
 * 持有——关掉弹窗再打开，看到的仍是同一阶段与进度。
 */
export function UpdateInstallDialog({ open, onOpenChange }: UpdateInstallDialogProps) {
  const t = useT();
  const snapshot = useUpdateState();
  const [restarting, setRestarting] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const phase = snapshot.phase;
  const targetVersion = snapshot.latest ?? t("dialogs.update.newVersion");
  const percent = snapshot.percent;
  const busy = phase === "checking" || phase === "downloading";
  const error = actionError ?? snapshot.message ?? t("dialogs.update.unknownError");

  useEffect(() => {
    if (!open) return;
    // 每次打开重置本地动作态：阶段与错误文案都由快照承载。
    setRestarting(false);
    setActionError(null);
  }, [open]);

  async function startDownload() {
    setActionError(null);
    try {
      await api.updateDownload();
    } catch (e) {
      setActionError(t("dialogs.update.downloadFailed", { error: api.asError(e) }));
    }
  }

  /** 与托盘一致：已知目标版本则重试下载，否则重试检查。 */
  async function retry() {
    if (snapshot.latest) {
      await startDownload();
      return;
    }
    setActionError(null);
    try {
      const result = await api.checkUpdate(undefined, true);
      if (!result.ok) {
        setActionError(result.message || result.error || t("dialogs.update.checkFailed"));
      }
    } catch (e) {
      setActionError(t("dialogs.update.checkFailedWithError", { error: api.asError(e) }));
    }
  }

  async function restartApp() {
    setRestarting(true);
    setActionError(null);
    try {
      await api.updateRestart();
    } catch (e) {
      setRestarting(false);
      setActionError(t("dialogs.update.restartFailed", { error: api.asError(e) }));
    }
  }

  async function openRelease() {
    try {
      await openReleaseUrl(GITHUB_RELEASE_URL);
    } catch (e) {
      setActionError(api.asError(e));
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && (busy || restarting)) return;
        onOpenChange(next);
      }}
    >
      <DialogContent showCloseButton={!busy && !restarting}>
        <DialogHeader>
          <DialogTitle>
            {phase === "checking" && t("dialogs.update.titleChecking")}
            {phase === "downloading" &&
              t("dialogs.update.titleDownloading", { version: targetVersion })}
            {(phase === "idle" || phase === "upToDate") && t("dialogs.update.titleUpToDate")}
            {phase === "available" && t("dialogs.update.titleAvailable")}
            {phase === "readyToRestart" && t("dialogs.update.titleReady")}
            {phase === "error" && t("dialogs.update.titleError")}
          </DialogTitle>
          <DialogDescription>
            {phase === "checking" && t("dialogs.update.descriptionChecking")}
            {phase === "downloading" && t("dialogs.update.descriptionDownloading")}
            {(phase === "idle" || phase === "upToDate") && t("dialogs.update.descriptionUpToDate")}
            {phase === "available" &&
              t("dialogs.update.descriptionAvailable", { version: targetVersion })}
            {phase === "readyToRestart" && t("dialogs.update.descriptionReady")}
            {phase === "error" && t("dialogs.update.descriptionError")}
          </DialogDescription>
        </DialogHeader>

        {phase === "checking" && (
          <div className="flex items-center gap-2 rounded-md bg-muted/50 p-3 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" />
            {t("dialogs.update.connecting")}
          </div>
        )}

        {phase === "available" && (
          <div className="rounded-md border p-3 text-sm">
            {t("dialogs.update.availableHint", { version: targetVersion })}
          </div>
        )}

        {phase === "downloading" && (
          <div className="space-y-2 rounded-md border p-3">
            <div className="flex items-center justify-between text-sm">
              <span className="flex items-center gap-2">
                <Loader2 className="size-4 animate-spin text-primary" />
                {t("dialogs.update.progressLabel")}
              </span>
              <span className="font-mono text-xs text-muted-foreground">
                {percent === null ? t("dialogs.update.downloading") : `${percent}%`}
              </span>
            </div>
            <div className="h-2 overflow-hidden rounded-full bg-muted">
              <div
                className="h-full rounded-full bg-primary transition-[width] duration-200"
                style={{ width: `${percent ?? 30}%` }}
              />
            </div>
          </div>
        )}

        {phase === "readyToRestart" && (
          <div className="rounded-md border border-emerald-500/30 bg-emerald-500/10 p-3 text-sm text-emerald-800">
            {t("dialogs.update.readyHint", { version: targetVersion })}
          </div>
        )}

        {phase === "error" && (
          <div className="space-y-2 rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm">
            <p className="text-destructive">{error}</p>
            <p className="break-all text-xs text-muted-foreground">{GITHUB_RELEASE_URL}</p>
          </div>
        )}

        <DialogFooter>
          {phase === "available" && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                {t("common.close")}
              </Button>
              <DemoAction>
                <Button onClick={() => void startDownload()}>
                  <Download />
                  {t("dialogs.update.download")}
                </Button>
              </DemoAction>
            </>
          )}
          {phase === "readyToRestart" && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)} disabled={restarting}>
                {t("dialogs.update.closeNow")}
              </Button>
              <DemoAction>
                <Button onClick={() => void restartApp()} disabled={restarting}>
                  {restarting ? <Loader2 className="animate-spin" /> : <RefreshCw />}
                  {restarting ? t("dialogs.update.restarting") : t("dialogs.update.restart")}
                </Button>
              </DemoAction>
            </>
          )}
          {phase === "error" && (
            <>
              <Button variant="outline" onClick={() => void openRelease()}>
                <ExternalLink />
                {t("dialogs.update.openRelease")}
              </Button>
              <DemoAction>
                <Button onClick={() => void retry()}>
                  <RefreshCw />
                  {t("common.retry")}
                </Button>
              </DemoAction>
            </>
          )}
          {(phase === "idle" || phase === "upToDate") && (
            <Button variant="default" onClick={() => onOpenChange(false)}>
              {t("common.close")}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
