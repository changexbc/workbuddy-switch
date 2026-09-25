import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { BASE_URL, PLATFORMS, collect, merge, platformKeys } from '../scripts/update-manifest.mjs';
import { configure, main as configureMain } from '../scripts/configure-updater.mjs';

const version = '0.2.0';
/** One fixture payload name per platform, shaped like the bundler output. */
const payloadNames = {
  'mac-arm64': 'Agent Companion.app.tar.gz',
  'mac-x64': 'Agent Companion.app.tar.gz',
  'win-x64': `Agent Companion_${version}_x64-setup.exe`,
  'linux-x64': `Agent Companion_${version}_amd64.AppImage`,
};

let counter = 0;
function workspace() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `agent-companion-update-release-${process.pid}-${counter++}-`));
  return dir;
}

function writeBundle(root, platform, {payload = payloadNames[platform], signature = `signature-${platform}\n`, extra} = {}) {
  const bundleDir = path.join(root, `bundle-${platform}`);
  fs.mkdirSync(bundleDir, {recursive: true});
  fs.writeFileSync(path.join(bundleDir, payload), `${platform} payload`);
  if (signature !== null) fs.writeFileSync(path.join(bundleDir, `${payload}.sig`), signature);
  if (extra) fs.writeFileSync(path.join(bundleDir, extra), 'stale');
  return bundleDir;
}

/** Runs the real `collect` for every platform, the way the CI matrix does. */
function collectAll(root, {version: releaseVersion = version, baseUrl = BASE_URL, artifacts} = {}) {
  const outDir = artifacts ?? path.join(root, 'artifacts');
  for (const platform of Object.keys(PLATFORMS)) {
    collect({platform, bundleDir: writeBundle(root, platform), version: releaseVersion, outDir, baseUrl, now: () => new Date('2026-09-24T00:00:00Z')});
  }
  return outDir;
}

test('collect writes one renamed payload per platform and merge assembles latest.json', () => {
  const root = workspace();
  const outDir = collectAll(root);

  for (const [platform, spec] of Object.entries(PLATFORMS)) {
    const asset = path.join(outDir, spec.asset);
    assert.ok(fs.existsSync(asset), `${platform} 应产出 ${spec.asset}`);
    assert.equal(fs.readFileSync(`${asset}.sig`, 'utf8'), `signature-${platform}\n`);
    const fragment = JSON.parse(fs.readFileSync(path.join(outDir, `update-fragment-${platform}.json`), 'utf8'));
    assert.equal(fragment.version, version);
    assert.deepEqual(Object.keys(fragment.platforms).sort(), [...spec.keys].sort());
  }

  const result = merge({dir: outDir, version});
  assert.equal(result.manifestPath, path.join(outDir, 'latest.json'));
  const manifest = JSON.parse(fs.readFileSync(result.manifestPath, 'utf8'));
  assert.deepEqual(Object.keys(manifest.platforms).sort(), [...platformKeys()].sort());
  assert.equal(manifest.version, version);
  assert.equal(manifest.pub_date, '2026-09-24T00:00:00Z');
  assert.deepEqual(manifest.platforms['darwin-aarch64'], {signature: 'signature-mac-arm64', url: `${BASE_URL}/Agent-Companion_macos_aarch64.app.tar.gz`});
  assert.deepEqual(manifest.platforms['darwin-x86_64'], {signature: 'signature-mac-x64', url: `${BASE_URL}/Agent-Companion_macos_x86_64.app.tar.gz`});
  assert.deepEqual(manifest.platforms['linux-x86_64'], {signature: 'signature-linux-x64', url: `${BASE_URL}/Agent-Companion_linux_x86_64.AppImage`});
  assert.deepEqual(manifest.platforms['windows-x86_64-nsis'], {signature: 'signature-win-x64', url: `${BASE_URL}/Agent-Companion_windows_x86_64-setup.exe`});
  assert.deepEqual(manifest.platforms['windows-x86_64'], manifest.platforms['windows-x86_64-nsis']);
  fs.rmSync(root, {recursive: true, force: true});
});

test('collect insists on exactly one signed payload and a usable version', () => {
  const root = workspace();
  assert.throws(() => collect({platform: 'mac-arm64', bundleDir: writeBundle(root, 'mac-arm64', {signature: null}), version, outDir: path.join(root, 'out')}), /缺少更新包签名/);
  assert.throws(() => collect({platform: 'mac-arm64', bundleDir: writeBundle(root, 'mac-arm64', {extra: 'Agent Companion (old).app.tar.gz'}), version, outDir: path.join(root, 'out')}), /期望恰好 1 个更新包/);
  assert.throws(() => collect({platform: 'mac-arm64', bundleDir: root, version: 'v0.2', outDir: path.join(root, 'out')}), /版本号无效/);
  assert.throws(() => collect({platform: 'solaris', bundleDir: root, version, outDir: path.join(root, 'out')}), /未知平台/);
  assert.throws(() => collect({platform: 'mac-arm64', bundleDir: path.join(root, 'missing'), version, outDir: path.join(root, 'out')}), /打包目录不存在/);
  fs.rmSync(root, {recursive: true, force: true});
});

test('merge refuses incomplete, mismatched or duplicated release data', () => {
  const missing = workspace();
  const partial = collectAll(missing);
  fs.rmSync(path.join(partial, 'update-fragment-linux-x64.json'));
  assert.throws(() => merge({dir: partial, version}), /缺少平台片段：linux-x64/);
  fs.rmSync(missing, {recursive: true, force: true});

  const mismatched = workspace();
  const wrongVersion = collectAll(mismatched);
  assert.throws(() => merge({dir: wrongVersion, version: '0.3.0'}), /片段版本 0.2.0 与发布版本 0.3.0 不一致/);
  fs.rmSync(mismatched, {recursive: true, force: true});

  const foreign = workspace();
  const foreignBase = collectAll(foreign, {baseUrl: 'https://example.invalid/download'});
  assert.throws(() => merge({dir: foreignBase, version}), /下载地址必须以 .* 开头/);
  fs.rmSync(foreign, {recursive: true, force: true});

  const duplicated = workspace();
  const sameSignature = collectAll(duplicated);
  for (const platform of ['mac-x64', 'linux-x64']) {
    const file = path.join(sameSignature, `update-fragment-${platform}.json`);
    const fragment = JSON.parse(fs.readFileSync(file, 'utf8'));
    for (const key of Object.keys(fragment.platforms)) fragment.platforms[key].signature = 'signature-mac-arm64';
    fs.writeFileSync(file, JSON.stringify(fragment));
  }
  assert.throws(() => merge({dir: sameSignature, version}), /签名重复/);
  fs.rmSync(duplicated, {recursive: true, force: true});

  // Windows 两个平台键指向同一安装包却写了不同签名（片段被伪造或拼接）：
  // 只看每个平台的第一个键会漏掉它，必须逐个键校验。
  const splitWindows = workspace();
  const splitWindowsDir = collectAll(splitWindows);
  const winFragmentFile = path.join(splitWindowsDir, 'update-fragment-win-x64.json');
  const winFragment = JSON.parse(fs.readFileSync(winFragmentFile, 'utf8'));
  winFragment.platforms['windows-x86_64'].signature = 'signature-win-x64-other';
  fs.writeFileSync(winFragmentFile, JSON.stringify(winFragment));
  assert.throws(() => merge({dir: splitWindowsDir, version}), /签名与其他键不一致/);
  fs.rmSync(splitWindows, {recursive: true, force: true});

  const missingAsset = workspace();
  const withoutAsset = collectAll(missingAsset);
  fs.rmSync(path.join(withoutAsset, PLATFORMS['win-x64'].asset));
  assert.throws(() => merge({dir: withoutAsset, version}), /产物目录缺少 Agent-Companion_windows_x86_64-setup.exe/);
  fs.rmSync(missingAsset, {recursive: true, force: true});

  const tampered = workspace();
  const wrongKeys = collectAll(tampered);
  const windowsFragment = path.join(wrongKeys, 'update-fragment-win-x64.json');
  const fragment = JSON.parse(fs.readFileSync(windowsFragment, 'utf8'));
  delete fragment.platforms['windows-x86_64'];
  fs.writeFileSync(windowsFragment, JSON.stringify(fragment));
  assert.throws(() => merge({dir: wrongKeys, version}), /平台键 windows-x86_64-nsis 与期望/);
  fs.rmSync(tampered, {recursive: true, force: true});

  assert.throws(() => merge({dir: path.join(os.tmpdir(), 'agent-companion-does-not-exist'), version}), /产物目录不存在/);
});

test('configure-updater gates tag builds and patches the config for CI', () => {
  const root = workspace();
  const configPath = path.join(root, 'tauri.conf.json');
  const fixture = {
    productName: 'Agent Companion',
    bundle: {active: true, createUpdaterArtifacts: true},
    plugins: {updater: {endpoints: [`${BASE_URL}/latest.json`], pubkey: ''}},
  };
  const writeFixture = () => fs.writeFileSync(configPath, `${JSON.stringify(fixture, null, 2)}\n`);
  const read = () => JSON.parse(fs.readFileSync(configPath, 'utf8'));
  const publicKey = Buffer.from('untrusted comment: minisign public key: F04E8FD98FB7AFD\nRWQ9evuY/egEDwUnTZNb558bFwSf1XZXrSHGg5TRHG0yLVGNS7HvZyhK\n').toString('base64');

  writeFixture();
  assert.throws(() => configure({configPath, pubkey: '', requireSigning: true}), /标签构建已停止/);
  assert.equal(read().bundle.createUpdaterArtifacts, true, '标签构建失败后不得留下半成品配置');

  const disabled = configure({configPath, pubkey: '   ', requireSigning: false});
  assert.equal(disabled.signing, 'disabled');
  assert.equal(read().bundle.createUpdaterArtifacts, false);
  assert.equal(read().plugins.updater.pubkey, '');

  writeFixture();
  const enabled = configure({configPath, pubkey: publicKey, requireSigning: true});
  assert.equal(enabled.signing, 'enabled');
  assert.match(enabled.fingerprint, /^[0-9a-f]{16}$/);
  assert.equal(read().plugins.updater.pubkey, publicKey);
  assert.equal(read().bundle.createUpdaterArtifacts, true);

  assert.throws(() => configure({configPath, pubkey: 'not-a-key', requireSigning: true}), /不是 minisign 公钥/);

  fs.writeFileSync(configPath, JSON.stringify({plugins: {updater: {endpoints: ['https://example.invalid/latest.json']}}}));
  assert.throws(() => configure({configPath, pubkey: publicKey, requireSigning: true}), /endpoints 必须包含/);

  writeFixture();
  const outputFile = path.join(root, 'github-output');
  fs.writeFileSync(outputFile, '');
  const env = {TAURI_UPDATER_PUBKEY: publicKey, GITHUB_OUTPUT: outputFile};
  const logs = [];
  assert.equal(configureMain(['--config', configPath, '--require-signing'], env, {write: chunk => logs.push(chunk)}), 0);
  assert.equal(fs.readFileSync(outputFile, 'utf8'), 'signing=enabled\n');
  assert.match(logs.join(''), /公钥指纹 sha256:[0-9a-f]{16}/);
  fs.rmSync(root, {recursive: true, force: true});
});
