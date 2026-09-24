import { desktopCommand, hostFetch, isDesktop } from './host.js';
import type { IntegrationAction, IntegrationStatus, Integrations } from '../types/integrations.js';
import type { SourceId } from '../types/settings.js';

const sourceIds = ['codex', 'workbuddy', 'codebuddy-ide', 'codeg'];
function isStatus(value: unknown): value is IntegrationStatus {
  if (!value || typeof value !== 'object') return false;
  const item = value as Record<string, unknown>;
  return typeof item.source === 'string' && sourceIds.includes(item.source)
    && ['hooks', 'webhook'].includes(String(item.kind))
    && ['installed', 'not_installed', 'partial', 'error', 'unavailable', 'pending'].includes(String(item.status))
    && typeof item.message === 'string' && typeof item.automatic === 'boolean'
    && Array.isArray(item.locations) && item.locations.every(path => typeof path === 'string')
    && (item.lastEventAt == null || (typeof item.lastEventAt === 'number' && Number.isFinite(item.lastEventAt)));
}

export async function requestIntegrations(action?: IntegrationAction): Promise<Integrations> {
  const response = await hostFetch('/api/integrations', action ? {
    method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(action),
  } : {});
  let value: unknown;
  try { value = await response.json(); } catch { throw Error('当前运行环境不支持接入管理，请更新并启动桌面应用'); }
  if (!response.ok) throw Error((value as {error?: string})?.error || '读取接入状态失败');
  if (!value || typeof value !== 'object' || !Array.isArray((value as Integrations).sources)) throw Error('接入状态响应无效');
  const result = value as Integrations;
  if (!result.sources.every(isStatus) || new Set(result.sources.map(item => item.source)).size !== result.sources.length) throw Error('接入状态响应无效');
  return result;
}

export async function openIntegrationFolder(source: SourceId, location: string): Promise<void> {
  if (!isDesktop()) throw Error('仅桌面应用支持打开配置文件夹');
  await desktopCommand('open_integration_folder', {source, location});
}
