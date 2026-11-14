# workbuddy-switch

**English** | [한국어](./README.ko.md)

An account switcher for WorkBuddy, CodeBuddy IDE, CodeBuddy CLI and the VS Code CodeBuddy extension, running in the browser (webui). All four support both the Domestic and International editions, and the app adds credit-expiry and token-usage monitoring. A desktop app (Tauri) is also available.

Multiple accounts share one login state, and the WorkBuddy login account switches with a single click. **Session copy** duplicates the current account's sessions under new ids for a target account — the source account's data is untouched and the copies belong to the target account in the cloud.

**Live demo**: [Open the GitHub Pages demo](https://changexbc.github.io/workbuddy-switch/) (read-only; accounts, credits and request records are fictional, all business actions are disabled)

## Quick Start

### npm install (webui)

```bash
npm i -g workbuddy-switch
workbuddy-switch              # start the local server and open the browser
workbuddy-switch status       # show the current account in the terminal
```

The webui matches the desktop app and covers every module below.

### Desktop app

Download the macOS / Windows / Linux installer from [GitHub Releases](https://github.com/changexbc/workbuddy-switch/releases/latest) (Tauri; recommended for daily use).

> **macOS says "is damaged and can't be opened"?** An unsigned app triggers the quarantine mechanism; run this once in a terminal:
>
> ```bash
> xattr -rd com.apple.quarantine "/Applications/workbuddy-switch.app"
> ```

If the app launches but switching accounts reports a permission error, see [macOS Permissions](#macos-permissions) below.

## Features

| Module | Description |
| --- | --- |
| Account management | OAuth QR-code sign-in, import/export accounts, delete accounts |
| Account switching | One-click switch of the WorkBuddy login account, with live progress |
| Session copy | Copy the sessions selected on the current account to a target account; the source account is untouched |
| Credit expiry | Automatically queries each account's remaining credits and expiry date; anything expiring within 7 days is highlighted, sorted by urgency and marked "use first" |
| Credit statistics | Aggregates official request usage: overview, 30-day trend, model breakdown, per-account consumption and request details |
| Token statistics | Token overview and trends by source, including composition share, activity heatmap, project/model Top 10 and session ranking |
| CodeBuddy CLI | Shares the same account library as WorkBuddy with an independent default account; changes take effect immediately, no CLI restart required |
| CodeBuddy IDE | Switch the CodeBuddy IDE desktop client account; independent of CodeBuddy CLI |
| VS Code CodeBuddy extension | Switch the CodeBuddy extension account inside VS Code; VS Code can be closed automatically and reopened after the write |
| Extension session copy | When switching extension accounts, optionally copy the current extension account's sessions to the target (additive; the source stays unchanged) |
| Auto rotation | In the background, sets the account with the most urgent credits as the next CodeBuddy CLI launch account; skipped while a CLI session is running |
| Auto update | Checks GitHub Releases for new versions; full-package updates are signature-verified |
| Permission check | macOS authorization guide (App Management / Full Disk Access drag-to-authorize, plus automatic detection) |

## Usage

1. **Add and export accounts**: Accounts page → "OAuth QR sign-in", "Import backup"; "Export" backs up the selected accounts as JSON
2. **Switch accounts and edit account info**: Account card → "Switch", with an option to copy the current session; "Account info" lets you add a note and choose whether the card shows the account name, phone number or note
3. **Check credits and statistics**: The accounts page queries credit expiry automatically; click "Refresh credits" to update manually. Open "Credit statistics" and "Token statistics" from the sidebar for usage details
4. **Switch accounts for each client**: CodeBuddy CLI, CodeBuddy IDE and the VS Code CodeBuddy extension can all be switched from the account card; the VS Code extension supports copying the current account's sessions in the dialog. CodeBuddy IDE must be opened and signed in manually once before first use
5. **Auto rotation**: Settings → CodeBuddy CLI auto rotation; when enabled, the default account is set automatically by credit urgency
6. **Updates**: Settings → auto update checks the public GitHub Releases source; the npm version can also be upgraded with `npm update -g workbuddy-switch`

## macOS Permissions

Switching accounts writes WorkBuddy's authentication files, which macOS gates behind "App Management" (or "Full Disk Access"):

1. When the first switch reports "no permission", click "Open System Settings"
2. Prefer enabling workbuddy-switch under **App Management**; if it is not listed there, drag workbuddy-switch into the arrowed box under **Full Disk Access**
3. Restart the app after granting; Settings → "Permission check" verifies at any time

> webui mode: permissions follow the terminal process that started the server; if that terminal already has Full Disk Access, nothing else is needed.

## License

[MIT](./LICENSE)
