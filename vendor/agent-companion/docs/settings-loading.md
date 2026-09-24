# Settings loading feedback

The settings HTML contains a skeleton outside `#root`, with a stylesheet linked
from the document head so it can paint before the JavaScript module graph loads.
`SettingsForm` alone controls the shell's `hidden` property in a layout effect.
React keeps placeholder settings hidden until both reads settle; failure reveals
the disabled form and retry action, and retry restores the skeleton. Reduced
motion disables the skeleton animation.

The native settings window closes and releases its WebView. Reopening constructs
it again, then loads the frontend and reads preferences plus listening settings
in parallel. This identifies the waiting stages, not their measured contribution
to latency. The skeleton does not accelerate native window creation.

Validation (2026-09-23):

- Typecheck, lint, production build, bundle guard and existing UI suite passed.
- `node scripts/qa-settings-loading.mjs` passed: JavaScript disabled, pending
  read, failure, retry, success, reduced motion and no browser runtime errors.
- Screenshot inspected: `artifacts/ui/settings-loading.png`.
- Node suite: 94 passed, 1 skipped; requires local port access outside sandbox.
- Native bundle built; native QA stopped at `native app starts a runtime in the
  isolated home`. Native visual acceptance and opening latency remain unverified.
