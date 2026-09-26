import { useEffect, useMemo, useState } from "react";
import { Loader2 } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { JetbrainsMark } from "@/components/product-marks";
import * as api from "@/lib/api";
import type { AccountMeta, JetbrainsStatus } from "@/lib/types";
import { cn } from "@/lib/utils";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 目标账号 */
  account: AccountMeta | null;
  /** JetBrains 状态（提供已装插件的目录列表与运行状态）。 */
  jetbrainsStatus?: JetbrainsStatus | null;
  /** 切换完成后刷新列表 */
  onDone?: () => void;
}

/**
 * JetBrains 插件切换弹窗：选择要写入的 IDE（配置目录）后执行切换。
 *
 * 检测到多个装了插件的 IDE 时可只切其中一个；缺省全选（与「一次切换全家生效」
 * 的缺省行为一致）。只装了一个时同样弹窗——把切换范围显式化，不引入第二种交互。
 */
export function JetbrainsSwitchDialog({ open, onOpenChange, account, jetbrainsStatus, onDone }: Props) {
  const targets = useMemo(
    () => (jetbrainsStatus?.targets ?? []).filter((t) => t.pluginInstalled),
    [jetbrainsStatus],
  );
  /** 勾选的配置目录名；undefined = 尚未初始化（效果等同全选）。 */
  const [selected, setSelected] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);

  // 每次打开弹窗重置为全选（目标集合可能因装/卸插件而变化，不保留上次勾选）。
  useEffect(() => {
    if (open) setSelected(null);
  }, [open, account?.id]);

  const checked = selected ?? targets.map((t) => t.configDir);
  const allChecked = targets.length > 0 && checked.length === targets.length;

  function toggle(dir: string, on: boolean) {
    setSelected((prev) => {
      const base = prev ?? targets.map((t) => t.configDir);
      return on ? [...new Set([...base, dir])] : base.filter((d) => d !== dir);
    });
  }

  async function onConfirm() {
    if (!account || checked.length === 0 || busy) return;
    setBusy(true);
    const toastId = toast.loading("正在切换 JetBrains IDE…", {
      description: "将注入凭证，运行中的 IDEA / PyCharm 会先退出再自动重开",
    });
    try {
      const result = await api.switchJetbrainsAccount(
        account.id,
        true,
        checked.length === targets.length ? undefined : checked,
      );
      await onDone?.();
      toast.success("JetBrains IDE 已切换", {
        id: toastId,
        description: result.message || result.account,
      });
      onOpenChange(false);
    } catch (error) {
      toast.error("JetBrains IDE 切换失败", {
        id: toastId,
        description: api.asError(error),
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <JetbrainsMark size={20} />
            切换 JetBrains 插件账号
          </DialogTitle>
          <DialogDescription>
            {account ? (
              <>把 {account.nickname || account.uid || "该账号"} 设为所选 IDE 的 CodeBuddy 插件登录账号。</>
            ) : (
              "选择要写入的 IDE。"
            )}
          </DialogDescription>
        </DialogHeader>

        {targets.length === 0 ? (
          <p className="rounded-lg bg-muted/60 px-3 py-2 text-sm text-muted-foreground">
            未检测到已安装 CodeBuddy 插件的 JetBrains IDE。请先在 IDEA / PyCharm 中安装「Tencent Cloud
            CodeBuddy」插件后重试。
          </p>
        ) : (
          <div className="space-y-1">
            <label className="flex cursor-pointer items-center gap-2.5 rounded-lg px-2 py-1.5 text-sm font-medium hover:bg-muted/60">
              <Checkbox
                checked={allChecked}
                onCheckedChange={(on) => setSelected(on ? null : [])}
                aria-label="全选"
              />
              全部 IDE
            </label>
            {targets.map((t) => (
              <label
                key={t.configDir}
                className={cn(
                  "flex cursor-pointer items-center gap-2.5 rounded-lg px-2 py-1.5 text-sm hover:bg-muted/60",
                  checked.length === 0 && "opacity-60",
                )}
              >
                <Checkbox
                  checked={checked.includes(t.configDir)}
                  onCheckedChange={(on) => toggle(t.configDir, Boolean(on))}
                  aria-label={`写入 ${t.configDir}`}
                />
                <span className="min-w-0 flex-1 truncate">{t.configDir}</span>
                {t.running && (
                  <span className="flex-none rounded-full bg-primary/10 px-2 py-0.5 text-xs text-primary">
                    运行中
                  </span>
                )}
              </label>
            ))}
            <p className="px-2 pt-1 text-xs leading-5 text-muted-foreground">
              运行中的 IDE 会先退出（弹「确认退出」时自动确认）再写入并重新打开；未运行的 IDE 写入后下次打开生效。
            </p>
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            取消
          </Button>
          <Button onClick={onConfirm} disabled={busy || targets.length === 0 || checked.length === 0}>
            {busy ? <Loader2 className="size-4 animate-spin" /> : null}
            {busy ? "切换中…" : `切换（${checked.length}/${targets.length}）`}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
