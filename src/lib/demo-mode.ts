import { translate } from "./i18n/translate";

/** Every write in a demo build is refused with this text (toast, tooltip, aria label). */
export function demoUnavailableMessage(): string {
  return translate("lib.demoMode.unavailable");
}

/** Public demo and README screenshot builds share the same read-only frontend runtime. */
export const demoModeEnabled =
  import.meta.env.VITE_DEMO_MODE === "1" || import.meta.env.VITE_SCREENSHOT_DEMO === "1";

/** GitHub Pages needs project-relative assets and hash routes; other demo/dev builds do not. */
export const pagesDemoHostingEnabled = import.meta.env.VITE_PAGES_DEMO === "1";
