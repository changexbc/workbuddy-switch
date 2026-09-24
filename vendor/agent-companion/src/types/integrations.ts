import type { SourceId } from './settings.js';

export interface IntegrationStatus {
  source: SourceId;
  kind: 'hooks' | 'webhook';
  status: 'installed' | 'not_installed' | 'partial' | 'error' | 'unavailable' | 'pending';
  message: string;
  locations: string[];
  automatic: boolean;
  lastEventAt?: number | null;
}
export interface Integrations { sources: IntegrationStatus[] }
export interface IntegrationAction { source: SourceId; action: 'install' | 'uninstall' }
