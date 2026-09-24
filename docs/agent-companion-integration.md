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
