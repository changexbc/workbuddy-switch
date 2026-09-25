#!/usr/bin/env node
/**
 * Applies the updater signing identity to the standalone Tauri config for CI.
 *
 * The trust root is `plugins.updater.pubkey`, and the bundler refuses to build
 * updater artifacts without the matching private key in
 * `TAURI_SIGNING_PRIVATE_KEY` (it warns when the two do not match). Both live in
 * repository configuration, never in the checkout:
 *
 *   TAURI_UPDATER_PUBKEY      repository variable, public minisign key
 *   TAURI_SIGNING_PRIVATE_KEY repository secret, private minisign key
 *
 * Tag builds must be signed (`--require-signing`): a release without a usable
 * trust root is worse than a failed build. Manual workflow runs may skip signing
 * (`--optional-signing`) so plain installers can still be built before the keys
 * exist; the config is then patched to stop producing updater artifacts.
 *
 * The private key never passes through this script and is never printed.
 */
import fs from 'node:fs';
import process from 'node:process';
import { parseArgs } from 'node:util';
import { pathToFileURL } from 'node:url';
import crypto from 'node:crypto';
import { BASE_URL } from './update-manifest.mjs';

export const CONFIG_PATH = 'src-tauri/tauri.conf.json';
const PASSWORD_HINT = 'npx tauri signer generate -w ~/.tauri/agent-companion.key';

function assertPublicKey(pubkey) {
  const decoded = Buffer.from(pubkey, 'base64').toString('utf8');
  if (!decoded.includes('minisign public key')) throw new Error(`TAURI_UPDATER_PUBKEY 不是 minisign 公钥（可用 ${PASSWORD_HINT} 生成）`);
  const key = decoded.split('\n').map(line => line.trim()).filter(line => line && !line.startsWith('untrusted comment:')).pop();
  if (!key) throw new Error('TAURI_UPDATER_PUBKEY 缺少编码后的公钥');
}

function readConfig(configPath) {
  let config;
  try {
    config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  } catch (error) {
    throw new Error(`无法读取 ${configPath}：${error.message}`);
  }
  const endpoints = config.plugins?.updater?.endpoints;
  if (!Array.isArray(endpoints) || !endpoints.includes(`${BASE_URL}/latest.json`)) {
    throw new Error(`${configPath} 的 plugins.updater.endpoints 必须包含 ${BASE_URL}/latest.json`);
  }
  return config;
}

/**
 * `requireSigning` decides what an empty pubkey means: a hard failure for tag
 * builds, "build installers only" for manual runs.
 */
export function configure({configPath = CONFIG_PATH, pubkey = '', requireSigning = true} = {}) {
  const config = readConfig(configPath);
  const key = pubkey.trim();
  if (!key) {
    if (requireSigning) {
      throw new Error(
        [
          '未配置更新签名公钥，标签构建已停止（不会创建不可更新的 Release）。',
          `生成密钥：${PASSWORD_HINT}`,
          'GitHub → Settings → Secrets and variables → Actions：',
          '  变量 TAURI_UPDATER_PUBKEY = 公钥内容',
          '  机密 TAURI_SIGNING_PRIVATE_KEY = 私钥内容（可选 TAURI_SIGNING_PRIVATE_KEY_PASSWORD）',
        ].join('\n'),
      );
    }
    config.bundle.createUpdaterArtifacts = false;
    fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
    return {signing: 'disabled', fingerprint: null, configPath};
  }

  assertPublicKey(key);
  config.plugins.updater.pubkey = key;
  config.bundle.createUpdaterArtifacts = true;
  fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
  return {signing: 'enabled', fingerprint: crypto.createHash('sha256').update(key).digest('hex').slice(0, 16), configPath};
}

export function main(argv = process.argv.slice(2), env = process.env, output = process.stdout) {
  const {values} = parseArgs({
    args: argv,
    options: {
      config: {type: 'string', default: CONFIG_PATH},
      'require-signing': {type: 'boolean', default: false},
      'optional-signing': {type: 'boolean', default: false},
    },
  });
  if (values['require-signing'] === values['optional-signing']) {
    throw new Error('必须且只能指定 --require-signing 或 --optional-signing');
  }
  const result = configure({
    configPath: values.config,
    pubkey: env.TAURI_UPDATER_PUBKEY ?? '',
    requireSigning: values['require-signing'],
  });
  if (env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT, `signing=${result.signing}\n`);
  output.write(
    result.signing === 'enabled'
      ? `configure-updater: 已启用签名更新产物（公钥指纹 sha256:${result.fingerprint}）\n`
      : 'configure-updater: 未配置更新签名公钥，本次构建不生成更新产物（仅手动安装包）\n',
  );
  return 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    process.exitCode = main();
  } catch (error) {
    console.error(`configure-updater: ${error.message}`);
    process.exitCode = 1;
  }
}
