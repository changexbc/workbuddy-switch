import { hostFetch } from './host.js';
import type { CustomAction, CustomDiagnostic, CustomEventRow, CustomIntegrations, CustomPreview, CustomTemplateStatus } from '../types/custom-integrations.js';

const outcomes = ['accepted', 'ignored', 'rejected'];

function isEventRow(value: unknown): value is CustomEventRow {
  const row = value as CustomEventRow;
  return Boolean(row) && typeof row.event === 'string' && typeof row.action === 'string';
}

function isTemplate(value: unknown): value is CustomTemplateStatus {
  const item = value as CustomTemplateStatus;
  return Boolean(item) && typeof item.id === 'string' && typeof item.source === 'string'
    && item.source === `custom:${item.id}`
    && typeof item.name === 'string' && typeof item.enabled === 'boolean'
    && typeof item.importedAt === 'number' && Number.isFinite(item.importedAt)
    && typeof item.command === 'string'
    && Array.isArray(item.capabilities) && item.capabilities.every(entry => typeof entry === 'string')
    && Array.isArray(item.events) && item.events.every(isEventRow)
    && (item.lastReceivedAt == null || typeof item.lastReceivedAt === 'number')
    && (item.lastMappedAt == null || typeof item.lastMappedAt === 'number');
}

function isDiagnostic(value: unknown): value is CustomDiagnostic {
  const item = value as CustomDiagnostic;
  return Boolean(item) && typeof item.at === 'number' && typeof item.source === 'string'
    && (item.event == null || typeof item.event === 'string')
    && outcomes.includes(String(item.outcome))
    && typeof item.reason === 'string' && typeof item.detail === 'string';
}

/** Rejects a malformed RPC response before any of it reaches the settings tree. */
export function decodeCustomIntegrations(value: unknown): CustomIntegrations {
  const result = value as CustomIntegrations;
  if (!result || typeof result !== 'object') throw Error('自定义接入响应无效');
  if (typeof result.storage?.ok !== 'boolean') throw Error('自定义接入响应无效');
  if (result.storage.error != null && typeof result.storage.error !== 'string') throw Error('自定义接入响应无效');
  if (typeof result.binaryInstalled !== 'boolean') throw Error('自定义接入响应无效');
  if (!Array.isArray(result.templates) || !result.templates.every(isTemplate)) throw Error('自定义接入响应无效');
  if (new Set(result.templates.map(item => item.id)).size !== result.templates.length) throw Error('自定义接入响应无效');
  if (!Array.isArray(result.diagnostics) || !result.diagnostics.every(isDiagnostic)) throw Error('自定义接入响应无效');
  return result;
}

async function request(url: string, body: unknown, failure: string): Promise<unknown> {
  const response = body === undefined
    ? await hostFetch(url)
    : await hostFetch(url, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)});
  let value: unknown;
  try { value = await response.json(); } catch { throw Error('当前运行环境不支持自定义接入，请更新并启动桌面应用'); }
  if (!response.ok) throw Error((value as {error?: string})?.error || failure);
  return value;
}

export async function requestCustomIntegrations(action?: CustomAction): Promise<CustomIntegrations> {
  const value = await request('/api/custom-integrations', action, action ? '自定义接入操作失败' : '读取自定义接入失败');
  return decodeCustomIntegrations(value);
}

/** Side-effect free: the backend maps this payload with a throwaway engine. */
export async function requestCustomPreview(template: unknown, payload: unknown): Promise<CustomPreview> {
  const value = await request('/api/custom-integrations/preview', {template, payload}, '预览失败');
  const result = value as CustomPreview;
  if (!result || typeof result !== 'object' || typeof result.ok !== 'boolean'
    || !outcomes.includes(String(result.outcome)) || !Array.isArray(result.events)) throw Error('预览响应无效');
  return result;
}
