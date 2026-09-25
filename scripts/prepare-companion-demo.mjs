import fs from 'node:fs/promises';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

// The public demo page embeds the upstream rail as a static web build. The
// artifact follows vendoring automatically: the pinned revision is the only
// input, so a new snapshot rebuilds once and repeated local builds are no-ops.
//
// Modes:
//   dev   — build when stale, then copy into `public/` (Vite serves it in dev)
//   build — build when stale and drop the dev copy; `vite build` empties `dist/`,
//           so the demo build copies into `dist/` afterwards with `copy`
//   copy  — copy into `dist/` (run after `vite build`, which wipes `dist/`)
//   clean — remove both copies, so a production build can never pick the demo
//           artifact up through `public/`
const MODES = ['dev', 'build', 'copy', 'clean'];
const args = process.argv.slice(2);
const force = args.includes('--force');
const positional = args.filter((arg) => arg !== '--force');
const mode = positional[0] ?? 'dev';
if (positional.length > 1 || !MODES.includes(mode)) {
  throw new Error(`Usage: node scripts/prepare-companion-demo.mjs <${MODES.join('|')}> [--force]`);
}

const root = path.resolve(import.meta.dirname, '..');
const companion = path.join(root, 'vendor/agent-companion');
const output = path.join(companion, 'dist-demo');
const devDestination = path.join(root, 'public/companion-demo');
const buildDestination = path.join(root, 'dist/companion-demo');

if (mode === 'clean') {
  await fs.rm(devDestination, { recursive: true, force: true });
  await fs.rm(buildDestination, { recursive: true, force: true });
  console.log('Removed public/companion-demo/ and dist/companion-demo/');
  process.exit(0);
}

const version = JSON.parse(await fs.readFile(path.join(root, 'vendor/agent-companion.version.json'), 'utf8'));
if (!/^[0-9a-f]{40}$/.test(version.revision)) {
  throw new Error('Invalid pinned Agent Companion version manifest');
}

if (mode !== 'copy') {
  const marker = path.join(output, '.revision');
  const built = await fs.readFile(marker, 'utf8').catch(() => '');
  if (force || built.trim() !== version.revision) {
    console.log(`Building Agent Companion demo assets at ${version.revision}`);
    // The archived source carries its own lockfile. `--base=./` overrides the
    // upstream Pages subpath so the frame works under any hosting prefix.
    execFileSync('npm', ['ci'], { cwd: companion, stdio: 'inherit' });
    execFileSync('npm', ['run', 'build:demo', '--', '--base=./'], { cwd: companion, stdio: 'inherit' });
    await fs.writeFile(marker, `${version.revision}\n`);
  } else {
    console.log(`Agent Companion demo assets already built at ${version.revision}`);
  }
}

// Fail fast on a renamed or missing entry instead of publishing an empty site.
for (const entry of ['desktop.html', 'assets']) {
  await fs.access(path.join(output, entry));
}

if (mode === 'build') {
  // `vite build` empties `dist/` right afterwards, so this mode only prepares the
  // artifact and drops the dev copy; `copy` places it in `dist/`. That keeps the
  // demo output single-sourced instead of letting the stale `public/` copy ride along.
  await fs.rm(devDestination, { recursive: true, force: true });
  console.log(`Agent Companion demo assets ready at ${path.relative(root, output)}/`);
} else {
  const destination = mode === 'dev' ? devDestination : buildDestination;
  await fs.rm(destination, { recursive: true, force: true });
  await fs.mkdir(path.dirname(destination), { recursive: true });
  await fs.cp(output, destination, { recursive: true });
  console.log(`Agent Companion demo assets copied to ${path.relative(root, destination)}/`);
}
