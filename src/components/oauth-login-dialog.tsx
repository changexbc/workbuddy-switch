import { useEffect, useState } from "react";
import { ExternalLink } from "lucide-react";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import * as api from "@/lib/api";
import type { AccountMeta } from "@/lib/types";
import { useAccountsStore } from "@/stores/accounts";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** OAuth 扫码登录采集：发起 → 打开浏览器 → 轮询采集结果 → 入库。 */
export function OAuthLoginDialog({ open, onOpenChange }: Props) {
  const reconcileAccounts = useAccountsStore((s) => s.reconcileAccounts);

  const [busy, setBusy] = useState(false);
  const [loginId, setLoginId] = useState<string | null>(null);
  const [uri, setUri] = useState("");
  const [error, setError] = useState("");
  const [result, setResult] = useState<AccountMeta | null>(null);
  const [manualToken, setManualToken] = useState("");

  // 打开时重置
  useEffect(() => {
    if (open) {
      setBusy(false);
      setLoginId(null);
      setUri("");
      setError("");
      setResult(null);
      setManualToken("");
    }
  }, [open]);

  // 轮询采集结果
  useEffect(() => {
    if (!loginId) return;
    let timer: number | undefined;
    let cancelled = false;

    const poll = async () => {
      try {
        const res = await api.oauthStatus(loginId);
        if (res.done) {
          if (res.result) {
            await reconcileAccounts();
            if (!cancelled) setResult(res.result);
          } else if (!cancelled) {
            setError(res.error || "登录失败");
          }
          if (timer !== undefined) window.clearInterval(timer);
          return;
        }
        timer = window.setTimeout(poll, 1500);
      } catch (e) {
        if (!cancelled) setError(api.asError(e));
        if (timer !== undefined) window.clearInterval(timer);
      }
    };
    poll();

    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [loginId, reconcileAccounts]);

  async function start() {
    setBusy(true);
    setError("");
    try {
      const res = await api.oauthStart();
      setLoginId(res.loginId);
      setUri(res.verificationUri);
      // 在系统浏览器打开验证页
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(res.verificationUri);
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
    }
  }

  /** 兜底：直接粘贴官方 access token 手动入库（对照 Python 版 manual-add 分支）。 */
  async function submitManualToken() {
    if (!manualToken.trim()) return;
    setBusy(true);
    setError("");
    try {
      const res = await api.manualAdd({ accessToken: manualToken.trim() });
      await reconcileAccounts();
      setResult(res.account);
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>OAuth 扫码登录</DialogTitle>
          <DialogDescription>
            在浏览器中打开验证链接，扫码授权后将自动采集账号并入库。
          </DialogDescription>
        </DialogHeader>

        {!loginId && !result && (
          <div className="space-y-3">
            <Button onClick={start} disabled={busy} className="w-full">
              {busy ? "正在发起登录…" : "开始扫码登录"}
            </Button>
            <div className="relative my-2">
              <div className="absolute inset-0 flex items-center">
                <span className="w-full border-t" />
              </div>
              <div className="relative flex justify-center text-xs text-muted-foreground">
                <span className="bg-background px-2">或直接粘贴 token</span>
              </div>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="manual-token">Access Token</Label>
              <Input
                id="manual-token"
                value={manualToken}
                onChange={(e) => setManualToken(e.target.value)}
                placeholder="粘贴 access token…"
              />
            </div>
            <Button variant="outline" onClick={submitManualToken} disabled={busy} className="w-full">
              手动入库
            </Button>
          </div>
        )}

        {loginId && !result && (
          <div className="space-y-3">
            <Alert>
              <ExternalLink className="size-4" />
              <AlertDescription className="break-all">
                <a
                  href={uri}
                  target="_blank"
                  rel="noreferrer"
                  className="text-primary underline-offset-2 hover:underline"
                  onClick={(e) => {
                    e.preventDefault();
                    void openInBrowser(uri);
                  }}
                >
                  {uri}
                </a>
              </AlertDescription>
            </Alert>
            <p className="text-sm text-muted-foreground">
              正在等待扫码授权，请在浏览器完成操作…
            </p>
          </div>
        )}

        {result && (
          <Alert>
            <AlertDescription>
              已采集账号：{result.nickname || result.email || result.id}
            </AlertDescription>
          </Alert>
        )}

        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            关闭
          </Button>
          {result && (
            <Button onClick={() => onOpenChange(false)}>完成</Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 在系统浏览器打开链接。 */
async function openInBrowser(url: string) {
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  return openUrl(url);
}
