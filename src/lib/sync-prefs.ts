import { useEffect, useState } from "react";

/**
 * 切换账号时默认勾选归档同步的偏好设置（Issue #165）。
 *
 * 语义：
 * - 开启后，切换账号时会预先勾选可仅同步归档状态的关联会话，保留正文。
 *   用户仍可随时取消勾选，确认切换后才执行；正文快进和覆盖沿用原有规则。
 * - 默认值：关闭（防误操作）。
 *
 * 持久化在 localStorage，键名 `wb-switch.sync.default-archive`，值 `"1"` 为开，其余（`"0"`、缺省或异常）均视为关闭。
 */
const DEFAULT_ARCHIVE_SYNC_KEY = "wb-switch.sync.default-archive";

/** 同页面内的变更通知（`storage` 事件只在其它标签页触发）。 */
const SYNC_PREFS_CHANGED_EVENT = "wb-switch:sync-prefs-changed";

export function isDefaultArchiveSyncEnabled(): boolean {
  try {
    return localStorage.getItem(DEFAULT_ARCHIVE_SYNC_KEY) === "1";
  } catch {
    return false;
  }
}

export function setDefaultArchiveSyncEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(DEFAULT_ARCHIVE_SYNC_KEY, enabled ? "1" : "0");
  } catch {
    /* 忽略隐私模式等写入异常 */
  }
  window.dispatchEvent(new Event(SYNC_PREFS_CHANGED_EVENT));
}

export function useDefaultArchiveSyncEnabled(): boolean {
  const [enabled, setEnabled] = useState(isDefaultArchiveSyncEnabled);
  useEffect(() => {
    const sync = () => setEnabled(isDefaultArchiveSyncEnabled());
    window.addEventListener("storage", sync);
    window.addEventListener(SYNC_PREFS_CHANGED_EVENT, sync);
    return () => {
      window.removeEventListener("storage", sync);
      window.removeEventListener(SYNC_PREFS_CHANGED_EVENT, sync);
    };
  }, []);
  return enabled;
}
