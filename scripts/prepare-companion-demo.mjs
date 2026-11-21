import fs from 'node:fs/promises';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

// The public demo page embeds the bundled rail as a static web build. The
// component is maintained in this repository, so the rebuild marker is a
// fingerprint of its sources: any edit under `vendor/agent-companion/`
// invalidates the cache, and repeated builds without changes are no-ops.
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

// Generated or installed trees never affect the demo build itself.
const SKIP = new Set(['node_modules', 'target', 'dist', 'dist-embed', 'dist-demo', '.git']);

/** Deterministic digest of every source file the demo build reads. */
async function sourceFingerprint(dir) {
  const files = [];
  async function walk(current) {
    for (const entry of await fs.readdir(current, { withFileTypes: true })) {
      if (SKIP.has(entry.name)) continue;
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) {
        await walk(full);
      } else if (entry.isFile()) {
        const digest = createHash('sha256').update(await fs.readFile(full)).digest('hex');
        files.push(`${path.relative(dir, full).split(path.sep).join('/')}:${digest}`);
      }
    }
  }
  await walk(dir);
  files.sort();
  return createHash('sha256').update(files.join('\n')).digest('hex');
}

if (mode !== 'copy') {
  const marker = path.join(output, '.source-fingerprint');
  const built = await fs.readFile(marker, 'utf8').catch(() => '');
  const fingerprint = await sourceFingerprint(companion);
  if (force || built.trim() !== fingerprint) {
    console.log(`Building Agent Companion demo assets (${fingerprint.slice(0, 12)})`);
    // `--base=./` keeps every asset relative, so the frame works under any
    // hosting prefix. Windows 的 npm 是 .cmd，Node 20+ 禁止直接 spawn（EINVAL），
    // 必须经 shell 解析。
    const shell = process.platform === 'win32';
    execFileSync('npm', ['ci'], { cwd: companion, stdio: 'inherit', shell });
    execFileSync('npm', ['run', 'build:demo', '--', '--base=./'], { cwd: companion, stdio: 'inherit', shell });
    await fs.writeFile(marker, `${fingerprint}\n`);
  } else {
    console.log(`Agent Companion demo assets already built (${fingerprint.slice(0, 12)})`);
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
