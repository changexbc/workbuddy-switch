import { useEffect, useState } from "react";
import { ExternalLink, Loader2, RefreshCw } from "lucide-react";
import type { DownloadEvent } from "@tauri-apps/plugin-updater";

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
import { GITHUB_RELEASE_URL, openReleaseUrl } from "@/lib/update";
import type { UpdateInfo } from "@/lib/types";

type UpdateStage = "checking" | "downloading" | "latest" | "success" | "error";

const UPDATE_CHECK_TIMEOUT_MS = 15_000;

function withTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => {
      reject(new Error("检查更新超时，请检查网络连接后重试"));
    }, timeoutMs);

    promise.then(resolve, reject).finally(() => window.clearTimeout(timer));
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 从 Tauri updater 返回的原始 manifest 中取出当前平台的安装包地址。 */
function getDownloadUrl(rawJson: Record<string, unknown>): string | null {
  if (typeof rawJson.url === "string") return rawJson.url;
  if (!isRecord(rawJson.platforms)) return null;

  const preferredTargets = navigator.userAgent.includes("Intel")
    ? ["darwin-x86_64", "darwin-aarch64"]
    : ["darwin-aarch64", "darwin-x86_64"];
  for (const target of preferredTargets) {
    const platform = rawJson.platforms[target];
    if (isRecord(platform) && typeof platform.url === "string") return platform.url;
  }

  for (const platform of Object.values(rawJson.platforms)) {
    if (isRecord(platform) && typeof platform.url === "string") return platform.url;
  }
  return null;
}

interface UpdateInstallDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  update: UpdateInfo | null;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 统一的签名更新下载进度、成功和失败弹框。 */
export function UpdateInstallDialog({
  open,
  onOpenChange,
  update,
}: UpdateInstallDialogProps) {
  const [stage, setStage] = useState<UpdateStage>("checking");
  const [error, setError] = useState<string | null>(null);
  const [targetVersion, setTargetVersion] = useState(update?.latest ?? "新版本");
  const [received, setReceived] = useState(0);
  const [total, setTotal] = useState(0);
  const [retry, setRetry] = useState(0);
  const [downloadUrl, setDownloadUrl] = useState<string | null>(null);
  const [restarting, setRestarting] = useState(false);
  const releaseUrl = update?.releaseUrl ?? GITHUB_RELEASE_URL;

  useEffect(() => {
    if (!open) return;
    let cancelled = false;

    setStage("checking");
    setError(null);
    setReceived(0);
    setTotal(0);
    setDownloadUrl(null);
    setRestarting(false);
    setTargetVersion(update?.latest ?? "新版本");

    async function install() {
      try {
        if (api.isWebui()) {
          throw new Error("浏览器 webui 模式不能直接安装桌面更新包");
        }

        const { check } = await import("@tauri-apps/plugin-updater");
        const configuredProxy = await api
          .getGithubConfig()
          .then((config) => config.proxy?.trim() || "")
          .catch(() => "");
        const candidate = await withTimeout(
          check({ proxy: configuredProxy || undefined }),
          UPDATE_CHECK_TIMEOUT_MS,
        );
        if (cancelled) return;
        if (!candidate) {
          if (update?.hasUpdate) {
            setError("发现新版本，但 GitHub Release 暂无可用的签名更新包");
            setStage("error");
          } else {
            setStage("latest");
          }
          return;
        }

        setTargetVersion(candidate.version);
        setDownloadUrl(getDownloadUrl(candidate.rawJson));
        setStage("downloading");
        await candidate.downloadAndInstall((event: DownloadEvent) => {
          if (cancelled) return;
          if (event.event === "Started") {
            setTotal(event.data.contentLength ?? 0);
            setReceived(0);
          } else if (event.event === "Progress") {
            setReceived((current) => current + event.data.chunkLength);
          }
        });
        if (!cancelled) setStage("success");
      } catch (e) {
        if (!cancelled) {
          setError(api.asError(e));
          setStage("error");
        }
      }
    }

    void install();
    return () => {
      cancelled = true;
    };
  }, [open, retry, update?.hasUpdate, update?.latest]);

  const percent = total > 0 ? Math.min(100, Math.round((received / total) * 100)) : null;
  const busy = stage === "checking" || stage === "downloading";

  async function openRelease() {
    try {
      await openReleaseUrl(releaseUrl);
    } catch (e) {
      setError(api.asError(e));
    }
  }

  async function openDownload() {
    if (!downloadUrl) return;
    try {
      await openReleaseUrl(downloadUrl);
    } catch (e) {
      setError(api.asError(e));
    }
  }

  async function restartApp() {
    setRestarting(true);
    try {
      await api.relaunchApp();
    } catch (e) {
      setRestarting(false);
      setError(`重启失败：${api.asError(e)}`);
      setStage("error");
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && busy) return;
        onOpenChange(next);
      }}
    >
      <DialogContent showCloseButton={!busy && !restarting}>
        <DialogHeader>
          <DialogTitle>
            {stage === "checking" && "正在检查更新"}
            {stage === "downloading" && `正在升级到 v${targetVersion}`}
            {stage === "latest" && "当前已是最新版本"}
            {stage === "success" && "更新已安装"}
            {stage === "error" && "拉取更新失败"}
          </DialogTitle>
          <DialogDescription>
            {stage === "checking" && "正在检查 GitHub Release 中的签名更新包，请稍候。"}
            {stage === "downloading" && "请不要关闭应用，更新包下载完成后会安装到本机。"}
            {stage === "latest" && "没有发现高于当前版本的签名更新包。"}
            {stage === "success" && "更新包已安装，可以立即重启应用完成升级。"}
            {stage === "error" && "自动更新未完成，你仍然可以从 GitHub Release 页面手动下载。"}
          </DialogDescription>
        </DialogHeader>

        {stage === "checking" && (
          <div className="flex items-center gap-2 rounded-md bg-muted/50 p-3 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" />
            正在连接公开 Release 更新源…
          </div>
        )}

        {stage === "downloading" && (
          <div className="space-y-2 rounded-md border p-3">
            <div className="flex items-center justify-between text-sm">
              <span className="flex items-center gap-2">
                <Loader2 className="size-4 animate-spin text-primary" />
                下载更新包
              </span>
              <span className="font-mono text-xs text-muted-foreground">
                {percent === null ? "下载中…" : `${percent}%`}
              </span>
            </div>
            <div className="h-2 overflow-hidden rounded-full bg-muted">
              <div
                className="h-full rounded-full bg-primary transition-[width] duration-200"
                style={{ width: `${percent ?? 30}%` }}
              />
            </div>
            {total > 0 && (
              <div className="text-right text-xs text-muted-foreground">
                {formatBytes(received)} / {formatBytes(total)}
              </div>
            )}
            {downloadUrl && (
              <div className="border-t pt-2 text-xs">
                <div className="text-muted-foreground">下载地址（点击可手动下载）</div>
                <button
                  type="button"
                  className="mt-1 flex w-full items-start gap-1 break-all text-left text-primary underline-offset-2 hover:underline"
                  onClick={() => void openDownload()}
                >
                  <ExternalLink className="mt-0.5 size-3.5 shrink-0" />
                  <span>{downloadUrl}</span>
                </button>
              </div>
            )}
          </div>
        )}

        {stage === "success" && (
          <div className="rounded-md border border-emerald-500/30 bg-emerald-500/10 p-3 text-sm text-emerald-800">
            v{targetVersion} 已准备完成，重启应用后生效。
          </div>
        )}

        {stage === "error" && (
          <div className="space-y-2 rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm">
            <p className="text-destructive">{error || "未知更新错误"}</p>
            {downloadUrl && (
              <button
                type="button"
                className="flex w-full items-start gap-1 break-all text-left text-primary underline-offset-2 hover:underline"
                onClick={() => void openDownload()}
              >
                <ExternalLink className="mt-0.5 size-3.5 shrink-0" />
                <span>{downloadUrl}</span>
              </button>
            )}
            <p className="break-all text-xs text-muted-foreground">{releaseUrl}</p>
          </div>
        )}

        <DialogFooter>
          {stage === "error" && (
            <>
              <Button variant="outline" onClick={() => void openRelease()}>
                <ExternalLink />
                打开 GitHub Release
              </Button>
              <Button onClick={() => setRetry((value) => value + 1)}>
                <RefreshCw />
                重试
              </Button>
            </>
          )}
          {stage === "success" && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)} disabled={restarting}>
                立即关闭
              </Button>
              <Button onClick={() => void restartApp()} disabled={restarting}>
                {restarting ? <Loader2 className="animate-spin" /> : <RefreshCw />}
                {restarting ? "正在重启…" : "立即重启"}
              </Button>
            </>
          )}
          {(stage === "latest" || stage === "error") && (
            <Button variant={stage === "error" ? "ghost" : "default"} onClick={() => onOpenChange(false)}>
              关闭
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
