# Development guide

**English** | [한국어](./DEVELOPMENT.ko.md)

## Requirements

Node.js 20 or later, a stable Rust toolchain, and macOS (or Windows / Linux).

## Commands

```bash
npm install
npm run tauri dev          # development mode
npm run build:app          # build a debug .app with the frontend asset patches applied
npm run build:app:release  # build a release .app and a signed update package
```

## Release a new version

The signing key for auto-update lives at `~/.wb-switch/wb-switch-updater.key`. The build scripts read it through `TAURI_SIGNING_PRIVATE_KEY`.

1. Run `npm run build:app:release`. It writes `.app.tar.gz` and `.sig`. A Windows NSIS build also writes `*_x64-setup.exe` and `.exe.sig` for the current version. CI deletes `target/**/release/bundle` first, so a stale installer in the cargo cache cannot reach the release.
2. macOS: run `UPDATE_OS=macos UPDATE_ARCH=aarch64 sh scripts/gen-update-json.sh` to write `latest-macos-aarch64.json`. For Intel, set `UPDATE_ARCH=x86_64`.
3. Windows: run `UPDATE_OS=windows UPDATE_ARCH=x86_64 sh scripts/gen-update-json.sh` to write `latest-windows-x86_64.json`.
4. Run `python3 scripts/merge-update-manifests.py <output-dir>` to merge them into `latest.json`. The script also copies the Windows entry into `latest-macos-x86_64.json`, so Windows clients that are already installed keep updating.
5. Upload the installers, the signed update packages, and `latest*.json` to the GitHub Release.

### Release the npm version (webui)

1. Compile the server binary and upload it to the GitHub Release. `.github/workflows/build.yml` does this for you.
2. Run `cd npm && npm publish`. The package is `workbuddy-switch`, and its postinstall step downloads the binary for the current platform from the release.

## Directory layout

```
src-tauri/
  src/
    commands.rs      # thin Tauri command wrappers, mirroring the old Python HTTP API
crates/
  wb-switch-core/    # core logic, one module per concern (account, auth_file, oauth,
                     # process, switch, session, checkin, refresh, update, config, ...)
  wb-switch-server/  # HTTP server and CLI: axum API plus a rust-embed frontend
src/                 # frontend: components, pages, lib, stores
                     # lib/api.ts has two channels, Tauri invoke and HTTP fetch
npm/                 # npm package: package.json, bin, scripts/install.js
```

## Translations

English is the default language and Korean is the second. The app has two catalogs, one per process boundary.

The webview catalog lives in `src/lib/i18n/messages/`. Each surface owns one pair of files, `en.ts` and `ko.ts`. The English file defines the key set, so a misspelled key fails the type check. The Korean file is partial and falls back to English, and `messages.test.ts` fails when a key or a placeholder is missing. Components read strings through `useT()` from `src/lib/i18n`; plain modules call `translate()` from `src/lib/i18n/translate`.

The Rust catalog is `crates/wb-switch-core/src/modules/i18n.rs`, which covers what the host draws outside the webview: tray menu labels, notifications, and the messages the backend returns to the interface. Call sites pass both languages, as in `i18n::tr("Open main window", "주 창 열기")`, so a missing translation is a compile error rather than a runtime fallback. The frontend reports the selected language through the `set_locale` command, which also stores it in `~/.wb-switch/locale` so the tray renders in the right language on the next start.

`index.html` keeps its own copy of the first-paint and startup-failure strings, because that inline script runs before the bundle.

## Privacy notes

- Do not commit local data. `.gitignore` excludes `accounts.json`, authentication files, keys, and tokens.
- Before a release, scan for token patterns with `git grep`. Look for `ghp_`, `npm_`, `gho_`, and similar prefixes.
