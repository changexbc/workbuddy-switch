#!/usr/bin/env node
/**
 * Updater payload collection and `latest.json` assembly for the standalone app.
 *
 * One owner for the platform mapping, the asset names and the manifest shape, so
 * the CI matrix, the merge job and this repository cannot drift apart:
 *
 *   collect  one matrix job: assert exactly one signed payload, copy it under its
 *            release asset name, write this platform's fragment
 *   merge    release job: require every platform fragment, validate the whole
 *            manifest and write `latest.json`
 *
 * The manifest is a static GitHub Release asset; every URL points at an asset of
 * the release that will carry it (`/releases/latest/download/<asset>` resolves to
 * the newest published release, so a draft release never serves a half-built
 * manifest).
 */
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { parseArgs } from 'node:util';
import { pathToFileURL } from 'node:url';

export const OWNER = 'changexbc';
export const REPO = 'agent-companion';
export const BASE_URL = `https://github.com/${OWNER}/${REPO}/releases/latest/download`;
export const VERSION_PATTERN = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

/**
 * macOS ships `.app.tar.gz`, Windows the signed NSIS installer and Linux the
 * signed AppImage; `.deb` stays a manual installer. Windows needs both platform
 * keys: the updater asks for `windows-x86_64-nsis` first and falls back to
 * `windows-x86_64` when the bundle type cannot be detected.
 */
export const PLATFORMS = {
  'mac-arm64': {asset: 'Agent-Companion_macos_aarch64.app.tar.gz', payload: '*.app.tar.gz', keys: ['darwin-aarch64']},
  'mac-x64': {asset: 'Agent-Companion_macos_x86_64.app.tar.gz', payload: '*.app.tar.gz', keys: ['darwin-x86_64']},
  'win-x64': {asset: 'Agent-Companion_windows_x86_64-setup.exe', payload: '*-setup.exe', keys: ['windows-x86_64-nsis', 'windows-x86_64']},
  'linux-x64': {asset: 'Agent-Companion_linux_x86_64.AppImage', payload: '*.AppImage', keys: ['linux-x86_64']},
};

export function platformKeys(platforms = PLATFORMS) {
  return Object.values(platforms).flatMap(spec => spec.keys);
}

function matches(name, pattern) {
  return new RegExp(`^${pattern.split('*').map(part => part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('.*')}$`).test(name);
}

function requireVersion(version) {
  if (!VERSION_PATTERN.test(version ?? '')) throw new Error(`版本号无效：${JSON.stringify(version)}（期望 v1.2.3 或 1.2.3）`);
  return version;
}

function readManifest(version) {
  return {version, notes: '', pub_date: '', platforms: {}};
}

/** `collect`: assert one signed payload for this platform and emit its fragment. */
export function collect({platform, bundleDir, version, outDir, baseUrl = BASE_URL, now = () => new Date()}) {
  const spec = PLATFORMS[platform];
  if (!spec) throw new Error(`未知平台：${platform}（可用：${Object.keys(PLATFORMS).join(', ')}）`);
  requireVersion(version);
  if (!fs.existsSync(bundleDir)) throw new Error(`打包目录不存在：${bundleDir}`);

  const candidates = fs.readdirSync(bundleDir, {withFileTypes: true})
    .filter(entry => entry.isFile() && matches(entry.name, spec.payload))
    .map(entry => entry.name);
  if (candidates.length !== 1) {
    throw new Error(`${platform}: 期望恰好 1 个更新包（${spec.payload}），实际 ${candidates.length} 个：${candidates.join(', ') || '（无）'}`);
  }
  const payload = path.join(bundleDir, candidates[0]);
  const signatureFile = `${payload}.sig`;
  if (!fs.existsSync(signatureFile)) throw new Error(`${platform}: 缺少更新包签名 ${path.basename(signatureFile)}`);
  const signature = fs.readFileSync(signatureFile, 'utf8').trim();
  if (!signature) throw new Error(`${platform}: 更新包签名为空 ${path.basename(signatureFile)}`);

  fs.mkdirSync(outDir, {recursive: true});
  // 更新包从打包目录移入发布资产名：Windows 安装包与 Linux AppImage 本身就是更新
  // 包，移动避免同一份产物在 Release 里出现两次。
  const assetPath = path.join(outDir, spec.asset);
  if (path.resolve(assetPath) !== path.resolve(payload)) fs.renameSync(payload, assetPath);
  fs.writeFileSync(`${assetPath}.sig`, `${signature}\n`);

  const url = `${baseUrl}/${spec.asset}`;
  const fragment = {
    platform,
    version,
    pubDate: now().toISOString().replace(/\.\d{3}Z$/, 'Z'),
    platforms: Object.fromEntries(spec.keys.map(key => [key, {url, signature}])),
  };
  const fragmentPath = path.join(outDir, `update-fragment-${platform}.json`);
  fs.writeFileSync(fragmentPath, `${JSON.stringify(fragment, null, 2)}\n`);
  return {fragmentPath, asset: assetPath, payload: candidates[0], keys: spec.keys, url};
}

function loadFragments(dir) {
  if (!fs.existsSync(dir)) throw new Error(`产物目录不存在：${dir}`);
  return fs.readdirSync(dir)
    .filter(name => /^update-fragment-.+\.json$/.test(name))
    .map(name => {
      const file = path.join(dir, name);
      let fragment;
      try {
        fragment = JSON.parse(fs.readFileSync(file, 'utf8'));
      } catch (error) {
        throw new Error(`无法解析 ${name}：${error.message}`);
      }
      if (!fragment || typeof fragment !== 'object' || typeof fragment.platform !== 'string') throw new Error(`${name} 不是有效的平台片段`);
      return {file: name, ...fragment};
    });
}

/** `merge`: combine every platform fragment into one validated `latest.json`. */
export function merge({dir, version, baseUrl = BASE_URL}) {
  requireVersion(version);
  const fragments = loadFragments(dir);
  const byPlatform = new Map(fragments.map(fragment => [fragment.platform, fragment]));

  const missing = Object.keys(PLATFORMS).filter(platform => !byPlatform.has(platform));
  if (missing.length) throw new Error(`缺少平台片段：${missing.join(', ')}（找到：${[...byPlatform.keys()].join(', ') || '（无）'}）`);
  const unknown = [...byPlatform.keys()].filter(platform => !PLATFORMS[platform]);
  if (unknown.length) throw new Error(`未知平台片段：${unknown.join(', ')}`);

  const manifest = readManifest(version);
  for (const [platform, spec] of Object.entries(PLATFORMS)) {
    const fragment = byPlatform.get(platform);
    if (fragment.version !== version) throw new Error(`${platform}: 片段版本 ${fragment.version} 与发布版本 ${version} 不一致`);
    if (typeof fragment.pubDate !== 'string' || !fragment.pubDate) throw new Error(`${platform}: 缺少 pubDate`);
    if (fragment.pubDate > manifest.pub_date) manifest.pub_date = fragment.pubDate;

    const keys = Object.keys(fragment.platforms ?? {}).sort();
    if (keys.join(',') !== [...spec.keys].sort().join(',')) {
      throw new Error(`${platform}: 平台键 ${keys.join(', ') || '（无）'} 与期望 ${spec.keys.join(', ')} 不一致`);
    }
    for (const key of spec.keys) {
      const entry = fragment.platforms[key];
      const url = entry?.url;
      const signature = entry?.signature;
      if (typeof url !== 'string' || !url.startsWith(`${baseUrl}/`)) throw new Error(`${platform}: ${key} 的下载地址必须以 ${baseUrl}/ 开头`);
      if (path.posix.basename(url) !== spec.asset) throw new Error(`${platform}: ${key} 的下载地址应指向 ${spec.asset}`);
      if (typeof signature !== 'string' || !signature.trim()) throw new Error(`${platform}: ${key} 缺少签名`);
      if (!fs.existsSync(path.join(dir, spec.asset))) throw new Error(`${platform}: 产物目录缺少 ${spec.asset}`);
      manifest.platforms[key] = {signature: signature.trim(), url};
    }
  }

  // 每个更新包只能有一个签名：同一资产的多个平台键（Windows 的 NSIS 键与兼容键
  // 指向同一个安装包）必须签名一致，不同资产的签名必须互不相同，否则说明产物被
  // 覆盖或复用。逐个校验平台下的所有键，不能只看第一个键。
  const signaturesByAsset = new Map();
  for (const platform of Object.keys(PLATFORMS)) {
    const spec = PLATFORMS[platform];
    const asset = path.posix.basename(manifest.platforms[spec.keys[0]].url);
    for (const key of spec.keys) {
      const signature = manifest.platforms[key].signature;
      if (!signaturesByAsset.has(asset)) {
        signaturesByAsset.set(asset, signature);
      } else if (signaturesByAsset.get(asset) !== signature) {
        throw new Error(`${platform}: ${asset} 的平台键 ${key} 签名与其他键不一致`);
      }
    }
  }
  if (new Set(signaturesByAsset.values()).size !== signaturesByAsset.size) {
    throw new Error('不同平台的更新包签名重复，说明产物被覆盖或复用');
  }

  const manifestPath = path.join(dir, 'latest.json');
  fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return {manifestPath, version, platforms: Object.keys(manifest.platforms)};
}

function usage() {
  return [
    '用法：',
    '  node scripts/update-manifest.mjs collect --platform <mac-arm64|mac-x64|win-x64|linux-x64> --bundle-dir <dir> --version <1.2.3> --out <dir>',
    '  node scripts/update-manifest.mjs merge --dir <dir> --version <1.2.3>',
    `可选：--base-url <url>（默认 ${BASE_URL}）`,
  ].join('\n');
}

export function main(argv = process.argv.slice(2)) {
  const {values, positionals} = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      platform: {type: 'string'},
      'bundle-dir': {type: 'string'},
      out: {type: 'string'},
      dir: {type: 'string'},
      version: {type: 'string'},
      'base-url': {type: 'string', default: BASE_URL},
    },
  });
  const command = positionals[0];
  if (command === 'collect') {
    const result = collect({
      platform: values.platform,
      bundleDir: values['bundle-dir'],
      version: (values.version ?? '').replace(/^v/, ''),
      outDir: values.out,
      baseUrl: values['base-url'],
    });
    console.log(`update-manifest: ${values.platform} 更新包 ${result.payload} → ${path.basename(result.asset)}（${result.keys.join(', ')}）`);
    return 0;
  }
  if (command === 'merge') {
    const result = merge({dir: values.dir, version: (values.version ?? '').replace(/^v/, ''), baseUrl: values['base-url']});
    console.log(`update-manifest: 已生成 ${result.manifestPath}（v${result.version}，${result.platforms.join(', ')}）`);
    return 0;
  }
  console.error(usage());
  return 2;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    process.exitCode = main();
  } catch (error) {
    console.error(`update-manifest: ${error.message}`);
    process.exitCode = 1;
  }
}
