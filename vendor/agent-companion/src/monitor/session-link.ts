import { isDesktop, desktopCommand } from '../desktop/host.js';

/** The link helpers only read these fields, so partial sessions stay valid inputs. */
export interface LinkableSession {
  source?: string;
  agentType?: string;
  sessionId?: string;
  cwd?: string;
  /** `"vscode"` for the CodeBuddy VS Code plugin; absent or anything else is the IDE. */
  hostKind?: string;
  /** Display name an imported custom source carries on every event. */
  sourceLabel?: string;
  /** Set on a Codeg session delegated by another one; drives the 子任务 badge. */
  subagent?: boolean;
  /** Title of the session a subagent was delegated from; absent when unreadable. */
  parentTitle?: string;
}

export interface SessionBadge {
  host: string;
  id: string;
  label: string;
  /** Longer explanation a caller may show instead of `label` (a tooltip). */
  detail?: string;
  /** The single icon the rail avatar wears; defaults to `host`. */
  avatar?: string;
}

export const SESSION_WINDOW = {x:14,y:18,width:484,height:232};
export const SESSION_BUTTON = {x:385,y:250,width:110,height:25,radius:6};
export const SESSION_SOURCES = ['codex','workbuddy','codebuddy-ide','codeg'];
export const AGENT_ICON_IDS = ['codex','workbuddy','codebuddy-ide','codeg','grok','codebuddy-vscode'];
const NESTED_AGENTS: Record<string, string> = {
  code_buddy:'codebuddy-ide', codebuddy:'codebuddy-ide', codebuddy_code:'codebuddy-ide',
  claude_code:'claude', claude_acp:'claude', claude:'claude',
  grok:'grok', grok_build:'grok',
  codex:'codex', codex_acp:'codex',
  workbuddy:'workbuddy',
};
export function sourceLabel(source?: string, agentType?: string) {
  if (source==='workbuddy') return agentType==='workbuddy-ai' || agentType==='international' ? 'WorkBuddy 国际版' : 'WorkBuddy';
  if (source==='codebuddy-ide') {
    const type=String(agentType||'').toLowerCase();
    if (type==='codebuddycn' || type==='domestic') return 'CodeBuddy 国内版';
    if (type==='codebuddy' || type==='international') return 'CodeBuddy 国际版';
    return 'CodeBuddy';
  }
  return source==='codex'?'Codex':source==='codeg'?'Codeg':source==='grok'?'Grok':source==='claude'?'Claude':'未绑定';
}
/**
 * An imported custom source (`custom:<id>`) has no built-in name or icon: the
 * template name rides on the session as `sourceLabel`, and the id is the
 * fallback when an older snapshot predates it.
 */
export function isCustomSource(source?: string) {
  return typeof source==='string' && /^custom:[a-z][a-z0-9-]{0,63}$/.test(source);
}
function customSourceLabel(session?: LinkableSession | null) {
  const name=typeof session?.sourceLabel==='string'?session.sourceLabel.trim():'';
  if(name)return name;
  const id=String(session?.source||'').slice('custom:'.length);
  return id||'自定义来源';
}
export function sessionSourceLabel(session?: LinkableSession | null) {
  if(isCustomSource(session?.source))return customSourceLabel(session);
  return sourceLabel(session?.source, session?.agentType);
}
export function nestedAgentId(agentType?: string): string | null {
  const type=String(agentType||'').toLowerCase().replace(/[\s-]+/g,'_');
  if(!type)return null;
  return NESTED_AGENTS[type] || type;
}
export function sessionBadge(session?: LinkableSession | null): SessionBadge | null {
  if(!session?.source)return null;
  if(isCustomSource(session.source))return {host:session.source,id:session.source,label:customSourceLabel(session)};
  if(isCodeBuddyVSCodeHost(session))return {host:'codebuddy-ide',id:'codebuddy-vscode',label:'VS Code',avatar:'codebuddy-vscode'};
  if(session.source==='codeg'){
    const nested=nestedAgentId(session.agentType);
    // A delegated child only appears while it waits for the user; the badge
    // names what it is and whose task it belongs to.
    if(session.subagent)return {host:'codeg',id:nested||'codeg',label:'子任务',detail:session.parentTitle?`父会话：${session.parentTitle}`:undefined};
    if(nested)return {host:'codeg',id:nested,label:sourceLabel(nested)==='未绑定'?String(session.agentType):sourceLabel(nested)};
  }
  if (session.source==='workbuddy') {
    const international = session.agentType==='workbuddy-ai' || session.agentType==='international';
    return {host:'workbuddy',id:'workbuddy',label:international?'WorkBuddy 国际版':'WorkBuddy'};
  }
  return {host:session.source,id:session.source,label:sourceLabel(session.source, session.agentType)};
}
function isCodeBuddyInternationalType(agentType?: string) {
  const type=String(agentType||'').toLowerCase();
  if (type==='codebuddycn' || type==='domestic') return false;
  return type==='codebuddy' || type==='international' || !type;
}
export function isCodeBuddyInternational(session?: LinkableSession | null) {
  return session?.source==='codebuddy-ide' && isCodeBuddyInternationalType(session.agentType);
}
/**
 * The CodeBuddy VS Code plugin shares the IDE's hooks and source; only the
 * payload client distinguishes them. Any other hostKind stays an IDE session.
 */
export function isCodeBuddyVSCodeHost(session?: LinkableSession | null) {
  return session?.source==='codebuddy-ide' && session.hostKind==='vscode';
}
/** The one accepted shape of a project folder: absolute, non-empty, forward slashes. */
function folderAbsolutePath(cwd: unknown): string | null {
  if(typeof cwd !== 'string' || !cwd.trim())return null;
  const normalized=cwd.trim().replace(/\\/g,'/').replace(/\/+$/,'');
  if(!normalized || !/^(?:\/|[A-Za-z]:\/)/.test(normalized))return null;
  return normalized.startsWith('/')?normalized:`/${normalized}`;
}
export function codeBuddyFolderLink(cwd: unknown, session?: LinkableSession | null): string | null {
  const absolute=folderAbsolutePath(cwd);
  if(!absolute)return null;
  const scheme=isCodeBuddyInternationalType(session?.agentType)?'codebuddy':'codebuddycn';
  return `${scheme}://file${absolute.split('/').map(encodeURIComponent).join('/')}`;
}
/**
 * True when the session carries a folder the desktop side can open. The VS Code
 * host has no folder URL: `vscode://file/...` is handled by VS Code's own URL
 * handler, which always targets the last active window, so the desktop app
 * resolves the window folder from the plugin history instead.
 */
export function hasOpenableFolder(cwd: unknown): boolean {
  return folderAbsolutePath(cwd) !== null;
}
export function codegAppLink(session?: LinkableSession | null): string | null {
  if(session?.source!=='codeg'||typeof session?.sessionId!=='string'||!session.sessionId.trim())return null;
  if(session.sessionId.startsWith('connection:'))return '/api/open-session?source=codeg';
  return `codeg://session/${encodeURIComponent(session.sessionId.trim())}`;
}

export function isWorkBuddyInternational(session?: LinkableSession | null) {
  return session?.source==='workbuddy' && (session.agentType==='workbuddy-ai' || session.agentType==='international');
}

export function agentSessionLink(session?: LinkableSession | null): string | null {
  if(session?.source==='codebuddy-ide'){
    // The plugin has no URI handler, so a VS Code session can only ever open
    // its project folder — or, without one, the application itself. The folder
    // is resolved desktop-side from the session id; `cwd` is only a candidate.
    if(isCodeBuddyVSCodeHost(session)){
      const id=typeof session.sessionId==='string'?session.sessionId.trim():'';
      if(!id)return '/api/open-session?source=codebuddy-ide&host=vscode';
      const folder=folderAbsolutePath(session.cwd);
      return `/api/open-session?source=codebuddy-ide&host=vscode&session=${encodeURIComponent(id)}${folder?`&cwd=${encodeURIComponent(folder)}`:''}`;
    }
    const edition=isCodeBuddyInternational(session)?'international':'domestic';
    return codeBuddyFolderLink(session.cwd, session) || `/api/open-session?source=codebuddy-ide&edition=${edition}`;
  }
  if(session?.source==='codeg')return codegAppLink(session);
  if(typeof session?.sessionId !== 'string' || !session.sessionId.trim())return null;
  const id=encodeURIComponent(session.sessionId);
  if(session.source==='codex')return `codex://threads/${id}`;
  if(session.source==='workbuddy')return `${isWorkBuddyInternational(session)?'workbuddy-ai':'workbuddy'}://chat/${id}`;
  return null;
}

export function openSessionLink(url?: string | null, fetchImpl: typeof fetch | undefined = globalThis.fetch) {
  if(!url)return;
  if(isDesktop())return desktopCommand('open_session_url',{url});
  const path=url.startsWith('/')?url.split('?')[0]:(()=>{try{return new URL(url,'http://127.0.0.1').pathname;}catch{return '';}})();
  if(path==='/api/open-session'){
    if(typeof fetchImpl!=='function')return;
    return fetchImpl(url,{method:'POST'});
  }
  if(/^https?:/i.test(url)){
    const opened=typeof window.open==='function'?window.open(url,'_blank','noopener,noreferrer'):null;
    if(!opened&&typeof window.location?.assign==='function')window.location.assign(url);
    return;
  }
  if(typeof window.location!=='undefined')window.location.href=url;
}

export function workBuddySessionLink(session?: LinkableSession | null): string | null {
  if (session?.source !== 'workbuddy' || typeof session.sessionId !== 'string' || !session.sessionId.trim()) return null;
  return `${isWorkBuddyInternational(session)?'workbuddy-ai':'workbuddy'}://chat/${encodeURIComponent(session.sessionId)}`;
}

// GLTF screen UVs use the same top-to-bottom orientation as our canvas texture.
export function screenSessionLink(state?: (LinkableSession & {mode?: string; status?: string}) | null, uv?: {x: number; y: number} | null, flippedVertical=false): string | null {
  if(state?.mode !== 'monitor' || !uv)return null;
  const x=(uv.x*512-SESSION_WINDOW.x)*512/SESSION_WINDOW.width;
  const y=((flippedVertical ? 1-uv.y : uv.y)*288-SESSION_WINDOW.y)*288/SESSION_WINDOW.height;
  // The CTA is always available for a supported session. During a
  // question, keep the whole question card clickable as a convenient shortcut.
  const b=SESSION_BUTTON;
  const cta=x>=b.x && x<=b.x+b.width && y>=b.y && y<=b.y+b.height;
  const questionCard=state.status==='wait' && x>=109 && x<=495 && y>=100 && y<=243;
  return cta || questionCard ? agentSessionLink(state) : null;
}
