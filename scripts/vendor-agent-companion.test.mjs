import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

test('vendoring replays host patches inside a repository and preserves the snapshot on conflict', async () => {
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'companion-vendor-test-'));
  const root = path.join(temp, 'host');
  const source = path.join(temp, 'upstream');
  const git = (cwd, args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  const commit = () => {
    git(source, ['add', 'rail.txt']);
    git(source, ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'fixture']);
    return git(source, ['rev-parse', 'HEAD']).trim();
  };
  try {
    await fs.mkdir(path.join(root, 'scripts'), { recursive: true });
    await fs.mkdir(path.join(root, 'vendor/agent-companion'), { recursive: true });
    await fs.mkdir(path.join(root, 'patches/agent-companion'), { recursive: true });
    await fs.mkdir(source);
    // The host is a Git checkout: applying from a nested staging directory
    // without --directory can silently skip every patch path.
    git(root, ['init']);
    git(source, ['init']);
    await fs.copyFile(new URL('./vendor-agent-companion.mjs', import.meta.url), path.join(root, 'scripts/vendor-agent-companion.mjs'));
    await fs.writeFile(path.join(source, 'rail.txt'), 'upstream\n');
    const revision = commit();
    await fs.writeFile(path.join(source, 'rail.txt'), 'uncommitted change\n');
    const patch = 'diff --git a/rail.txt b/rail.txt\n--- a/rail.txt\n+++ b/rail.txt\n@@ -1 +1 @@\n-upstream\n+host behavior\n';
    await fs.writeFile(path.join(root, 'patches/agent-companion/host-compatibility.patch'), patch);
    const run = (ref) => spawnSync(process.execPath, ['scripts/vendor-agent-companion.mjs', source, ref], { cwd: root, encoding: 'utf8' });
    const first = run(revision);
    assert.equal(first.status, 0, first.stderr);
    const snapshot = path.join(root, 'vendor/agent-companion/rail.txt');
    assert.equal(await fs.readFile(snapshot, 'utf8'), 'host behavior\n');
    const manifestPath = path.join(root, 'vendor/agent-companion.version.json');
    const manifestText = await fs.readFile(manifestPath, 'utf8');
    const manifest = JSON.parse(manifestText);
    assert.equal(manifest.revision, revision);
    assert.equal(manifest.patches[0].sha256, createHash('sha256').update(patch).digest('hex'));
    assert.equal(run(revision).status, 0);
    assert.equal(await fs.readFile(manifestPath, 'utf8'), manifestText);

    await fs.writeFile(path.join(source, 'rail.txt'), 'conflicting upstream\n');
    const conflictingRevision = commit();
    const failed = run(conflictingRevision);
    assert.notEqual(failed.status, 0);
    assert.match(failed.stderr, /patch does not apply/);
    assert.equal(await fs.readFile(snapshot, 'utf8'), 'host behavior\n');
    assert.equal(await fs.readFile(manifestPath, 'utf8'), manifestText);
    assert.deepEqual(await fs.readdir(path.join(root, 'vendor')), ['agent-companion', 'agent-companion.version.json']);
  } finally {
    await fs.rm(temp, { recursive: true, force: true });
  }
});
