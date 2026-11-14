import { invoke } from "@tauri-apps/api/core";

import { isDesktop } from "@/lib/api";
import type { Locale } from "./locale";

/**
 * The tray and its notifications are rendered by the Rust host, which keeps its
 * own copy of the few strings it shows. The webui has no host, so the call is
 * desktop only.
 */
export function syncHostLocale(locale: Locale): void {
  if (!isDesktop()) return;
  void invoke("set_locale", { locale }).catch(() => {
    // The host keeps the previous language when the call fails; the webview is
    // already on the new one, so the mismatch is limited to the tray menu.
  });
}
