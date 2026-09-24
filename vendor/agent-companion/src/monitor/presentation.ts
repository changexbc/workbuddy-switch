import { STATUS, sessionHeadline } from './model.js';
import { agentSessionLink, codeBuddyFolderLink, hasOpenableFolder, isCodeBuddyVSCodeHost, sessionBadge, sessionSourceLabel, type SessionBadge } from './session-link.js';
import type { ConnectionState, PresentationStatus, Session, SourceHealth } from '../types/snapshot.js';

export interface SessionPresentation {
  status: PresentationStatus;
  statusLabel: string;
  title: string;
  provider: string;
  badge: SessionBadge | null;
  question: string;
  url: string | null;
  action: string;
}

// Shared by the office card and the desktop rail. No scene or DOM dependencies.
export function sessionPresentation(session?: Session | null, connection: ConnectionState = 'connected', sourceHealth?: SourceHealth): SessionPresentation {
  const unavailable = connection !== 'connected' || (sourceHealth && !['ok', 'partial'].includes(sourceHealth.state));
  const status = unavailable ? 'offline' : session?.status || 'idle';
  const pending = session?.pending?.[0];
  const url = agentSessionLink(session);
  // Each host names the action after the link its own builder accepts, so the
  // button never promises an app the target does not open.
  const action = session?.source !== 'codebuddy-ide' ? '进入对话'
    : isCodeBuddyVSCodeHost(session) ? (hasOpenableFolder(session.cwd) ? '打开工程' : '打开 VS Code')
      : (codeBuddyFolderLink(session.cwd, session) ? '打开工程' : '打开 CodeBuddy');
  return {
    status,
    statusLabel: session?.endedBy === 'host' ? '已退出' : status === 'running' && session?.permissionChecks?.length ? '权限检查中' : STATUS[status] || status,
    title: sessionHeadline(session, status),
    provider: sessionSourceLabel(session),
    badge: sessionBadge(session),
    question: status === 'wait' ? pending?.questions?.[0]?.text || pending?.text || '' : '',
    url,
    action,
  };
}
