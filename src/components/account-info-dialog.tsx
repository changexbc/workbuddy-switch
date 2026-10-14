import { useEffect, useState } from "react";
import { Check, Copy, Loader2 } from "lucide-react";
import { toast } from "sonner";

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
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import * as api from "@/lib/api";
import { accountIdentity } from "@/lib/account-display";
import { formatRelativeTime, usedClientLabel } from "@/lib/account-order";
import { avatarTone } from "@/lib/avatar-tone";
import { cn } from "@/lib/utils";
import { accountVariant, variantLabel } from "@/lib/variant";
import type { AccountMeta, DisplayField } from "@/lib/types";

/** 备注长度上限：与后端 NOTE_MAX_CHARS 保持一致。 */
const NOTE_MAX_LENGTH = 24;

const FIELD_OPTIONS: Array<{ value: DisplayField; label: string }> = [
  { value: "nickname", label: "账号名" },
  { value: "phone", label: "手机号" },
  { value: "note", label: "备注" },
];

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 目标账号；为空时不渲染内容。 */
  account: AccountMeta | null;
  /** 保存成功后回调（携带更新后的账号元数据）。 */
  onSaved?: (account: AccountMeta) => void;
}

/**
 * 「最后使用」行文案：相对时间 + 精确时刻 + 来源端；从未切换给出明确说明。
 * 只统计通过本应用发起的切换，在客户端里自行登录/登出不计入。
 */
function lastUsedText(account: AccountMeta): string {
  const at = account.lastUsedAt;
  if (!at) return "从未切换";
  const date = new Date(at);
  const absolute = Number.isNaN(date.getTime())
    ? "—"
    : date.toLocaleString("zh-CN", {
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      });
  const client = usedClientLabel(account.lastUsedClient);
  return [`${formatRelativeTime(at, Date.now())}`, `${absolute}`, client].filter(Boolean).join(" · ");
}

/**
 * 账号信息弹框：查看账号信息、编辑本地备注、选择卡片显示字段。
 * 备注与显示字段只保存在本机账号库，不影响官方登录数据。
 */
export function AccountInfoDialog({ open, onOpenChange, account, onSaved }: Props) {
  const [note, setNote] = useState("");
  const [field, setField] = useState<DisplayField>("nickname");
  const [busy, setBusy] = useState(false);
  const [uidCopied, setUidCopied] = useState(false);
  // 关闭时父组件会清空 account：保留最后一次的账号继续渲染，让退出动画播完再卸载。
  const [snapshot, setSnapshot] = useState<AccountMeta | null>(null);

  // 每次打开按目标账号重置草稿状态。
  useEffect(() => {
    if (open && account) {
      setNote(account.note ?? "");
      setField(account.displayField ?? "nickname");
      setBusy(false);
      setUidCopied(false);
    }
  }, [open, account]);

  useEffect(() => {
    if (account) setSnapshot(account);
  }, [account]);

  const current = account ?? snapshot;
  if (!current) return null;

  // 闭包内收窄：函数参数在闭包里不被 TS 收窄，先取到局部常量。
  const accountId = current.id;
  const hasPhone = Boolean(current.phoneNumber);
  // 手机号不可用时回退账号名（含用户此前选择 phone 的存量数据）。
  const effectiveField: DisplayField = field === "phone" && !hasPhone ? "nickname" : field;

  // 头部身份区展示官方字段（昵称 / 手机号 / 邮箱），不随本地显示偏好变化。
  const name = current.nickname ?? current.uid ?? current.id ?? "未命名账号";
  const subtitle = current.phoneNumber ?? accountIdentity(current);
  const uid = current.uid;

  /** 「卡片显示」每格的实时预览值：备注格跟随输入框草稿，改完即可见。 */
  function previewValue(value: DisplayField): string {
    if (value === "nickname") return current?.nickname ?? "—";
    if (value === "phone") return current?.phoneNumber ?? "无手机号";
    return note.trim() || "未填写备注";
  }

  async function copyUid(value: string) {
    try {
      await navigator.clipboard.writeText(value);
      setUidCopied(true);
      window.setTimeout(() => setUidCopied(false), 1500);
    } catch (e) {
      toast.error("复制失败", { description: api.asError(e) });
    }
  }

  async function save() {
    setBusy(true);
    try {
      const res = await api.updateAccountDisplay(accountId, {
        note: note.trim() || null,
        displayField: effectiveField,
      });
      onSaved?.(res.account);
      toast.success("账号信息已保存");
      onOpenChange(false);
    } catch (e) {
      toast.error("保存失败", { description: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>账号信息</DialogTitle>
          <DialogDescription>备注与显示字段仅保存在本机。</DialogDescription>
        </DialogHeader>

        {/* 身份区：与账号卡片同一视觉语言（色调头像 + 名称 + 档位徽标）。 */}
        <div className="flex items-center gap-3">
          <div
            className={cn(
              "flex size-11 shrink-0 items-center justify-center rounded-full text-base font-semibold",
              avatarTone(name),
            )}
            aria-hidden="true"
          >
            {name.charAt(0).toUpperCase()}
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-center gap-2">
              <span className="truncate text-sm font-semibold" title={name}>
                {name}
              </span>
              <Badge variant="secondary" className="shrink-0 px-1.5 py-0 text-[10px] font-medium">
                {variantLabel(accountVariant(current))}
              </Badge>
            </div>
            <p className="mt-0.5 truncate text-xs text-muted-foreground" title={subtitle}>
              {subtitle}
            </p>
          </div>
        </div>

        {/* 次级字段：企业名与 UID（等宽 + 一键复制）。 */}
        <dl className="divide-y divide-border/70 rounded-lg border text-xs">
          <div className="flex items-center gap-3 px-3 py-2.5">
            <dt className="w-14 shrink-0 text-muted-foreground">企业名</dt>
            <dd className="min-w-0 flex-1 truncate font-medium" title={current.enterpriseName ?? ""}>
              {current.enterpriseName || "—"}
            </dd>
          </div>
          <div className="flex items-center gap-3 px-3 py-2.5">
            <dt className="w-14 shrink-0 text-muted-foreground">UID</dt>
            <dd
              className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground"
              title={uid ?? ""}
            >
              {uid ?? "—"}
            </dd>
            {uid && (
              <button
                type="button"
                className="shrink-0 cursor-pointer rounded-md p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                onClick={() => void copyUid(uid)}
                aria-label="复制 UID"
                title="复制 UID"
              >
                {uidCopied ? <Check className="size-3.5 text-primary" /> : <Copy className="size-3.5" />}
              </button>
            )}
          </div>
        </dl>

        {/* 最后使用：只记本应用触发的切换，供「刚才切了哪个」定位（时间 + 来源端）。 */}
        <div className="rounded-lg border px-3 py-2.5 text-xs">
          <div className="flex items-center gap-3">
            <span className="w-14 shrink-0 text-muted-foreground">最后使用</span>
            <span className="min-w-0 flex-1 truncate font-medium" title={lastUsedText(current)}>
              {lastUsedText(current)}
            </span>
          </div>
        </div>
        <div className="space-y-1.5">
          <div className="flex items-baseline justify-between">
            <Label htmlFor="account-note" className="text-xs font-medium text-muted-foreground">
              备注
            </Label>
            <span className="text-[11px] tabular-nums text-muted-foreground">
              {note.length}/{NOTE_MAX_LENGTH}
            </span>
          </div>
          <Input
            id="account-note"
            value={note}
            maxLength={NOTE_MAX_LENGTH}
            placeholder={`最多 ${NOTE_MAX_LENGTH} 个字符`}
            onChange={(e) => setNote(e.target.value)}
          />
        </div>

        <div className="space-y-1.5">
          <Label className="text-xs font-medium text-muted-foreground">卡片显示</Label>
          <RadioGroup
            value={effectiveField}
            onValueChange={(value) => setField(value as DisplayField)}
            className="grid grid-cols-3 gap-2"
          >
            {FIELD_OPTIONS.map((opt) => {
              const id = `display-field-${opt.value}`;
              const disabled = opt.value === "phone" && !hasPhone;
              const selected = effectiveField === opt.value;
              return (
                <Label
                  key={opt.value}
                  htmlFor={id}
                  className={cn(
                    "flex cursor-pointer flex-col gap-1 rounded-lg border px-2.5 py-2 transition-colors",
                    selected ? "border-primary/60 bg-primary/5" : "border-border hover:bg-muted/40",
                    disabled && "cursor-not-allowed opacity-50 hover:bg-transparent",
                  )}
                >
                  <span className="flex items-center justify-between gap-1">
                    <span className="text-xs font-medium">{opt.label}</span>
                    <RadioGroupItem value={opt.value} id={id} disabled={disabled} className="size-3.5 shrink-0" />
                  </span>
                  <span className="truncate text-[11px] text-muted-foreground">{previewValue(opt.value)}</span>
                </Label>
              );
            })}
          </RadioGroup>
          {!hasPhone && (
            <p className="text-[11px] text-muted-foreground">该账号没有手机号，无法按手机号显示。</p>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" disabled={busy} onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button disabled={busy} onClick={() => void save()}>
            {busy && <Loader2 className="animate-spin" />}
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
