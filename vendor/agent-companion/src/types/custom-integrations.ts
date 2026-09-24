/** Response shapes of the custom-hook RPC, mirrored from `adapters/custom.rs`. */
export type CustomOutcome = 'accepted' | 'ignored' | 'rejected';

export interface CustomEventRow {
  event: string;
  action: string;
}

export interface CustomTemplateStatus {
  id: string;
  /** `custom:<id>`; never one of the four built-in source ids. */
  source: string;
  name: string;
  enabled: boolean;
  importedAt: number;
  /** Capabilities the mapping declares, e.g. `start`, `wait:permission`, `finish:error`. */
  capabilities: string[];
  events: CustomEventRow[];
  /** Null until an event of that kind arrives; a template alone proves nothing. */
  lastReceivedAt: number | null;
  lastMappedAt: number | null;
  /** The command a user pastes into the third-party tool's own hook config. */
  command: string;
}

export interface CustomDiagnostic {
  at: number;
  source: string;
  event: string | null;
  outcome: CustomOutcome;
  reason: string;
  detail: string;
}

export interface CustomIntegrations {
  version: number;
  storage: {ok: boolean; error: string | null};
  binaryInstalled: boolean;
  templates: CustomTemplateStatus[];
  diagnostics: CustomDiagnostic[];
}

export interface CustomPreview {
  ok: boolean;
  outcome: CustomOutcome;
  action?: string | null;
  reason: string;
  detail?: string;
  path?: string;
  /** Present only when the preview did not map, and always without side effects. */
  error?: string;
  event?: string | null;
  sessionId?: string | null;
  roundId?: string | null;
  capabilities?: string[];
  events: unknown[];
}

export type CustomAction =
  | {action: 'import'; template: unknown}
  | {action: 'enable' | 'disable' | 'remove'; id: string};

/** Body of a preview request: a template to validate plus the raw payload to map. */
export interface CustomPreviewRequest {
  template: unknown;
  payload: unknown;
}
