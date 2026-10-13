// Exercise the shipped npm entry point with the compiled CLI, isolated from
// real credentials/network. Do not run postinstall or download release binaries.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

function sandbox(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wb-npm-cli-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const bin = path.join(root, 'bin');
  fs.mkdirSync(bin);
  const launcher = path.join(bin, 'workbuddy-switch.js');
  fs.copyFileSync(path.join(__dirname, '../bin/workbuddy-switch.js'), launcher);
  const executable = `wb-switch${process.platform === 'win32' ? '.exe' : ''}`;
  const compiled = process.env.WB_SWITCH_CLI_TEST_BINARY || path.join(__dirname, '../../target/debug', executable);
  assert.ok(fs.existsSync(compiled), 'Run cargo test -p wb-switch-server first, or set WB_SWITCH_CLI_TEST_BINARY');
  fs.copyFileSync(compiled, path.join(bin, `wb-switch-${process.platform}-${process.arch}${process.platform === 'win32' ? '.exe' : ''}`));
  const home = path.join(root, 'home');
  fs.mkdirSync(path.join(home, '.wb-switch'), { recursive: true });
  const name = '中文 · café · 📷 · 𠮷';
  fs.writeFileSync(path.join(home, '.wb-switch/accounts.json'), JSON.stringify([
    { id: 'npm-fixture', nickname: name, access_token: { $wbEncrypted: 1, envelope: 'fixture-secret' } },
  ]));
  const env = { ...process.env, WB_SWITCH_HOME: home };
  for (const key of ['CODEBUDDY_API_KEY', 'CODEBUDDY_AUTH_TOKEN', 'CODEBUDDY_INTERNET_ENVIRONMENT']) delete env[key];
  return { name, root, launcher, env, binPath: path.join(bin, `wb-switch-${process.platform}-${process.arch}${process.platform === 'win32' ? '.exe' : ''}`), run: (...args) => spawnSync(process.execPath, [launcher, ...args], { env, encoding: 'utf8', timeout: 15000 }) };
}

test('npm entry forwards selectors and preserves Unicode in machine output', t => {
  const fixture = sandbox(t);
  const result = fixture.run('accounts', '--variant=cn');
  assert.ifError(result.error);
  assert.equal(result.status, 0);
  assert.equal(result.stderr, '');
  const value = JSON.parse(result.stdout);
  assert.equal(value.target, 'codebuddy-cli');
  assert.equal(value.scope, 'account');
  assert.equal(value.accounts[0].nickname, fixture.name);
  assert.ok(!result.stdout.includes('fixture-secret'));
});

test('npm entry preserves error exit codes and a structured final result', t => {
  const fixture = sandbox(t);
  const result = fixture.run('switch');
  assert.ifError(result.error);
  assert.equal(result.status, 2);
  const value = JSON.parse(result.stdout);
  assert.equal(value.command, 'switch');
  assert.equal(value.exitCode, 2);
  assert.equal(value.error.code, 'INVALID_ARGUMENT');
  assert.equal(value.error.retryable, false);
});

test('npm entry supports readable text and keeps explicit JSON compatible', t => {
  const fixture = sandbox(t);
  const text = fixture.run('accounts', '--text');
  assert.equal(text.status, 0);
  assert.ok(text.stdout.includes(fixture.name));
  assert.ok(text.stdout.startsWith('ID\t'));
  const json = fixture.run('accounts', '--json');
  assert.equal(json.status, 0);
  assert.equal(JSON.parse(json.stdout).accounts[0].nickname, fixture.name);
  const conflict = fixture.run('accounts', '--text', '--json');
  assert.equal(conflict.status, 2);
  assert.equal(JSON.parse(conflict.stdout).error.code, 'INVALID_ARGUMENT');
});

test('npm entry keeps progress separate and supports ASCII JSON and partial failures', t => {
  const fixture = sandbox(t);
  const result = fixture.run('inspect', '--ascii-json');
  assert.ifError(result.error);
  assert.equal(result.status, 1);
  assert.ok([...result.stdout].every(ch => ch.charCodeAt(0) < 128));
  const value = JSON.parse(result.stdout);
  assert.equal(value.accounts[0].nickname, fixture.name);
  assert.equal(value.summary.complete, false);
  assert.equal(typeof value.accounts[0].credits.error.message, 'string');
  const progress = result.stderr.trim().split(/\r?\n/).map(line => JSON.parse(line));
  assert.ok(progress.some(event => event.type === 'progress' && event.event === 'account_completed'));
  assert.ok(!result.stderr.includes('fixture-secret'));
});

test('npm entry supports compact AI output without changing failure semantics', t => {
  const fixture = sandbox(t);
  const store = path.join(fixture.env.WB_SWITCH_HOME, '.wb-switch/accounts.json');
  const before = fs.readFileSync(store);
  const result = fixture.run('inspect', '--summary', '--ascii-json');
  assert.ifError(result.error);
  assert.equal(result.status, 1);
  const value = JSON.parse(result.stdout);
  assert.equal(value.outputMode, 'summary');
  assert.equal(value.accounts[0].name, fixture.name);
  assert.equal(value.accounts[0].credits.balanceType, 'unknown');
  assert.equal(value.accounts[0].availabilityStatus, 'unknown');
  assert.equal(typeof value.accounts[0].credits.error.code, 'string');
  assert.ok([...result.stdout].every(ch => ch.charCodeAt(0) < 128));
  assert.ok(!result.stdout.includes('fixture-secret'));
  assert.deepEqual(fs.readFileSync(store), before);
});

test('npm parent forwards Unix termination and waits for the child commit', { skip: process.platform === 'win32', timeout: 8000 }, async t => {
  const fixture = sandbox(t);
  // A fake local child models remote rotation followed by a delayed local
  // commit. It never opens account files or sends network requests.
  fs.writeFileSync(fixture.binPath, `#!/usr/bin/env node\n
    let started = false;
    process.on('SIGTERM', () => {
      if (started) return;
      started = true;
      setTimeout(() => { process.stdout.write('committed\\n'); process.exit(130); }, 250);
    });
    process.stdout.write('ready\\n');
    setTimeout(() => process.exit(10), 5000);
  `);
  fs.chmodSync(fixture.binPath, 0o755);
  const child = spawn(process.execPath, [fixture.launcher, 'credits'], { env: fixture.env, stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(() => { if (child.exitCode === null) child.kill('SIGKILL'); });
  let output = '';
  child.stdout.on('data', chunk => { output += chunk.toString(); });
  const closed = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => resolve({ code, signal }));
  });
  await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.stdout.on('data', () => { if (output.includes('ready')) resolve(); });
  });
  const started = Date.now();
  child.kill('SIGTERM');
  const result = await closed;
  assert.equal(result.code, 130);
  assert.equal(result.signal, null);
  assert.ok(Date.now() - started >= 200, 'parent must wait for the child commit');
  assert.ok(output.includes('committed'));
});
