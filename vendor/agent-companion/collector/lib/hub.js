import path from 'node:path';
export const TERMINAL = new Set(['done', 'error', 'aborted']);
export const STALE_MS = 45 * 60 * 1000;

function asId(value) {
  return value == null || value === '' ? '' : String(value);
}
function hostedCodexIds(sessions) {
  const hosted = new Set();
  for (const session of sessions) {
    const id = asId(session.externalId);
    if (session.source !== 'codeg' || !id) continue;
    hosted.add(id);
    hosted.add(id.replace(/^thr_/, ''));
    if (!id.startsWith('thr_')) hosted.add(`thr_${id}`);
  }
  return hosted;
}
function isCodegHostedCodex(session, hosted) {
  if (session.source !== 'codex') return false;
  const id = asId(session.sessionId);
  return hosted.has(id) || hosted.has(id.replace(/^thr_/, '')) || hosted.has(`thr_${id}`);
}

export class Hub {
  constructor({ now = Date.now } = {}) { this.now = now; this.startedAt = now(); this.sessions = new Map(); this.events = new Map(); this.ready = false; this.sources = {}; }
  health(source, state, detail, extra = {}) { this.sources[source] = { state, detail, checkedAt: this.now(), ...extra }; }
  ingest(ev) {
    if (!ev.sessionId || !ev.source) return;
    const id = `${ev.source}:${ev.sessionId}`, ts = ev.ts || this.now();
    let s = this.sessions.get(id);
    if (!s) {
      s = { id, source: ev.source, sessionId: ev.sessionId, cwd: '', title: '', roundId: ev.roundId || `observed:${ts}`, startedAt: ts, updatedAt: 0, status: 'unknown', steps: [], pending: [], tokens: null, endedAt: null };
      this.sessions.set(id, s);
    }
    if (ev.cwd) s.cwd = ev.cwd;
    if (ev.title) s.title = String(ev.title).slice(0, 240);
    if (ev.folderId != null && ev.folderId !== '') s.folderId = ev.folderId;
    if (ev.agentType) s.agentType = String(ev.agentType);
    if (ev.hostKind) s.hostKind = String(ev.hostKind);
    if (ev.sourceLabel) s.sourceLabel = [...String(ev.sourceLabel)].slice(0, 60).join('');
    if (ev.externalId) s.externalId = String(ev.externalId);
    if (Number.isInteger(ev.webPort) && ev.webPort > 0 && ev.webPort < 65536) s.webPort = ev.webPort;
    if (ev.type === 'meta') { if (ev.roundId && s.roundId.startsWith('observed:')) s.roundId = ev.roundId; return; }
    // Hooks can omit call ids. A later model continuation proves those anonymous
    // approvals are no longer blocking. Compare each request's time so recovery
    // can reconcile it even when a newer token event advanced updatedAt.
    if (ev.source === 'codex' && ev.type === 'activity') {
      s.pending = s.pending.filter(p => !(String(p.id).startsWith('perm:') && p.ts < ts));
      if (s.status === 'wait' && !s.pending.length) s.status = 'running';
    }
    const sameRoundRequest = ev.source === 'codex' && ev.roundId === s.roundId
      && ['wait', 'resolve'].includes(ev.type);
    if (ts < s.updatedAt && !sameRoundRequest) return; // Hooks may arrive ahead of same-round log requests.
    if (ev.type === 'start' && ev.roundId !== s.roundId) {
      Object.assign(s, { roundId: ev.roundId || `turn:${ts}`, startedAt: ts, endedAt: null, pending: [], steps: [], status: 'running', tokens: null });
      s.permissionChecks = []; delete s.endedBy;
    }
    if (ev.roundId && s.roundId.startsWith('observed:')) s.roundId = ev.roundId;
    if (ev.roundId && ev.type !== 'start' && ev.roundId !== s.roundId) return;
    s.updatedAt = Math.max(ts, s.updatedAt);
    if (ev.type === 'start') s.status = s.pending.length ? 'wait' : 'running';
    if (ev.type === 'step' && !TERMINAL.has(s.status)) {
      if (!s.steps.some(x => x.id === ev.eventId)) s.steps.push({ id: ev.eventId, ts, label: String(ev.label || '工具调用').slice(0, 200) });
      s.steps = s.steps.slice(-20); s.status = s.pending.length ? 'wait' : 'running';
    }
    if (ev.type === 'wait' && !TERMINAL.has(s.status)) {
      if (!s.pending.some(p => p.id === ev.callId)) s.pending.push({ id: ev.callId, tool: ev.tool, text: String(ev.text || '等待用户输入').slice(0, 500), questions: ev.questions || [], ts });
      s.status = 'wait'; this.event(s, 'wait', ev.callId, ts);
    }
    if (ev.type === 'resolve') {
      s.pending = s.pending.filter(p => p.id !== ev.callId);
      if (!TERMINAL.has(s.status)) s.status = s.pending.length ? 'wait' : 'running';
    }
    if (ev.type === 'permission_check' && !TERMINAL.has(s.status)) {
      s.permissionChecks ||= [];
      if (!s.permissionChecks.includes(ev.callId)) s.permissionChecks.push(ev.callId);
    }
    if (ev.type === 'permission_resolve') {
      s.permissionChecks = (s.permissionChecks || []).filter(id => id !== ev.callId);
    }
    if (ev.type === 'end') {
      s.status = ev.status; s.endedAt = ts; s.pending = [];
      s.permissionChecks = [];
      if (ev.endedBy) s.endedBy = String(ev.endedBy);
      if (['done', 'error'].includes(s.status)) this.event(s, s.status, s.roundId, ts);
    }
    if (ev.type === 'tokens' && Number.isFinite(ev.tokens)) s.tokens = ev.tokens;
  }
  event(s, kind, key, ts) {
    const id = JSON.stringify([s.id, s.roundId, kind, key]);
    if (!this.events.has(id)) this.events.set(id, { id, sessionId: s.id, roundId: s.roundId, kind, ts, historical: !this.ready || ts < this.startedAt, title: s.title || path.basename(s.cwd) || s.source });
    if (this.events.size > 500) this.events.delete(this.events.keys().next().value);
  }
  snapshot() {
    const now = this.now();
    const hosted = hostedCodexIds(this.sessions.values());
    const sessions = [...this.sessions.values()].filter(s => !isCodegHostedCodex(s, hosted)).map(s => {
      const stale = !TERMINAL.has(s.status) && !s.pending.length && now - s.updatedAt > STALE_MS;
      return { ...s, project: path.basename(s.cwd) || s.source, status: stale ? 'unknown' : s.status, stale, elapsed: Math.max(0, ((s.endedAt || now) - s.startedAt) / 1000), progress: null };
    }).sort((a,b) => b.updatedAt - a.updatedAt);
    // A long replay can evict old events; unresolved requests must stay discoverable.
    const events = new Map(this.events);
    for (const s of sessions) for (const p of s.pending) {
      const id=JSON.stringify([s.id,s.roundId,'wait',p.id]);
      if(!events.has(id))events.set(id,{id,sessionId:s.id,roundId:s.roundId,kind:'wait',ts:p.ts,historical:true,title:s.title||s.project});
    }
    return { version: 1, ready: this.ready, ts: now, sources: this.sources, sessions, events: [...events.values()] };
  }
}
