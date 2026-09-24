// Persistence for custom templates, mirroring crates/agent-studio-core/src/custom/store.rs.
// Lives in its own file so a damaged store never touches settings.json or the
// built-in integrations policy.
import fs from 'node:fs/promises';
import path from 'node:path';
import {randomUUID} from 'node:crypto';
import {validateTemplate} from './custom.js';

export const CUSTOM_STORE_VERSION = 1;
const FILE = path.join('.agent-studio', 'custom-integrations.json');

export const customStoreFile = home => path.join(home, FILE);

export function parseCustomStore(bytes) {
  let value;
  try { value = JSON.parse(bytes); } catch { throw Error('不是有效 JSON'); }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw Error('顶层必须是对象');
  for (const key of Object.keys(value)) if (!['version', 'templates'].includes(key)) throw Error(`未知字段 ${key}`);
  if (value.version !== CUSTOM_STORE_VERSION) throw Error('存储版本无效');
  const entries = new Map();
  if (value.templates === undefined) return entries;
  if (!value.templates || typeof value.templates !== 'object' || Array.isArray(value.templates)) throw Error('templates 必须是对象');
  for (const [id, entry] of Object.entries(value.templates)) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) throw Error(`${id} 配置无效`);
    for (const key of Object.keys(entry)) if (!['enabled', 'importedAt', 'template'].includes(key)) throw Error(`${id} 含有未知字段 ${key}`);
    if (typeof entry.enabled !== 'boolean') throw Error(`${id} 缺少 enabled`);
    if (!Number.isInteger(entry.importedAt)) throw Error(`${id} 缺少 importedAt`);
    const validated = validateTemplate(entry.template);
    if (!validated.ok) throw Error(`${id} 模板无效：${validated.path}${validated.message}`);
    if (validated.template.id !== id) throw Error(`${id} 与模板 id 不一致`);
    entries.set(id, {template: validated.template, enabled: entry.enabled, importedAt: entry.importedAt});
  }
  return entries;
}

function serialize(entries) {
  return JSON.stringify({
    version: CUSTOM_STORE_VERSION,
    templates: Object.fromEntries([...entries].map(([id, entry]) => [id, {enabled: entry.enabled, importedAt: entry.importedAt, template: entry.template}])),
  }, null, 2) + '\n';
}

async function atomicWrite(file, text) {
  await fs.mkdir(path.dirname(file), {recursive: true});
  const temporary = `${file}.${randomUUID()}.tmp`;
  try {
    await fs.writeFile(temporary, text, {mode: 0o600});
    await fs.rename(temporary, file);
  } finally { await fs.rm(temporary, {force: true}); }
}

export function createCustomStore(home) {
  const file = customStoreFile(home);
  const entries = new Map();
  let loadError = null;
  const writable = () => {
    if (loadError) throw Error(loadError);
  };
  const persist = async () => atomicWrite(file, serialize(entries));
  return {
    file,
    get error() { return loadError; },
    get entries() { return entries; },
    get(id) { return entries.get(id); },
    async load() {
      try {
        entries.clear();
        for (const [id, entry] of parseCustomStore(await fs.readFile(file, 'utf8'))) entries.set(id, entry);
      } catch (failure) {
        if (failure.code === 'ENOENT') { loadError = null; return; }
        entries.clear();
        loadError = `自定义接入配置不可用（${failure.message}）。已保留原文件，请修复或删除 ${file}`;
      }
    },
    async import(template, now) {
      writable();
      if (entries.has(template.id)) throw Error(`已存在 ID 为 ${template.id} 的自定义来源，请先删除后再导入`);
      entries.set(template.id, {template, enabled: true, importedAt: now});
      try { await persist(); } catch (failure) { entries.delete(template.id); throw failure; }
    },
    async remove(id) {
      writable();
      if (!entries.has(id)) return false;
      const previous = entries.get(id);
      entries.delete(id);
      try { await persist(); } catch (failure) { entries.set(id, previous); throw failure; }
      return true;
    },
    async setEnabled(id, enabled) {
      writable();
      const entry = entries.get(id);
      if (!entry) throw Error('未知的自定义来源');
      const previous = entry.enabled;
      entry.enabled = enabled;
      try { await persist(); } catch (failure) { entry.enabled = previous; throw failure; }
    },
  };
}
