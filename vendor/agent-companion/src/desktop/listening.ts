import { hostFetch } from './host.js';
import type { Settings, SourceConfig, SourceId } from '../types/settings.js';

export const agents: readonly (readonly [SourceId, string])[] = [['codex', 'Codex'], ['workbuddy', 'WorkBuddy'], ['codebuddy-ide', 'CodeBuddy'], ['codeg', 'Codeg']];

const knownIds = agents.map(([id]) => id as string);

async function request(options: Parameters<typeof hostFetch>[1] = {}): Promise<Settings> {
  const response = await hostFetch('/api/settings', options);
  const value = await response.json();
  if (!response.ok) throw Error((value as {error?: string})?.error || '读取监听配置失败');
  return value as Settings;
}

export async function loadListening(): Promise<Record<SourceId, SourceConfig>> { return (await request()).sources; }

export async function saveListening(changes: Partial<Record<SourceId, boolean>>): Promise<Record<SourceId, SourceConfig>> {
  // Read at save time to preserve paths and edits from the other settings hosts.
  const latest = await request();
  for (const [id, enabled] of Object.entries(changes)) {
    if (!knownIds.includes(id) || typeof enabled !== 'boolean') throw Error('监听配置无效');
    const sourceId = id as SourceId;
    latest.sources[sourceId] = { ...latest.sources[sourceId], enabled };
  }
  return (await request({method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(latest)})).sources;
}

export async function saveWorkbuddyLogWatch(logWatch: boolean): Promise<Record<SourceId, SourceConfig>> {
  if (typeof logWatch !== 'boolean') throw Error('监听配置无效');
  const latest = await request();
  latest.sources.workbuddy = { ...latest.sources.workbuddy, logWatch };
  return (await request({method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(latest)})).sources;
}
