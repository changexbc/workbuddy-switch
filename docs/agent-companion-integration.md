# Agent Companion desktop component

The desktop build includes one pinned Agent Companion source snapshot in
`vendor/agent-companion/`. Its revision and archive digest are recorded in
`vendor/agent-companion.version.json`. The Rust plugin, embedded pages, and
monitor runtime all come from that snapshot. The regular WebUI `npm run build`
does not prepare or start the native component.

## Update the pinned source

After Agent Companion changes are committed, run:

```sh
node scripts/vendor-agent-companion.mjs /path/to/agent-companion <full-commit-sha>
```

The script uses `git archive`, so local uncommitted changes do not enter the
snapshot. Commit the resulting `vendor/` files together with the manifest and
the host changes. A clean wb-switch checkout then needs no sibling repository.
The `interfaceVersion` in the manifest is the host/component compatibility
boundary; changes to native plugin commands or window behavior require a host
release.

## Build

`tauri dev` runs `npm run dev:desktop`. `tauri build` runs
`npm run build:desktop`. Both install the pinned frontend dependencies from its
lockfile and compile its runtime with `--locked`. The build copies the complete
`dist-embed/` output under `companion/`, preserving both HTML entries and their
relative assets. Tauri bundles the matching target-specific runtime as an
external binary. For cross-target builds, set `AGENT_COMPANION_TARGET` to the
same Rust triple passed to `tauri build --target`; the release workflow does
this for each platform.

The installed app contains a single wb-switch tray. The built-in component
does not create another tray or login item. The packaged files are a baseline
for a future separately updated UI/runtime; the current integration does not
download executable code at runtime.

## Public demo overlay

The read-only demo page (`npm run build:demo`) additionally embeds the
component's *web demo* build, so visitors can see the rail without installing
anything. `scripts/prepare-companion-demo.mjs` rebuilds
`vendor/agent-companion/dist-demo/` only when the pinned revision differs from
the marker written inside that directory. Its modes keep the two outputs
separate: `dev` copies the artifact to `public/companion-demo/` (gitignored, for
the Vite dev server), `build` copies nothing and drops that dev copy, `copy`
places the artifact in `dist/` after the Vite build, and `clean` removes both.
`npm run dev:demo` runs `dev` first, `npm run build:demo` runs `build` then
`copy`. Production builds (`npm run build`, and therefore `build:desktop`) run
`clean` first, so a stale dev copy can never ride into a production bundle
through `public/`.

`src/components/companion-demo-dialog.tsx` renders `companion-demo/desktop.html`
in an iframe and drives it with fictional snapshots from
`src/lib/companion-demo-script.ts` over the upstream `agent-companion-demo`
postMessage channel. The frame is the upstream rail in its demo build: no host,
no tray, no local data, and session links or the close command only report back
as `blocked`. Publish the first snapshot on the frame's `ready` message — the
iframe `load` event is too early, because the demo bridge is a dynamic import
that registers its listener after `load`, and a snapshot sent then is dropped.

Only the demo build renders the overlay and its sidebar entry
(`CompanionDemoFooter`); desktop and WebUI keep their existing behavior.
