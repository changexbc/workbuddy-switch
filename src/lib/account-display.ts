import { translate } from "@/lib/i18n/translate";
import type { DisplayField } from "@/lib/types";

/**
 * 显示名计算所需字段（均可缺省）：
 * 本地账号记录、当前登录态（AppStatus.current）、备份预览记录都能直接传入。
 */
export interface DisplayableAccount {
  nickname?: string | null;
  uid?: string | null;
  id?: string | null;
  email?: string | null;
  /** 官方手机号（仅国内版账号可能有）。 */
  phoneNumber?: string | null;
  /** 本地备注。 */
  note?: string | null;
  /** 本地显示字段偏好。 */
  displayField?: DisplayField | null;
}

/**
 * 账号显示名：按本地 `displayField` 取字段（备注 / 手机号 / 账号名），
 * 选定字段为空时回退 `nickname → uid → id`。
 *
 * 所有展示账号名的位置统一走本函数，保证卡片、切换弹窗、选择列表等名称一致。
 */
export function displayName(account: DisplayableAccount): string {
  const byField =
    account.displayField === "phone"
      ? account.phoneNumber
      : account.displayField === "note"
        ? account.note
        : account.nickname;
  return byField || account.nickname || account.uid || account.id || translate("lib.account.unnamed");
}

/** 账号身份行：邮箱脱敏展示（本地段只留首字符），无邮箱时回退 UID / ID。 */
export function accountIdentity(account: DisplayableAccount): string {
  if (account.email) {
    const [local, domain] = account.email.split("@");
    if (!domain) return account.email;
    return `${local.slice(0, 1)}${"*".repeat(Math.max(3, local.length - 1))}@${domain}`;
  }
  return account.uid ? `UID · ${account.uid}` : `ID · ${account.id}`;
}
