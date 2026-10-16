import { useEffect, useState } from "react";
import { CloudDownload, Loader2 } from "lucide-react";

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
import { Input } from "@/components/ui/input";
import * as api from "@/lib/api";
import { DEFAULT_VARIANT, variantLabel } from "@/lib/variant";
import type { ImportPreviewAccount, WbVariant } from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 恢复完成后回调（参数为导入结果计数），供宿主刷新账号列表。 */
  onRestored?: (result: {
    imported: number;
    skipped: number;
    overwritten: number;
    restoredConfigs: boolean;
  }) => void;
  /** 当前档位；仅用于文案，备份中的账号按各自档位归类。 */
  variant?: WbVariant;
}

/** 恢复预览账号展示名（脱敏展示：昵称/邮箱/uid），与导入弹框同口径。 */
function previewLabel(a: ImportPreviewAccount): string {
  const asLabel = (v: unknown) => (typeof v === "string" ? v : "");
  return asLabel(a.nickname) || asLabel(a.email) || asLabel(a.uid) || `第 ${a.index + 1} 项`;
}

/**
 * 从云端 WebDAV 备份恢复账号：输入备份加密密码 → 拉取解密预览 → 勾选 → 合并入库。
 * 与本地导入共用合并语义（同 uid 覆盖、其余追加），token 仅在服务端加密态传输。
 */
export function WebdavRestoreDialog({
  open,
  onOpenChange,
  onRestored,
  variant = DEFAULT_VARIANT,
}: Props) {
  const [passphrase, setPassphrase] = useState("");
  const [preview, setPreview] = useState<ImportPreviewAccount[]>([]);
  const [hasConfigs, setHasConfigs] = useState(false);
  const [restoreConfigs, setRestoreConfigs] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [reading, setReading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (open) {
      setPassphrase("");
      setPreview([]);
      setHasConfigs(false);
      setRestoreConfigs(false);
      setSelected(new Set());
      setReading(false);
      setBusy(false);
      setError("");
    }
  }, [open]);

  async function loadPreview() {
    if (reading || busy || !passphrase) return;
    setReading(true);
    setError("");
    setPreview([]);
    setHasConfigs(false);
    setRestoreConfigs(false);
    setSelected(new Set());
    try {
      const res = await api.webdavRestorePreview(passphrase);
      setPreview(res.accounts);
      setHasConfigs(Boolean(res.hasConfigs));
      setSelected(new Set(res.accounts.map((a) => a.index)));
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setReading(false);
    }
  }

  const allSelected = preview.length > 0 && selected.size === preview.length;

  function toggleAll() {
    setSelected(allSelected ? new Set() : new Set(preview.map((a) => a.index)));
  }

  function toggle(index: number) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(index)) next.delete(index);
      else next.add(index);
      return next;
    });
  }

  async function doRestore() {
    if (busy || reading || selected.size === 0) return;
    setBusy(true);
    setError("");
    try {
      const res = await api.webdavRestore(passphrase, [...selected], restoreConfigs);
      onRestored?.({
        imported: res.imported,
        skipped: res.skipped,
        overwritten: res.overwritten,
        restoredConfigs: Boolean(res.restoredConfigs),
      });
      onOpenChange(false);
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="min-w-0 overflow-x-hidden">
        <DialogHeader>
          <DialogTitle>从云端恢复账号</DialogTitle>
          <DialogDescription>
            输入备份加密密码读取 WebDAV 云端备份，勾选要恢复的账号。
            {variant === "ai" &&
              `备份中的国内版与国际版账号按各自档位归类，不随当前${variantLabel(variant)}改变。`}
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-2">
          <Input
            type="password"
            value={passphrase}
            onChange={(e) => setPassphrase(e.target.value)}
            placeholder="备份加密密码"
            spellCheck={false}
            autoComplete="off"
            className="w-full"
          />
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              onClick={() => void loadPreview()}
              disabled={reading || busy || !passphrase}
            >
              {reading ? <Loader2 className="animate-spin" /> : <CloudDownload />}
              读取云端备份
            </Button>
            {preview.length > 0 && (
              <span className="text-xs text-muted-foreground">
                共 {preview.length} 个账号，已选 {selected.size} 个
              </span>
            )}
          </div>
        </div>

        {reading && (
          <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
            <Loader2 className="animate-spin" /> 正在拉取并解密…
          </div>
        )}

        {!reading && preview.length > 0 && (
          <>
            {hasConfigs && (
              <label className="flex cursor-pointer items-center gap-2 rounded-md border px-3 py-2 text-sm hover:bg-accent/50">
                <input
                  type="checkbox"
                  className="size-4 accent-primary"
                  checked={restoreConfigs}
                  onChange={(e) => setRestoreConfigs(e.target.checked)}
                />
                <span>同时恢复备份中的功能配置（签到 / 旅行 / 限额 / 自动轮换 / 更新源）</span>
              </label>
            )}
            <div className="flex items-center justify-between text-sm">
              <span className="text-muted-foreground">勾选要恢复的账号</span>
              <button
                type="button"
                className="cursor-pointer text-primary hover:underline"
                onClick={toggleAll}
              >
                {allSelected ? "取消全选" : "全选"}
              </button>
            </div>
            <div className="max-h-56 space-y-1 overflow-y-auto pr-1">
              {preview.map((a) => (
                <label
                  key={a.index}
                  className="flex cursor-pointer items-center gap-3 rounded-md border px-3 py-2 hover:bg-accent/50"
                >
                  <input
                    type="checkbox"
                    className="size-4 accent-primary"
                    checked={selected.has(a.index)}
                    onChange={() => toggle(a.index)}
                  />
                  <span className="min-w-0 flex-1 truncate text-sm">{previewLabel(a)}</span>
                  {a.encrypted ? (
                    <Badge variant="secondary">加密凭据</Badge>
                  ) : (
                    !a.hasToken && <Badge variant="outline">缺少 token</Badge>
                  )}
                </label>
              ))}
            </div>
          </>
        )}

        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            取消
          </Button>
          <Button onClick={doRestore} disabled={busy || reading || selected.size === 0}>
            {busy ? "恢复中…" : "恢复勾选账号"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
