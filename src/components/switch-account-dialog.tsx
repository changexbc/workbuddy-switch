import { useEffect, useState } from "react";
import { ExternalLink, Loader2 } from "lucide-react";
import { listen } from "@tauri-apps/api/event";

import { Alert, AlertDescription } from "@/components/ui/alert";
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
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import * as api from "@/lib/api";
import type { AccountMeta, Session, SwitchResult } from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 目标账号 */
  account: AccountMeta | null;
  /** 切换完成后刷新列表 */
  onDone?: () => void;
}

/** 切换账号弹窗：可勾选当前账号的会话复制到目标账号（路径 B）。 */
export function SwitchAccountDialog({ open, onOpenChange, account, onDone }: Props) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [copySessions, setCopySessions] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<SwitchResult | null>(null);
  const [currentUid, setCurrentUid] = useState<string | null>(null);
  const [progress, setProgress] = useState("");

  // 监听后端切换进度：桌面端走 Tauri 事件，webui 走 HTTP 轮询
  useEffect(() => {
    if (api.isWebui()) {
      const timer = setInterval(() => {
        void api.switchProgress().then((p) => {
          if (p.progress) setProgress(p.progress);
        });
      }, 600);
      return () => clearInterval(timer);
    }
    let unlisten: (() => void) | undefined;
    listen<{ message: string }>("switch-progress", (e) => {
      setProgress(e.payload.message);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  // 打开时加载当前账号会话
  useEffect(() => {
    if (open && account) {
      setCopySessions(false);
      setSelected(new Set());
      setError("");
      setResult(null);
      setLoadingSessions(true);
      api
        .listSessions()
        .then((res) => {
          setSessions(res.sessions);
          setCurrentUid(res.current);
        })
        .catch((e) => setError(api.asError(e)))
        .finally(() => setLoadingSessions(false));
    }
  }, [open, account]);

  function toggleSession(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function doSwitch() {
    if (!account) return;
    setBusy(true);
    setProgress("正在切换账号…");
    setError("");
    try {
      const res = await api.switchAccount({
        accountId: account.id,
        copySessionIds: copySessions ? [...selected] : undefined,
      });
      setResult(res);
      onDone?.();
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
      setProgress("");
    }
  }

  /** 打开系统设置授权面板（默认完全磁盘访问），供小白一键跳转。 */
  async function openPermissionSettings() {
    try {
      await api.openPermissionSettings("all_files");
    } catch (e) {
      // 打开失败时退化为提示
      setError(api.asError(e));
    }
  }

  /** 权限自检：确认完全磁盘访问是否生效。 */
  const [permCheck, setPermCheck] = useState<string | null>(null);
  async function runPermissionCheck() {
    setPermCheck("检测中…");
    try {
      const res = await api.checkAuthPermission();
      setPermCheck(res.ok ? `✓ ${res.message}` : `✗ ${res.error}（${res.dir}）`);
    } catch (e) {
      setPermCheck(`✗ ${api.asError(e)}`);
    }
  }

  // 出现「无权限」错误时，自动每 2s 轮询一次授权状态；用户拖入 app 授权成功后自动恢复
  useEffect(() => {
    if (!error.includes("无权限")) return;
    let cancelled = false;
    let timer: number | undefined;
    const check = async () => {
      try {
        const res = await api.checkAuthPermission();
        if (res.ok) {
          if (!cancelled) {
            setPermCheck("✓ 授权成功，可以重新切换了");
            setError("");
          }
          return;
        }
      } catch {
        /* 忽略中间态 */
      }
      if (!cancelled) timer = window.setTimeout(check, 2000);
    };
    check();
    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, [error]);

  const copyCount = copySessions ? selected.size : 0;
  const needsPermission = error.includes("无权限");

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent showCloseButton={!busy}>
        <DialogHeader>
          <DialogTitle>切换到「{account?.nickname || account?.email || account?.uid || "该账号"}」</DialogTitle>
          <DialogDescription>
            切换会关闭并重启 WorkBuddy，认证文件将写入目标账号。
          </DialogDescription>
        </DialogHeader>

        {busy && (
          <div className="absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg bg-background/85 backdrop-blur-sm">
            <Loader2 className="size-8 animate-spin text-primary" />
            <p className="text-sm font-medium">{progress || "正在切换账号…"}</p>
            <p className="max-w-xs text-center text-xs text-muted-foreground">
              正在处理中，请勿关闭窗口
            </p>
          </div>
        )}

        <div className="space-y-3">
          <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
            <div>
              <div className="text-sm font-medium">复制会话到目标账号</div>
              <div className="text-xs text-muted-foreground">
                将当前账号勾选的会话以新 id 复制给目标账号（云端归属目标）
              </div>
            </div>
            <Switch
              checked={copySessions}
              onCheckedChange={setCopySessions}
              disabled={loadingSessions || sessions.length === 0}
            />
          </div>

          {copySessions && (
            <>
              <Separator />
              <div className="max-h-56 space-y-1 overflow-y-auto pr-1">
                {loadingSessions ? (
                  <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
                    <Loader2 className="animate-spin" /> 加载会话…
                  </div>
                ) : sessions.length === 0 ? (
                  <p className="py-4 text-center text-sm text-muted-foreground">
                    {currentUid ? "当前账号暂无会话" : "未检测到当前登录账号，无法列出会话"}
                  </p>
                ) : (
                  sessions.map((s) => (
                    <label
                      key={s.id}
                      className="flex cursor-pointer items-center gap-3 rounded-md border px-3 py-2 hover:bg-accent/50"
                    >
                      <input
                        type="checkbox"
                        className="size-4 accent-primary"
                        checked={selected.has(s.id)}
                        onChange={() => toggleSession(s.id)}
                      />
                      <span className="min-w-0 flex-1 truncate text-sm">{s.title}</span>
                      {s.hasHistory && <Badge variant="outline">有正文</Badge>}
                    </label>
                  ))
                )}
              </div>
            </>
          )}
        </div>

        {error && (
          <Alert variant={needsPermission ? "warning" : "destructive"}>
            <AlertDescription>
              <div>{error}</div>
              {needsPermission && (
                <div className="mt-2 space-y-2">
                  <div className="rounded-md border bg-muted/60 p-3 text-xs text-muted-foreground">
                    <p className="mb-1 font-medium text-foreground">如何授权（只需 3 步）：</p>
                    <ol className="list-decimal space-y-1 pl-4">
                      <li>点击下方「打开完全磁盘访问」</li>
                      <li>
                        把 <b>workbuddy-switch.app</b> 从 Finder 拖进面板列表（即使没提示框也直接拖），
                        打开它的开关
                      </li>
                      <li>授权后这里会自动检测到，无需其他操作</li>
                    </ol>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    <Button variant="outline" size="sm" onClick={openPermissionSettings}>
                      <ExternalLink />
                      打开完全磁盘访问
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => void api.revealAppInFinder()}
                    >
                      在 Finder 中显示
                    </Button>
                    <Button variant="secondary" size="sm" onClick={runPermissionCheck}>
                      立即检测
                    </Button>
                  </div>
                </div>
              )}
              {permCheck && <div className="mt-2 text-xs">{permCheck}</div>}
            </AlertDescription>
          </Alert>
        )}
        {result && (
          <Alert>
            <AlertDescription>
              已切换至「{result.account}」。
              {result.sessionCopy
                ? ` 已复制 ${result.sessionCopy.copied.length} 个会话`
                : ""}
              {result.backup ? ` 认证文件备份：${result.backup}` : ""}
              {" CodeBuddy CLI 保持原当前账号；如需切换，请在对应账号卡片上单独点击 CLI 切换。"}
            </AlertDescription>
          </Alert>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            取消
          </Button>
          {!result && (
            <Button onClick={doSwitch} disabled={busy || (copySessions && copyCount === 0)}>
              {busy ? "切换中…" : "确认切换"}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
