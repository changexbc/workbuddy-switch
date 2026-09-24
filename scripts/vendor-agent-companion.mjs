import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const [source, ref] = process.argv.slice(2);
if (!source || !ref) {
  throw new Error('Usage: node scripts/vendor-agent-companion.mjs <source-checkout> <commit>');
}
const root = path.resolve(import.meta.dirname, '..');
const checkout = path.resolve(source);
const revision = execFileSync('git', ['-C', checkout, 'rev-parse', '--verify', `${ref}^{commit}`], { encoding: 'utf8' }).trim();
const archive = path.join(os.tmpdir(), `agent-companion-${revision}-${process.pid}.tar`);
const destination = path.join(root, 'vendor/agent-companion');

try {
  // git archive includes only committed files. The build never reads the
  // checkout, so unrelated local edits cannot enter the bundled component.
  execFileSync('git', ['-C', checkout, 'archive', '--format=tar', '-o', archive, revision], { stdio: 'inherit' });
  const digest = createHash('sha256').update(await fs.readFile(archive)).digest('hex');
  await fs.rm(destination, { recursive: true, force: true });
  await fs.mkdir(destination, { recursive: true });
  execFileSync('tar', ['-xf', archive, '-C', destination], { stdio: 'inherit' });
  await fs.writeFile(path.join(root, 'vendor/agent-companion.version.json'), `${JSON.stringify({ revision, archiveSha256: digest, interfaceVersion: 1 }, null, 2)}\n`);
  console.log(`Vendored Agent Companion ${revision} (archive SHA-256 ${digest})`);
} finally {
  await fs.rm(archive, { force: true });
}
