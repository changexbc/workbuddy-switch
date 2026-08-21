import { useEffect, useState } from "react";
import { Download, FolderOpen, Loader2 } from "lucide-react";

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
import * as api from "@/lib/api";
import type { AccountMeta } from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  accounts: AccountMeta[];
  /** 导出完成后回调（参数为导出的账号数）。 */
  onExported?: (count: number) => void;
}

/** 账号展示名（与账号卡片一致）。 */
function accountLabel(a: AccountMeta): string {
  return a.nickname || a.email || a.uid || a.id;
}

/** 导出文件名：wb-switch-accounts-YYYY-MM-DD.json */
function exportFileName(): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `wb-switch-accounts-${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}.json`;
}

/** 前端 Blob 下载（webui 浏览器用）。 */
function downloadJson(filename: string, data: unknown) {
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

/** 桌面端在系统文件管理器中定位导出文件（Windows 为资源管理器）。 */
async function revealInFinder(path: string): Promise<void> {
  const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
  await revealItemInDir(path);
}

/** 按平台显示文件管理器文案（与 update-install-dialog 的 userAgent 判断一致）。 */
function revealLabel(): string {
  const ua = navigator.userAgent;
  if (ua.includes("Windows")) return "在资源管理器中显示";
  if (ua.includes("Linux")) return "在文件管理器中显示";
  return "在 Finder 中显示";
}

/** 导出账号弹框：多选账号 → 后端导出完整记录（含 token）→ 下载 JSON。 */
export function ExportAccountsDialog({ open, onOpenChange, accounts, onExported }: Props) {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  /** 桌面端导出成功后的文件路径（webui 用浏览器下载，无此状态）。 */
  const [savedPath, setSavedPath] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setSelected(new Set());
      setBusy(false);
      setError("");
      setSavedPath(null);
    }
  }, [open]);

  const allSelected = accounts.length > 0 && selected.size === accounts.length;

  function toggleAll() {
    setSelected(allSelected ? new Set() : new Set(accounts.map((a) => a.id)));
  }

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function doExport() {
    if (busy || selected.size === 0) return;
    setBusy(true);
    setError("");
    try {
      const ids = [...selected];
      if (api.isWebui()) {
        // webui：浏览器 Blob 下载
        const res = await api.exportAccounts(ids);
        downloadJson(exportFileName(), res.accounts);
        onExported?.(res.accounts.length);
        onOpenChange(false);
      } else {
        // 桌面端：系统保存对话框选位置 → 后端写入该路径（WKWebView 不支持 `<a download>`）
        const { save } = await import("@tauri-apps/plugin-dialog");
        const path = await save({
          title: "导出账号",
          defaultPath: exportFileName(),
          filters: [{ name: "JSON", extensions: ["json"] }],
        });
        if (!path) return; // 用户取消保存对话框
        const res = await api.exportAccountsToPath(ids, path);
        setSavedPath(res.path);
        onExported?.(ids.length);
      }
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
          <DialogTitle>导出账号</DialogTitle>
          <DialogDescription>勾选要导出的账号，导出为 JSON 文件。</DialogDescription>
        </DialogHeader>

        <Alert variant="warning">
          <AlertTitle>安全提示</AlertTitle>
          <AlertDescription>
            导出文件含登录 token，等同密码，请勿上传网盘或发送给他人。
          </AlertDescription>
        </Alert>

        {accounts.length === 0 ? (
          <p className="py-4 text-center text-sm text-muted-foreground">暂无账号可导出。</p>
        ) : (
          <>
            <div className="flex items-center justify-between text-sm">
              <span className="text-muted-foreground">
                共 {accounts.length} 个账号，已选 {selected.size} 个
              </span>
              <button type="button" className="text-primary hover:underline" onClick={toggleAll}>
                {allSelected ? "取消全选" : "全选"}
              </button>
            </div>
            <div className="max-h-56 space-y-1 overflow-y-auto pr-1">
              {accounts.map((a) => (
                <label
                  key={a.id}
                  className="flex cursor-pointer items-center gap-3 rounded-md border px-3 py-2 hover:bg-accent/50"
                >
                  <input
                    type="checkbox"
                    className="size-4 accent-primary"
                    checked={selected.has(a.id)}
                    onChange={() => toggle(a.id)}
                  />
                  <span className="min-w-0 flex-1 truncate text-sm">{accountLabel(a)}</span>
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

        {savedPath && (
          <Alert>
            <AlertTitle>导出成功</AlertTitle>
            <AlertDescription className="space-y-2">
              <span className="block break-all font-mono text-xs">{savedPath}</span>
              <Button variant="outline" size="sm" onClick={() => void revealInFinder(savedPath)}>
                <FolderOpen />
                {revealLabel()}
              </Button>
            </AlertDescription>
          </Alert>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            {savedPath ? "完成" : "取消"}
          </Button>
          {!savedPath && (
            <Button onClick={doExport} disabled={busy || selected.size === 0}>
              {busy ? <Loader2 className="animate-spin" /> : <Download />}
              导出勾选账号
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
