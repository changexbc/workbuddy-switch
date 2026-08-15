import { useEffect, useState } from "react";

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

/** 手动添加账号（完整字段表单）。 */
export function ManualAddDialog({ open, onOpenChange }: Props) {
  const upsertAccount = useAccountsStore((s) => s.upsertAccount);

  const [accessToken, setAccessToken] = useState("");
  const [refreshToken, setRefreshToken] = useState("");
  const [uid, setUid] = useState("");
  const [nickname, setNickname] = useState("");
  const [email, setEmail] = useState("");
  const [domain, setDomain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<AccountMeta | null>(null);

  useEffect(() => {
    if (open) {
      setAccessToken("");
      setRefreshToken("");
      setUid("");
      setNickname("");
      setEmail("");
      setDomain("");
      setBusy(false);
      setError("");
      setResult(null);
    }
  }, [open]);

  async function submit() {
    setBusy(true);
    setError("");
    try {
      const res = await api.manualAdd({
        accessToken: accessToken.trim(),
        refreshToken: refreshToken.trim() || undefined,
        uid: uid.trim() || undefined,
        nickname: nickname.trim() || undefined,
        email: email.trim() || undefined,
        domain: domain.trim() || undefined,
      });
      upsertAccount(res.account);
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
          <DialogTitle>手动添加账号</DialogTitle>
          <DialogDescription>填入官方账号凭据（access token 必填）。</DialogDescription>
        </DialogHeader>

        <div className="grid gap-3">
          <div className="space-y-1.5">
            <Label htmlFor="ma-token">
              Access Token <span className="text-destructive">*</span>
            </Label>
            <Input
              id="ma-token"
              value={accessToken}
              onChange={(e) => setAccessToken(e.target.value)}
              placeholder="eyJhbGciOi…"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="ma-refresh">Refresh Token</Label>
            <Input
              id="ma-refresh"
              value={refreshToken}
              onChange={(e) => setRefreshToken(e.target.value)}
              placeholder="可选"
            />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="ma-uid">UID</Label>
              <Input
                id="ma-uid"
                value={uid}
                onChange={(e) => setUid(e.target.value)}
                placeholder="可选"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="ma-nickname">昵称</Label>
              <Input
                id="ma-nickname"
                value={nickname}
                onChange={(e) => setNickname(e.target.value)}
                placeholder="可选"
              />
            </div>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="ma-email">邮箱</Label>
              <Input
                id="ma-email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="默认：手动添加"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="ma-domain">Domain</Label>
              <Input
                id="ma-domain"
                value={domain}
                onChange={(e) => setDomain(e.target.value)}
                placeholder="如 www.codebuddy.cn"
              />
            </div>
          </div>
        </div>

        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        {result && (
          <Alert>
            <AlertDescription>
              已添加：{result.nickname || result.email || result.id}
            </AlertDescription>
          </Alert>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button onClick={submit} disabled={busy || !accessToken.trim()}>
            {busy ? "提交中…" : "添加账号"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
