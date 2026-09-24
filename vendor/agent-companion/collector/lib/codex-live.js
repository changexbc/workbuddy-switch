import { isInternalCodexPrompt } from '../../src/monitor/session-visibility.js';
// Codex sessions are driven exclusively by lifecycle hooks.
import fs from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const HOOK_EVENTS = ['SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'PermissionRequest', 'Stop', 'Interrupt', 'SessionEnd'];
export const HOOK_SCRIPT_NAME = 'astra-office-status.py';
export const HOOK_URL_NAME = 'astra-office-monitor.url';
export function hookCommand(scriptPath) {
  return `/usr/bin/python3 ${scriptPath}`;
}

function isNativeHook(handler) {
  const command = handler?.command || '';
  return command.includes('agent-studio-runtime') && /\s+hook(?:\s|$)/.test(command);
}

export function mergeCodexHooks(file, scriptPath) {
  const doc = file && typeof file === 'object' ? structuredClone(file) : {};
  doc.hooks = doc.hooks && typeof doc.hooks === 'object' ? doc.hooks : {};
  const command = hookCommand(scriptPath);
  const nativeInstalled = Object.values(doc.hooks).some(groups =>
    Array.isArray(groups) && groups.some(group => group.hooks?.some(isNativeHook)));
  for (const event of HOOK_EVENTS) {
    const groups = Array.isArray(doc.hooks[event]) ? doc.hooks[event] : [];
    doc.hooks[event] = groups.map(group => Array.isArray(group.hooks)
      ? { ...group, hooks: group.hooks.filter(h => !h.command?.includes(HOOK_SCRIPT_NAME)) }
      : group).filter(group => !Array.isArray(group.hooks) || group.hooks.length);
    // The native runtime owns registration after migration. Never restore Python hooks.
    if (nativeInstalled) continue;
    const handler = { type: 'command', command, timeout: 3, statusMessage: 'Astra office' };
    // SessionEnd always runs synchronously; Codex warns if async is set.
    if (event !== 'SessionEnd') handler.async = true;
    doc.hooks[event].push({ hooks: [handler] });
  }
  return doc;
}

export async function installCodexHooks(home, { monitorUrl = 'http://127.0.0.1:8849', scriptSource, codexDir=path.join(home,'.codex') } = {}) {
  const dir = path.join(codexDir, 'hooks');
  const scriptPath = path.join(dir, HOOK_SCRIPT_NAME);
  const hooksPath = path.join(codexDir, 'hooks.json');
  let existing = {};
  try { existing = JSON.parse(await fs.readFile(hooksPath, 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  const merged = mergeCodexHooks(existing, scriptPath);
  if (!Object.values(merged.hooks).some(groups => groups.some(group =>
    group.hooks?.some(h => h.command === hookCommand(scriptPath))))) {
    if (JSON.stringify(existing) !== JSON.stringify(merged))
      await fs.writeFile(hooksPath, `${JSON.stringify(merged, null, 2)}\n`);
    return { scriptPath, hooksPath, native: true };
  }
  await fs.mkdir(dir, { recursive: true });
  const source = scriptSource || fileURLToPath(new URL('../hooks/codex-status.py', import.meta.url));
  await fs.copyFile(source, scriptPath);
  await fs.writeFile(path.join(dir, HOOK_URL_NAME), String(monitorUrl).replace(/\/$/, ''), 'utf8');
  if (JSON.stringify(existing) !== JSON.stringify(merged))
    await fs.writeFile(hooksPath, `${JSON.stringify(merged, null, 2)}\n`);
  return { scriptPath, hooksPath };
}

function text(value) {
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) return value.map(item => item?.text || item).filter(Boolean).join('\n');
  return value == null ? '' : String(value);
}

export function normalizeHook(payload = {}) {
  const event = payload.hook_event_name || payload.hookEventName || '';
  const sessionId = payload.session_id || payload.sessionId || '';
  return {
    event: String(event),
    sessionId: String(sessionId),
    cwd: payload.cwd || '',
    turnId: payload.turn_id || payload.turnId || '',
    prompt: text(payload.prompt).trim(),
    tool: payload.tool_name || payload.toolName || 'permission',
    callId: payload.tool_use_id || payload.toolUseId || '',
    input: payload.tool_input || payload.toolInput || {},
    ts: Number(payload.timestamp) || Date.now(),
  };
}

export class CodexLivePoller {
  constructor(hub, { home, monitorUrl, dataDir=path.join(home,'.codex') } = {}) {
    this.hub = hub; this.home = home; this.monitorUrl = monitorUrl; this.dataDir = dataDir;
    this.live = new Map(); this.hookCount = 0;
  }

  ingestHook(payload) {
    const h = normalizeHook(payload);
    if (!h.sessionId || !HOOK_EVENTS.includes(h.event)) return false;
    const prev = this.live.get(h.sessionId);
    if (prev?.internal || h.event === 'UserPromptSubmit' && isInternalCodexPrompt(h.prompt)) {
      this.live.set(h.sessionId, { internal: true });
      this.hub.sessions.delete(`codex:${h.sessionId}`);
      return false;
    }
    const begins = ['SessionStart','UserPromptSubmit'].includes(h.event);
    const round = h.turnId || (begins ? `turn:${h.ts}` : prev?.roundId || `turn:${h.ts}`);
    if (prev && prev.roundId !== round && !begins) return false;
    const state = !prev || prev.roundId !== round
      ? {roundId:round,cwd:h.cwd || prev?.cwd || '',calls:new Map(),permissions:[]}
      : prev;
    state.cwd = h.cwd || state.cwd;
    const emit = ev => this.hub.ingest({source:'codex',sessionId:h.sessionId,cwd:state.cwd,roundId:round,ts:h.ts,...ev});
    if (!prev || begins) emit({type:'start'});
    const command = h.input.command == null ? null : createHash('sha256').update(JSON.stringify(h.input.command)).digest('hex');
    // Only synchronous questions block the turn; async prompts are ordinary steps.
    const isQuestion = /(?:^|__|\.)(request_user_input|AskUserQuestion|ask_user_question|RequestUserInput)$/.test(h.tool);
    if (h.event === 'UserPromptSubmit') {
      if(h.prompt) emit({type:'meta',title:h.prompt.slice(0,240)});
    }
    if (h.event === 'PreToolUse' && h.callId && !state.calls.get(h.callId)?.resolved) {
      state.calls.set(h.callId,{tool:h.tool,command,resolved:false,async:h.tool.endsWith('request_user_input_async'),ts:h.ts});
      if (isQuestion) emit({type:'wait',callId:h.callId,tool:h.tool,text:'需要你确认'});
      else emit({type:'step',eventId:h.callId,label:h.tool});
    }
    if (h.event === 'PermissionRequest') {
      const match = [...state.calls].filter(([id,c])=>h.callId ? id===h.callId : c.tool===h.tool && c.command===command).sort((a,b)=>Number(a[1].resolved)-Number(b[1].resolved) || b[1].ts-a[1].ts)[0];
      if (!match?.[1].resolved) {
        const call = match?.[0] || h.callId;
        const id = `perm:${call || h.ts}`;
        state.permissions.push({id,call,tool:h.tool,command});
        emit({type:'permission_check',callId:id});
      }
    }
    if (h.event === 'PostToolUse' && h.callId) {
      const async = state.calls.get(h.callId)?.async || h.tool.endsWith('request_user_input_async');
      state.calls.set(h.callId,{tool:h.tool,command,resolved:true,async,ts:h.ts});
      if (!async) emit({type:'resolve',callId:h.callId});
      state.permissions = state.permissions.filter(p=>{
        if (p.call===h.callId || !p.call && p.tool===h.tool && p.command===command) {emit({type:'permission_resolve',callId:p.id});return false;}
        return true;
      });
    }
    if (['Stop','SessionEnd','Interrupt'].includes(h.event)) {
      emit({type:'end',status:h.event==='Interrupt'?'aborted':'done'});state.permissions=[];
    }
    while (state.calls.size>256) {
      const oldest=[...state.calls].filter(([,c])=>c.resolved).sort((a,b)=>a[1].ts-b[1].ts)[0];
      if (!oldest) break;state.calls.delete(oldest[0]);
    }
    this.live.set(h.sessionId,state);this.hookCount++;this.poll();return true;
  }

  async poll() {
    this.hub.health('codex','ok',this.hookCount ? '已连接 Codex Hook（不读取会话文件）' : '等待新的 Codex Hook；不恢复历史会话');
  }

  async install(monitorUrl = this.monitorUrl) {
    if (!monitorUrl) return;
    try { await installCodexHooks(this.home, {monitorUrl,codexDir:this.dataDir}); }
    catch { /* installation is best-effort; incoming hooks remain usable */ }
  }
}
