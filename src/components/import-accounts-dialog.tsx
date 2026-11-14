import { useEffect, useRef, useState } from "react";
import { FileUp, Loader2 } from "lucide-react";

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
import * as api from "@/lib/api";
import { useT, type Translate } from "@/lib/i18n";
import { DEFAULT_VARIANT, variantLabel } from "@/lib/variant";
import type { ImportPreviewAccount, WbVariant } from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 导入完成后回调（参数为导入结果计数，附所选账号中的加密凭据数）。 */
  onImported?: (result: {
    imported: number;
    skipped: number;
    overwritten: number;
    encrypted: number;
  }) => void;
  /** 当前档位；仅用于文案，导入结果按文件内各账号自身的档位归类。 */
  variant?: WbVariant;
}

/**
 * 导入预览账号展示名（脱敏展示：昵称/邮箱/uid）。
 * 只接受字符串：旧后端可能把信封对象透传进来，渲染对象会触发 React error #31（白屏）。
 */
function previewLabel(a: ImportPreviewAccount, t: Translate): string {
  const asLabel = (v: unknown) => (typeof v === "string" ? v : "");
  return (
    asLabel(a.nickname) ||
    asLabel(a.email) ||
    asLabel(a.uid) ||
    t("dialogs.import.itemLabel", { index: a.index + 1 })
  );
}

/** 导入账号弹框：选 JSON 文件 → 后端解析预览 → 勾选账号 → 导入合并。 */
export function ImportAccountsDialog({
  open,
  onOpenChange,
  onImported,
  variant = DEFAULT_VARIANT,
}: Props) {
  const t = useT();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [fileName, setFileName] = useState("");
  const [fileText, setFileText] = useState("");
  const [preview, setPreview] = useState<ImportPreviewAccount[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [parsing, setParsing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (open) {
      setFileName("");
      setFileText("");
      setPreview([]);
      setSelected(new Set());
      setParsing(false);
      setBusy(false);
      setError("");
    }
  }, [open]);

  function chooseFile() {
    fileInputRef.current?.click();
  }

  function onFileChange(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    // 清空 value，允许再次选择同一文件
    e.target.value = "";
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      const text = String(reader.result ?? "");
      setFileName(file.name);
      setFileText(text);
      setParsing(true);
      setError("");
      api
        .previewImportAccounts(text)
        .then((res) => {
          setPreview(res.accounts);
          setSelected(new Set(res.accounts.map((a) => a.index)));
        })
        .catch((err) => {
          setPreview([]);
          setSelected(new Set());
          setError(api.asError(err));
        })
        .finally(() => setParsing(false));
    };
    reader.readAsText(file);
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

  async function doImport() {
    if (busy || parsing || !fileText || selected.size === 0) return;
    setBusy(true);
    setError("");
    try {
      // 所选账号中的加密凭据数：随导入结果上报，供宿主附加能力限制提醒。
      const encrypted = preview.filter((a) => selected.has(a.index) && a.encrypted).length;
      const res = await api.importAccounts(fileText, [...selected]);
      onImported?.({ ...res, encrypted });
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
          <DialogTitle>{t("dialogs.import.title")}</DialogTitle>
          <DialogDescription>
            {t("dialogs.import.description")}
            {variant === "ai" &&
              ` ${t("dialogs.import.variantNote", { variant: variantLabel(variant) })}`}
          </DialogDescription>
        </DialogHeader>

        <input
          ref={fileInputRef}
          type="file"
          accept=".json,application/json"
          className="hidden"
          onChange={onFileChange}
        />

        <div className="flex items-center gap-2">
          <Button variant="outline" onClick={chooseFile} disabled={busy}>
            <FileUp />
            {t("dialogs.import.chooseFile")}
          </Button>
          {fileName && <span className="truncate text-xs text-muted-foreground">{fileName}</span>}
        </div>

        {parsing && (
          <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
            <Loader2 className="animate-spin" /> {t("dialogs.import.parsing")}
          </div>
        )}

        {!parsing && preview.length > 0 && (
          <>
            <div className="flex items-center justify-between text-sm">
              <span className="text-muted-foreground">
                {t("dialogs.common.selectedAccounts", {
                  total: preview.length,
                  selected: selected.size,
                })}
              </span>
              <button type="button" className="cursor-pointer text-primary hover:underline" onClick={toggleAll}>
                {allSelected ? t("dialogs.common.clearSelection") : t("dialogs.common.selectAll")}
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
                  <span className="min-w-0 flex-1 truncate text-sm">{previewLabel(a, t)}</span>
                  {a.encrypted ? (
                    <Badge variant="secondary">{t("dialogs.import.encryptedBadge")}</Badge>
                  ) : (
                    !a.hasToken && (
                      <Badge variant="outline">{t("dialogs.import.missingToken")}</Badge>
                    )
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
            {t("common.cancel")}
          </Button>
          <Button onClick={doImport} disabled={busy || parsing || selected.size === 0}>
            {busy ? t("dialogs.import.importing") : t("dialogs.import.confirm")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
