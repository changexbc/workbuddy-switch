# Agent Companion desktop component

The desktop build includes the Agent Companion source in
`vendor/agent-companion/`. The component is maintained **in this repository**:
there is no upstream fetch, no vendoring script and no sibling checkout. The
Rust plugin, embedded pages and monitor runtime are all built from that
directory. The regular WebUI `npm run build` does not prepare or start the
native component.

`vendor/agent-companion.version.json` only records the base revision this copy
started from and the host/component `interfaceVersion`. It is provenance, not an
update input; changes to native plugin commands or window behavior still require
a host release.

## Changes maintained here

This copy is not pristine: wb-switch patches it so the component works on
Windows. Edit the files in place; nothing overwrites them later.

- `crates/agent-studio-runtime/src/main.rs`: hook commands on Windows use double
  quotes and forward slashes (`"C:/…"`). CodeBuddy IDE and the VS Code plugin run
  hooks through `cmd.exe`, which treats `'` literally, so the previous
  single-quote spelling never executed.
- `crates/agent-studio-runtime/src/integrations.rs`: installing replaces those
  legacy single-quote entries instead of appending a second command beside them.
- `crates/agent-studio-core/src/host_process.rs`: Windows host presence comes
  from the process snapshot (image names), so a force-quit VS Code/IDE still
  ends its unfinished sessions as `aborted`.
- `crates/agent-studio-desktop/src/session.rs`: a VS Code plugin session opens
  its window folder through `Code.exe` (PATH shim → standard install dirs → App
  Paths registry); the plugin history root is
  `%APPDATA%\Code\User\globalStorage\tencent-cloud.coding-copilot\genie-history`.
- `src/monitor/session-link.ts` and `collector/lib/host-process.js`: drive paths
  keep their `D:` prefix instead of becoming `/D:/…`, and the Node collector
  mirrors the Windows process probe.

## Build

`tauri dev` runs `npm run dev:desktop`. `tauri build` runs
`npm run build:desktop`. Both install the component's frontend dependencies from
its lockfile and compile its runtime with `--locked`. The build copies the complete
`dist-embed/` output under `companion/`, preserving both HTML entries and their
relative assets. Tauri bundles the matching target-specific runtime as an
external binary. For cross-target builds, set `AGENT_COMPANION_TARGET` to the
same Rust triple passed to `tauri build --target`; the release workflow does
this for each platform.

The installed app contains a single wb-switch tray. The built-in component
does not create another tray or login item. The packaged files are versioned
with the host app; the integration does not download executable code at runtime.

## Public demo overlay

The read-only demo page (`npm run build:demo`) additionally embeds the
component's *web demo* build, so visitors can see the rail without installing
anything. `scripts/prepare-companion-demo.mjs` rebuilds
`vendor/agent-companion/dist-demo/` only when the source fingerprint differs
from the marker written inside that directory. Its modes keep the two outputs
separate: `dev` copies the artifact to `public/companion-demo/` (gitignored, for
the Vite dev server), `build` copies nothing and drops that dev copy, `copy`
places the artifact in `dist/` after the Vite build, and `clean` removes both.
`npm run dev:demo` runs `dev` first, `npm run build:demo` runs `build` then
`copy`. Production builds (`npm run build`, and therefore `build:desktop`) run
`clean` first, so a stale dev copy can never ride into a production bundle
through `public/`.

`src/components/companion-demo-dialog.tsx` renders `companion-demo/desktop.html`
in an iframe and drives it with fictional snapshots from
`src/lib/companion-demo-script.ts` over the component's `agent-companion-demo`
postMessage channel. The frame is the bundled rail in its demo build: no host,
no tray, no local data, and session links or the close command only report back
as `blocked`. Publish the first snapshot on the frame's `ready` message — the
iframe `load` event is too early, because the demo bridge is a dynamic import
that registers its listener after `load`, and a snapshot sent then is dropped.

Only the demo build renders the overlay and its sidebar entry
(`CompanionDemoFooter`); desktop and WebUI keep their existing behavior.
