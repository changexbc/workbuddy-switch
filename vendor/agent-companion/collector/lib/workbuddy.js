// WorkBuddy is event-driven via Claude Code-compatible command hooks.
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { normalizeHook } from './codex-live.js';
import { question, questionDetails } from './codex.js';
import { HostPresence, endHostSessions } from './host-process.js';

export const HOOK_EVENTS = ['SessionStart', 'SessionEnd', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'PostToolUseFailure', 'PermissionRequest', 'Stop', 'Notification', 'PreCompact'];

function waitText(payload) {
  const message = typeof payload.message === 'string' ? payload.message.trim() : '';
  if (message) return message;
  const title = typeof payload.title === 'string' ? payload.title.trim() : '';
  return title || '需要你确认';
}

function confirmationNotification(payload) {
  const kind = String(payload.notification_type || payload.notificationType || '').toLowerCase();
  if (kind === 'auth_success') return false;
  if (kind === 'idle_prompt' || /permission|confirm|approval|credential/.test(kind)) return true;
  const text = `${payload.message || ''} ${payload.title || ''}`;
  return /[?？]|确认|Allow |Deny |允许|拒绝|credential|凭证/i.test(text);
}
export const HOOK_SCRIPT_NAME = 'astra-office-workbuddy.py';
export const HOOK_URL_NAME = 'astra-office-monitor.url';

function isNativeWorkBuddyHook(command = '') {
  return command.includes('agent-studio-runtime') && /\s+hook(?:\s|$)/.test(command) && command.includes('--source workbuddy');
}

function isPythonWorkBuddyHook(command = '') {
  return command.includes(HOOK_SCRIPT_NAME) || command.includes('workbuddy-status.py');
}

function isOurHook(command = '') {
  return isNativeWorkBuddyHook(command) || isPythonWorkBuddyHook(command);
}

export function workBuddySettingsCandidates(home = os.homedir()) {
  return [
    { label: 'international', dir: path.join(home, '.workbuddy-ai'), settingsPath: path.join(home, '.workbuddy-ai', 'settings.json') },
    { label: 'domestic', dir: path.join(home, '.workbuddy'), settingsPath: path.join(home, '.workbuddy', 'settings.json') },
  ];
}

async function exists(p) {
  try { await fs.stat(p); return true; } catch { return false; }
}

export async function resolveWorkBuddySettingsPathsAsync(home = os.homedir(), { settingsDir } = {}) {
  if (settingsDir) return [path.join(settingsDir, 'settings.json')];
  const [international, domestic] = workBuddySettingsCandidates(home);
  const files = [];
  if (await exists(international.dir) || await exists(international.settingsPath)) files.push(international.settingsPath);
  if (await exists(domestic.settingsPath) || await exists(path.join(domestic.dir, 'workbuddy.db'))) files.push(domestic.settingsPath);
  return files;
}

export function defaultWorkBuddyPaths(home = os.homedir()) {
  return workBuddySettingsCandidates(home).map(c => c.dir);
}

function hookCommand(scriptPath) {
  return `/usr/bin/python3 ${scriptPath}`;
}

export function mergeWorkBuddyHooks(file, command) {
  const doc = file && typeof file === 'object' ? structuredClone(file) : {};
  doc.hooks = doc.hooks && typeof doc.hooks === 'object' ? doc.hooks : {};
  const nativeInstalled = Object.values(doc.hooks).some(groups =>
    Array.isArray(groups) && groups.some(group =>
      (Array.isArray(group.hooks) ? group.hooks : [group]).some(h => isNativeWorkBuddyHook(h?.command))));
  for (const event of HOOK_EVENTS) {
    const groups = Array.isArray(doc.hooks[event]) ? doc.hooks[event] : [];
    const stale = command => nativeInstalled ? isPythonWorkBuddyHook(command) : isOurHook(command);
    doc.hooks[event] = groups.map(group => {
      if (Array.isArray(group.hooks)) {
        return { ...group, hooks: group.hooks.filter(h => !stale(h?.command)) };
      }
      if (stale(group.command)) return { hooks: [] };
      return group;
    }).filter(group => Array.isArray(group.hooks) ? group.hooks.length : !stale(group.command));
    if (nativeInstalled) continue;
    const handler = { type: 'command', command, timeout: 3, statusMessage: 'Agent Studio' };
    if (event !== 'SessionEnd') handler.async = true;
    doc.hooks[event].push({ matcher: '', hooks: [handler] });
  }
  return doc;
}

async function installWorkBuddyHooksAt(settingsPath, { monitorUrl, scriptSource } = {}) {
  const dir = path.dirname(settingsPath);
  try { await fs.stat(dir); } catch { return null; }
  let existing = {};
  try { existing = JSON.parse(await fs.readFile(settingsPath, 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  const hookDir = path.join(dir, 'hooks');
  const scriptPath = path.join(hookDir, HOOK_SCRIPT_NAME);
  const nativeInstalled = existing.hooks && Object.values(existing.hooks).some(groups =>
    Array.isArray(groups) && groups.some(group =>
      (Array.isArray(group.hooks) ? group.hooks : [group]).some(h => isNativeWorkBuddyHook(h?.command))));
  const command = nativeInstalled
    ? ((Object.values(existing.hooks || {}).flatMap(groups => Array.isArray(groups) ? groups : [])
      .flatMap(group => Array.isArray(group.hooks) ? group.hooks : [group])
      .find(h => isNativeWorkBuddyHook(h?.command)) || {}).command)
    : hookCommand(scriptPath);
  const merged = mergeWorkBuddyHooks(existing, command);
  if (!nativeInstalled) {
    await fs.mkdir(hookDir, { recursive: true });
    const source = scriptSource || fileURLToPath(new URL('../hooks/workbuddy-status.py', import.meta.url));
    await fs.copyFile(source, scriptPath);
    if (monitorUrl) await fs.writeFile(path.join(hookDir, HOOK_URL_NAME), String(monitorUrl).replace(/\/$/, ''), 'utf8');
  }
  if (JSON.stringify(existing) !== JSON.stringify(merged)) {
    await fs.writeFile(settingsPath, `${JSON.stringify(merged, null, 2)}\n`);
  }
  return { scriptPath, settingsPath, native: !!nativeInstalled };
}

export async function installWorkBuddyHooks(home, { monitorUrl = 'http://127.0.0.1:8849', scriptSource, settingsDir } = {}) {
  const settingsPaths = await resolveWorkBuddySettingsPathsAsync(home, { settingsDir });
  const installed = [];
  for (const settingsPath of settingsPaths) {
    const result = await installWorkBuddyHooksAt(settingsPath, { monitorUrl, scriptSource });
    if (result) installed.push(result);
  }
  if (!installed.length) return null;
  return { ...installed[0], settingsPaths: installed.map(item => item.settingsPath) };
}

export class WorkBuddyPoller {
  constructor(hub, { home = os.homedir(), monitorUrl, dataDir, hostPresence } = {}) {
    this.hub = hub;
    this.home = home;
    this.monitorUrl = monitorUrl;
    this.dataDir = dataDir;
    this.live = new Map();
    this.hookCount = 0;
    this.presence = hostPresence || new HostPresence({ source: 'workbuddy' });
  }

  ingestHook(payload) {
    const h = normalizeHook(payload);
    if (!h.sessionId || ![...HOOK_EVENTS, 'Interrupt'].includes(h.event)) return false;
    const prev = this.live.get(h.sessionId);
    const begins = ['SessionStart', 'UserPromptSubmit'].includes(h.event);
    const round = h.turnId || (begins ? `turn:${h.ts}` : prev?.roundId || `turn:${h.ts}`);
    if (prev && prev.roundId !== round && !begins) return false;
    const state = !prev || prev.roundId !== round
      ? { roundId: round, cwd: h.cwd || prev?.cwd || '', calls: new Map() }
      : prev;
    state.cwd = h.cwd || state.cwd;
    const edition = payload.agent_edition || payload.agentEdition || state.agentType || '';
    const agentType = edition === 'international' || edition === 'workbuddy-ai' ? 'workbuddy-ai' : 'workbuddy';
    state.agentType = agentType;
    const emit = ev => this.hub.ingest({ source: 'workbuddy', sessionId: h.sessionId, cwd: state.cwd, agentType, roundId: round, ts: h.ts, ...ev });
    if (!prev || begins) emit({ type: 'start' });
    const sessionTitle = typeof payload.session_title === 'string' ? payload.session_title.trim() : '';
    const isQuestion = /(?:^|__|\.)(request_user_input(?:_async)?|AskUserQuestion|ask_user_question|RequestUserInput)$/.test(h.tool);
    if (h.event === 'UserPromptSubmit') {
      for (const [call, c] of state.calls) if (c.async || c.permission) emit({ type: 'resolve', callId: call });
      const title = sessionTitle || h.prompt;
      if (title) emit({ type: 'meta', title: title.slice(0, 240) });
    }
    if (h.event === 'PreToolUse' && h.callId && !state.calls.get(h.callId)?.resolved) {
      state.calls.set(h.callId, { tool: h.tool, resolved: false, async: h.tool.endsWith('request_user_input_async'), ts: h.ts });
      if (isQuestion) emit({ type: 'wait', callId: h.callId, tool: h.tool, text: question(h.input), questions: questionDetails(h.input) });
      else emit({ type: 'step', eventId: h.callId, label: h.tool });
    }
    if ((h.event === 'PostToolUse' || h.event === 'PostToolUseFailure') && h.callId) {
      const asyncCall = state.calls.get(h.callId)?.async || h.tool.endsWith('request_user_input_async');
      state.calls.set(h.callId, { tool: h.tool, resolved: true, async: asyncCall, ts: h.ts });
      if (!asyncCall) emit({ type: 'resolve', callId: h.callId });
    }
    if (h.event === 'PermissionRequest') {
      const call = h.callId || `perm:${h.ts}`;
      if (!state.calls.get(call)?.resolved) {
        state.calls.set(call, { tool: h.tool || 'permission', resolved: false, permission: true, ts: h.ts });
        emit({ type: 'wait', callId: call, tool: h.tool || 'permission', text: waitText(payload) });
      }
    }
    if (['Stop', 'SessionEnd', 'Interrupt'].includes(h.event)) {
      emit({ type: 'end', status: h.event === 'Interrupt' ? 'aborted' : 'done' });
    }
    if (h.event === 'Notification') {
      if (sessionTitle) emit({ type: 'meta', title: sessionTitle.slice(0, 240) });
      if (confirmationNotification(payload)) {
        const call = h.callId || `notify:${h.ts}`;
        if (!state.calls.get(call)?.resolved) {
          state.calls.set(call, { tool: 'notification', resolved: false, permission: true, ts: h.ts });
          emit({ type: 'wait', callId: call, tool: 'notification', text: waitText(payload) });
        }
      } else emit({ type: 'activity' });
    }
    while (state.calls.size > 256) {
      const oldest = [...state.calls].filter(([, c]) => c.resolved).sort((a, b) => a[1].ts - b[1].ts)[0];
      if (!oldest) break;
      state.calls.delete(oldest[0]);
    }
    this.live.set(h.sessionId, state);
    this.hookCount++;
    this.presence.noteHook();
    this.poll();
    return true;
  }

  async poll() {
    const presence = await this.presence.observe();
    if (presence === 'gone') {
      endHostSessions(this.hub, 'workbuddy');
      this.hub.health('workbuddy', 'exited', 'WorkBuddy 已退出，未完成的任务已标记中止');
      return;
    }
    this.hub.health('workbuddy', 'ok', this.hookCount ? '已连接 WorkBuddy Hook（不读取会话文件）' : '等待新的 WorkBuddy Hook；不恢复历史会话');
  }

  async install(monitorUrl = this.monitorUrl) {
    try {
      await installWorkBuddyHooks(this.home, { monitorUrl, settingsDir: this.dataDir });
    } catch { /* installation is best-effort; incoming hooks remain usable */ }
  }
}
