import { useEffect, useState } from "react";
import { Check, CircleAlert, Copy, ExternalLink } from "lucide-react";
import { toast } from "sonner";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { displayName } from "@/lib/account-display";
import * as api from "@/lib/api";
import { useT } from "@/lib/i18n";
import { DEFAULT_VARIANT, variantAppName, variantLabel } from "@/lib/variant";
import type { AccountMeta, WbVariant } from "@/lib/types";
import { useAccountsStore } from "@/stores/accounts";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 登录目标档位；缺省国内版（零回归）。 */
  variant?: WbVariant;
}

/** OAuth 登录采集：发起 → 打开浏览器 → 轮询采集结果 → 入库。 */
export function OAuthLoginDialog({ open, onOpenChange, variant = DEFAULT_VARIANT }: Props) {
  const t = useT();
  const reconcileAccounts = useAccountsStore((s) => s.reconcileAccounts);
  /** 登录链路文案按档位区分：国内版是扫码授权，国际版只有浏览器 Web 登录授权（无二维码）。 */
  const copy =
    variant === "ai"
      ? {
          // 国际版只说档位：与账号页同口径（原来写作「WorkBuddy 国际版 账号」，
          // 既有中英混排的冗余空格，又让同一句话里出现产品名与档位名两种叫法）
          title: t("dialogs.oauth.titleWeb", { variant: variantLabel(variant) }),
          description: t("dialogs.oauth.descriptionWeb"),
          start: t("dialogs.oauth.startWeb"),
          waiting: t("dialogs.oauth.waitingWeb"),
        }
      : {
          // 国内版保留产品名（档位与产品同名，无歧义）；产品名仍取自 variant.ts 单一来源
          title: t("dialogs.oauth.titleScan", { variant: variantLabel(variant) }),
          description: t("dialogs.oauth.descriptionScan", { app: variantAppName("cn") }),
          start: t("dialogs.oauth.startScan"),
          waiting: t("dialogs.oauth.waitingScan"),
        };

  const [busy, setBusy] = useState(false);
  const [loginId, setLoginId] = useState<string | null>(null);
  const [uri, setUri] = useState("");
  const [error, setError] = useState("");
  const [result, setResult] = useState<AccountMeta | null>(null);
  const [copied, setCopied] = useState(false);

  // 打开时重置
  useEffect(() => {
    if (open) {
      setBusy(false);
      setLoginId(null);
      setUri("");
      setError("");
      setResult(null);
      setCopied(false);
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
            setError(res.error || t("dialogs.oauth.loginFailed"));
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
  }, [loginId, reconcileAccounts, t]);

  async function start() {
    setBusy(true);
    setError("");
    try {
      const res = await api.oauthStart(variant);
      setLoginId(res.loginId);
      setUri(res.verificationUri);
      // 国内版沿用自动打开；国际版不自动跳浏览器，让用户自己复制链接到无痕窗口
      if (variant !== "ai") {
        await openInBrowser(res.verificationUri);
      }
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
    }
  }

  /** 复制验证链接：无痕窗口需要这条链接；失败必须如实提示，不能静默当成成功。 */
  async function copyLink() {
    try {
      await navigator.clipboard.writeText(uri);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      toast.error(t("dialogs.oauth.copyFailed"), {
        description: t("dialogs.oauth.copyFailedDescription"),
      });
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{copy.title}</DialogTitle>
          <DialogDescription>{copy.description}</DialogDescription>
        </DialogHeader>

        {!loginId && !result && (
          <div className="space-y-3 pt-2">
            <Button onClick={start} disabled={busy} className="w-full">
              {busy ? t("dialogs.oauth.starting") : copy.start}
            </Button>
          </div>
        )}

        {loginId && !result && (
          <div className="space-y-3">
            {variant === "ai" && (
              <Alert variant="warning">
                <CircleAlert className="size-4" />
                <AlertTitle>{t("dialogs.oauth.incognitoTitle")}</AlertTitle>
                <AlertDescription>{t("dialogs.oauth.incognitoDescription")}</AlertDescription>
              </Alert>
            )}
            <Alert>
              <ExternalLink className="size-4" />
              <AlertDescription className="break-all">
                <a
                  href={uri}
                  target="_blank"
                  rel="noreferrer"
                  className="cursor-pointer text-primary underline-offset-2 hover:underline"
                  onClick={(e) => {
                    // WebUI 直接使用浏览器默认链接行为，确保即使自动弹窗被拦截
                    // 也能通过用户点击打开验证页。
                    if (api.isWebui()) return;
                    e.preventDefault();
                    void openInBrowser(uri);
                  }}
                >
                  {uri}
                </a>
              </AlertDescription>
            </Alert>
            {variant === "ai" && (
              <Button variant="outline" size="sm" onClick={copyLink} className="w-full">
                {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
                {copied ? t("common.copied") : t("dialogs.oauth.copyLink")}
              </Button>
            )}
            <p className="text-sm text-muted-foreground">
              {copy.waiting}
            </p>
          </div>
        )}

        {result && (
          <Alert>
            <AlertDescription>
              {t("dialogs.oauth.collected", { name: displayName(result) })}
            </AlertDescription>
          </Alert>
        )}

        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {result && (
          <DialogFooter>
            <Button onClick={() => onOpenChange(false)}>{t("common.done")}</Button>
          </DialogFooter>
        )}
      </DialogContent>
    </Dialog>
  );
}

/** WebUI 使用浏览器新标签页，Tauri 使用系统 opener。 */
async function openInBrowser(url: string): Promise<void> {
  if (api.isWebui()) {
    // 浏览器环境没有 Tauri 注入的 invoke；window.open 被拦截时由弹窗中的
    // 原生链接作为兜底，因此这里不把拦截视为 OAuth 失败。
    try {
      window.open(url, "_blank", "noopener,noreferrer");
    } catch {
      // 忽略自动弹窗失败；弹窗中已展示的原生链接仍可点击。
    }
    return;
  }

  const { openUrl } = await import("@tauri-apps/plugin-opener");
  return openUrl(url);
}
