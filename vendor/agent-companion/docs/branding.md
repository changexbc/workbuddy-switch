# Agent Companion artwork

The approved artwork is the first **conversation-tail cat**: jade-green body,
dark forest-green belly, dark eyes and smile, warm yellow background. The mint
belly experiment is not used. The tail encloses speech-bubble negative space.

## Sources and regeneration

Committed sources live in `assets/branding/`, outside the ignored exploration
directory. `app-source.png` is the approved artwork copied unchanged.
`transparent-source.png` and `tray-template-source.png` are dedicated generated
derivatives with a complete body and transparent background. Their generation
prompts are recorded alongside them. Generation is not part of normal builds.

With `npm ci`, Python 3 and Pillow installed:

```sh
python3 scripts/generate-icons.py
node scripts/qa-icons.mjs
cargo test --manifest-path src-tauri/Cargo.toml --locked icons::tests
```

The packaging script resizes and encodes committed artwork; it does not call an
image model. The lockfile-pinned Tauri CLI supplies platform icon containers.
Mobile outputs are discarded because this is a desktop app.

## Assets and consumers

| Asset | Consumer |
| --- | --- |
| `src-tauri/icons/icon.png`, `icon.icns`, `icon.ico` | Existing Tauri bundle configuration; app, Finder/Dock and executable icons |
| `src-tauri/icons/{16,32,64,128,256,512,1024}x*.png` | Matching PNG exports |
| `src-tauri/icons/tray-icon-template.{png,rgba}` | macOS tray, 36 px source for an 18 pt menu-bar image |
| `src-tauri/icons/tray-icon-template-18.{png,rgba}` | 1× template export |
| `src-tauri/icons/tray-icon-color.{png,rgba}` | Windows/Linux tray, 32 px transparent color |
| `src-tauri/icons/tray-icon-color-16.{png,rgba}` | 16 px color export |
| `src-tauri/icons/tray-icon-mono-{black,white}.{png,rgba}` | Optional 32 px monochrome exports |
| `public/favicon.png`, `public/favicon.ico`, `public/apple-touch-icon.png` | Browser metadata; both Vite HTML entries link to the new assets |
| `public/icon.png`, `public/icon-transparent.png` | Reusable 512 px color artwork |

Like `wb-switch-rust`, the native tray uses embedded raw RGBA, avoiding a runtime
PNG decoding dependency. The Rust array lengths enforce pixel dimensions at
compile time. macOS sets `icon_as_template(true)`, so the system controls tint in
light/dark/highlighted menu bars; it must not use the yellow app tile as a
template. Other platforms use the color asset. Black/white variants are exported
but this app does not add WB Switch's Windows theme detection or watcher.

The existing animated session avatars and welcome animation are separate UI
characters and remain as implemented. This change covers application branding,
tray assets and browser metadata.

## Verification boundaries

`qa-icons.mjs` checks every preview resource and writes a light/dark comparison
at `artifacts/branding/preview.html` and `preview.png`, including actual 18 px
menu-bar presentation and enlarged details. Rust tests check both platforms'
RGBA lengths, transparent corners, visible ink and antialiased edges.

The browser preview is not proof of native menu-bar appearance. Windows/Linux
runtime rendering and human visual acceptance must be reported separately from
build and asset verification.

### Verification on 2026-09-23

- TypeScript and ESLint passed.
- Node suite: 96 passed, 3 skipped, 0 failures (host run; sandbox cannot bind loopback).
- Focused native tray asset test: 1 passed, covering both platform buffers.
- Vite build, independent bundle guard and browser UI regression passed.
- Icon preview: all 14 image instances decoded; light/dark screenshots inspected.
- `npm run desktop:build -- --features diagnostics` produced the macOS app.
  Its `CFBundleIconFile` is `icon.icns`; the packaged file matches the new
  `src-tauri/icons/icon.icns` byte-for-byte.
- The isolated native app QA did not observe a runtime within its timeout. A
  development instance was already running, consistent with the single-instance
  guard redirecting the launch. The existing instance was left running. This is
  not recorded as a passed native test or native menu-bar visual acceptance.
- Windows/Linux runtime rendering was not exercised on this macOS host.
