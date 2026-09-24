// Custom agent hook integrations, Node side. Mirrors
// crates/agent-studio-core/src/custom so the collector and the native runtime
// agree byte for byte; scripts/qa-custom-parity.mjs compares both against the
// shared fixture.
export const CUSTOM_SOURCE_PREFIX = 'custom:';
export const WAIT_CALL_ID = 'custom-wait';
export const CUSTOM_LIMITS = {
  templateBytes: 1048576, payloadBytes: 1048576, nameMax: 120, pointerMax: 512, eventsMax: 64,
  eventNameMax: 200, ignoreIfPresentMax: 16, sessionIdMax: 256, idFieldMax: 256, eventIdMax: 200,
  cwdMax: 2048, titleMax: 4096, dedupPerSession: 64, sessionsMax: 256, diagnosticsMax: 50,
};
const DETAIL_MAX = 200;
const PENDING_MAX = 16;
const TOP_KEYS = ['schemaVersion', 'id', 'name', 'transport', 'mapping', 'ignoreIfPresent', 'events'];
const MAPPING_KEYS = ['event', 'sessionId', 'roundId', 'eventId', 'timestamp', 'cwd', 'title', 'requestId'];
const ACTION_KEYS = ['action', 'reason', 'status'];
const BUILTIN_IDS = ['codex', 'workbuddy', 'codebuddy-ide', 'codeg'];

export const isCustomId = id => typeof id === 'string' && /^[a-z][a-z0-9-]{0,63}$/.test(id);
export const customSource = id => `${CUSTOM_SOURCE_PREFIX}${id}`;
export function parseCustomSource(source) {
  if (typeof source !== 'string' || !source.startsWith(CUSTOM_SOURCE_PREFIX)) return null;
  const id = source.slice(CUSTOM_SOURCE_PREFIX.length);
  return isCustomId(id) ? id : null;
}

/** RFC 6901. Returns `undefined` when the pointer has no value; JSON `null` stays `null`. */
export function resolvePointer(root, pointer) {
  if (pointer === '') return root;
  if (typeof pointer !== 'string' || !pointer.startsWith('/')) return undefined;
  let current = root;
  for (const raw of pointer.split('/').slice(1)) {
    const token = raw.replaceAll('~1', '/').replaceAll('~0', '~');
    if (current === null || typeof current !== 'object') return undefined;
    if (Array.isArray(current)) {
      if (!/^(0|[1-9][0-9]*)$/.test(token)) return undefined;
      current = current[Number(token)];
    } else {
      current = Object.prototype.hasOwnProperty.call(current, token) ? current[token] : undefined;
    }
    if (current === undefined) return undefined;
  }
  return current;
}

function pointerError(pointer) {
  if (typeof pointer !== 'string') return '必须为以 / 开头的 JSON Pointer（RFC 6901）';
  // Code points, matching Rust `chars().count()` and the schema's "按字符计".
  // `String.length` counts UTF-16 units and rejects a pointer Rust accepts.
  if ([...pointer].length > CUSTOM_LIMITS.pointerMax) return '必须为以 / 开头的 JSON Pointer（RFC 6901）';
  if (!pointer.startsWith('/')) return '必须为以 / 开头的 JSON Pointer（RFC 6901）';
  for (const token of pointer.split('/').slice(1)) {
    if (token === '') return '必须为以 / 开头的 JSON Pointer（RFC 6901）';
  }
  if (/~(?![01])/.test(pointer) || pointer.endsWith('~')) return 'Pointer 中的 ~ 必须转义为 ~0 或 ~1';
  return null;
}

const error = (path, message) => ({ok: false, path, message});

/**
 * Validates an imported template and returns its normalized form. Unknown keys
 * are rejected everywhere so a typo cannot silently disable a mapping.
 */
export function validateTemplate(input) {
  let bytes = 0;
  try { bytes = Buffer.byteLength(JSON.stringify(input) ?? ''); } catch { return error('', '模板无法序列化'); }
  if (bytes > CUSTOM_LIMITS.templateBytes) return error('', '模板超过 1 MiB 上限');
  if (!input || typeof input !== 'object' || Array.isArray(input)) return error('', '模板必须是 JSON 对象');
  for (const key of Object.keys(input)) if (!TOP_KEYS.includes(key)) return error(`/${key}`, '未知字段，请检查拼写');
  if (input.schemaVersion === undefined || input.schemaVersion === null) return error('/schemaVersion', '缺少必填字段');
  if (input.schemaVersion !== 1) return error('/schemaVersion', 'schemaVersion 仅支持 1');
  if (input.id === undefined || input.id === null) return error('/id', '缺少必填字段');
  if (!isCustomId(input.id)) return error('/id', 'id 必须匹配 [a-z][a-z0-9-]{0,63}');
  if (BUILTIN_IDS.includes(input.id)) return error('/id', 'id 不能使用内置来源 ID（codex、workbuddy、codebuddy-ide、codeg）');
  if (input.name === undefined || input.name === null) return error('/name', '缺少必填字段');
  if (typeof input.name !== 'string' || input.name === '' || [...input.name].length > CUSTOM_LIMITS.nameMax) {
    return error('/name', 'name 必须为 1-120 字符的非空字符串');
  }
  if (input.transport === undefined || input.transport === null) return error('/transport', '缺少必填字段');
  if (input.transport !== 'hook') return error('/transport', 'transport 仅支持 hook');
  const mapping = input.mapping;
  if (!mapping || typeof mapping !== 'object' || Array.isArray(mapping)) return error('/mapping', 'mapping 必须为对象');
  for (const key of Object.keys(mapping)) if (!MAPPING_KEYS.includes(key)) return error(`/mapping/${key}`, '未知字段，请检查拼写');
  const normalized = {};
  for (const key of ['event', 'sessionId']) {
    const value = mapping[key];
    if (value === undefined || value === null) return error(`/mapping/${key}`, '缺少必填字段');
    const problem = pointerError(value);
    if (problem) return error(`/mapping/${key}`, problem);
    normalized[key] = value;
  }
  for (const key of ['roundId', 'eventId', 'timestamp', 'cwd', 'title', 'requestId']) {
    if (mapping[key] === undefined) continue;
    const problem = pointerError(mapping[key]);
    if (problem) return error(`/mapping/${key}`, problem);
    normalized[key] = mapping[key];
  }
  const events = input.events;
  if (!events || typeof events !== 'object' || Array.isArray(events) || !Object.keys(events).length
    || Object.keys(events).length > CUSTOM_LIMITS.eventsMax) {
    return error('/events', 'events 必须为 1-64 个事件的对象（事件名 → 动作）');
  }
  const normalizedEvents = {};
  for (const [event, definition] of Object.entries(events)) {
    const base = `/events/${event}`;
    if (event === '' || [...event].length > CUSTOM_LIMITS.eventNameMax) return error('/events', '事件名必须为 1-200 字符的非空字符串');
    if (!definition || typeof definition !== 'object' || Array.isArray(definition)) return error(base, '事件配置必须为对象');
    for (const key of Object.keys(definition)) if (!ACTION_KEYS.includes(key)) return error(`${base}/${key}`, '未知字段，请检查拼写');
    const action = definition.action;
    if (action === undefined || action === null) return error(`${base}/action`, '缺少必填字段');
    if (action === 'wait') {
      if (definition.status !== undefined) return error(`${base}/status`, '该 action 不支持此字段');
      if (definition.reason === undefined || definition.reason === null) return error(`${base}/reason`, '缺少必填字段');
      if (!['permission', 'input'].includes(definition.reason)) return error(`${base}/reason`, 'reason 仅支持 permission 或 input');
      normalizedEvents[event] = {action: 'wait', reason: definition.reason};
    } else if (action === 'finish') {
      if (definition.reason !== undefined) return error(`${base}/reason`, '该 action 不支持此字段');
      if (definition.status === undefined || definition.status === null) return error(`${base}/status`, '缺少必填字段');
      if (!['done', 'error'].includes(definition.status)) return error(`${base}/status`, 'status 仅支持 done 或 error');
      normalizedEvents[event] = {action: 'finish', status: definition.status};
    } else if (['start', 'resume', 'close'].includes(action)) {
      for (const key of ['reason', 'status']) if (definition[key] !== undefined) return error(`${base}/${key}`, '该 action 不支持此字段');
      normalizedEvents[event] = {action};
    } else {
      return error(`${base}/action`, 'action 仅支持 start、wait、resume、finish、close');
    }
  }
  const result = {schemaVersion: 1, id: input.id, name: input.name, transport: 'hook', mapping: normalized, events: normalizedEvents};
  if (input.ignoreIfPresent !== undefined) {
    if (!Array.isArray(input.ignoreIfPresent) || input.ignoreIfPresent.length > CUSTOM_LIMITS.ignoreIfPresentMax) {
      return error('/ignoreIfPresent', 'ignoreIfPresent 必须为最多 16 项的 JSON Pointer 数组');
    }
    result.ignoreIfPresent = [];
    for (const [index, item] of input.ignoreIfPresent.entries()) {
      const problem = pointerError(item);
      if (problem) return error(`/ignoreIfPresent/${index}`, problem);
      result.ignoreIfPresent.push(item);
    }
  }
  return {ok: true, template: result};
}

export const capabilityOf = action => action.action === 'wait' ? `wait:${action.reason}`
  : action.action === 'finish' ? `finish:${action.status}` : action.action;

export function templateCapabilities(template) {
  return [...new Set(Object.values(template.events).map(capabilityOf))].sort();
}

/** Sorted event name → capability rows, the shape the settings page renders. */
export function templateEventTable(template) {
  return Object.entries(template.events)
    .map(([event, action]) => ({event, action: capabilityOf(action)}))
    .sort((a, b) => a.event < b.event ? -1 : a.event > b.event ? 1 : 0);
}

class FieldError extends Error {
  constructor(outcome) { super('field'); this.outcome = outcome; }
}

const accepted = (action, rawEvent, sessionId, roundId, events) => ({
  outcome: 'accepted', action, reason: action, detail: '', path: '', event: rawEvent, sessionId, roundId, events,
});
const ignored = (action, reason, detail, rawEvent, sessionId, roundId = null) => ({
  outcome: 'ignored', action, reason, detail, path: '', event: rawEvent, sessionId, roundId, events: [],
});
const rejected = (reason, path, detail) => ({
  outcome: 'rejected', action: null, reason, detail, path, event: null, sessionId: null, roundId: null, events: [],
});

export function outcomeJson(value) {
  return {
    outcome: value.outcome, action: value.action, reason: value.reason,
    detail: [...value.detail].slice(0, DETAIL_MAX).join(''), path: value.path,
    event: value.event, sessionId: value.sessionId, roundId: value.roundId, events: value.events,
  };
}

function idField(raw, pointer, max, field, missingDetail) {
  if (!pointer) return null;
  const value = resolvePointer(raw, pointer);
  if (value === undefined || value === null) return null;
  if (typeof value !== 'string' || value === '') throw new FieldError(rejected('type_mismatch', `/mapping/${field}`, missingDetail));
  if ([...value].length > max) throw new FieldError(rejected('too_long', `/mapping/${field}`, '字段超出长度上限'));
  return value;
}

function textField(raw, pointer, max, field) {
  if (!pointer) return null;
  const value = resolvePointer(raw, pointer);
  if (value === undefined || value === null) return null;
  if (typeof value !== 'string') throw new FieldError(rejected('type_mismatch', `/mapping/${field}`, '字段必须为字符串'));
  if ([...value].length > max) throw new FieldError(rejected('too_long', `/mapping/${field}`, '字段超出长度上限'));
  return value;
}

function timestampField(raw, pointer) {
  if (!pointer) return null;
  const value = resolvePointer(raw, pointer);
  if (value === undefined || value === null) return null;
  if (typeof value !== 'number' || !Number.isInteger(value) || value <= 0) {
    throw new FieldError(rejected('type_mismatch', '/mapping/timestamp', 'timestamp 必须为整数 Unix 毫秒'));
  }
  return value;
}

function requiredEvent(raw, pointer) {
  const value = resolvePointer(raw, pointer);
  if (value === undefined || value === null) return rejected('missing_event', '/mapping/event', '载荷中缺少事件名');
  if (typeof value !== 'string' || value === '') return rejected('type_mismatch', '/mapping/event', '事件名必须为非空字符串');
  if ([...value].length > CUSTOM_LIMITS.eventNameMax) return rejected('too_long', '/mapping/event', '字段超出长度上限');
  return value;
}

function presentValue(raw, pointer) {
  const value = resolvePointer(raw, pointer);
  return value !== undefined && value !== null && !(typeof value === 'string' && value === '');
}

function standardEvent(template, source, sessionId, roundId, ts, cwd, title, extra) {
  const base = {source, sessionId, roundId, ts, sourceLabel: template.name};
  if (cwd) base.cwd = cwd;
  if (title) base.title = title;
  return {...base, ...extra};
}

export function createCustomEngine({dedupPerSession = CUSTOM_LIMITS.dedupPerSession, sessionsMax = CUSTOM_LIMITS.sessionsMax} = {}) {
  const sessions = new Map();
  const order = [];
  const key = (source, sessionId) => `${source}:${sessionId}`;

  function store(id, state) {
    if (!sessions.has(id)) order.push(id);
    sessions.set(id, state);
    while (sessions.size > sessionsMax) {
      const oldest = order.shift();
      if (oldest !== undefined) sessions.delete(oldest);
    }
  }
  function newState(roundId) {
    return {roundId, active: true, seen: new Map(), seenOrder: [], pending: []};
  }
  function seen(state, roundId, eventId) {
    return state.seen.has(`${roundId}|${eventId}`);
  }
  function remember(state, roundId, eventId) {
    const id = `${roundId}|${eventId}`;
    if (state.seen.has(id)) return;
    state.seen.set(id, true);
    state.seenOrder.push(id);
    while (state.seenOrder.length > dedupPerSession) state.seen.delete(state.seenOrder.shift());
  }
  function pushPending(state, callId) {
    if (state.pending.includes(callId)) return;
    state.pending.push(callId);
    while (state.pending.length > PENDING_MAX) state.pending.shift();
  }
  function takePending(state, callId) {
    const index = state.pending.indexOf(callId);
    if (index < 0) return false;
    state.pending.splice(index, 1);
    return true;
  }

  function apply(template, raw, now) {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return rejected('invalid_payload', '', '载荷必须是 JSON 对象');
    const rawEvent = requiredEvent(raw, template.mapping.event);
    if (typeof rawEvent !== 'string') return rawEvent;
    const action = template.events[rawEvent];
    if (!action) return ignored('start', 'unknown_event', `未映射的事件 ${rawEvent}`, rawEvent, '');
    if ((template.ignoreIfPresent || []).some(pointer => presentValue(raw, pointer))) {
      return ignored(action.action, 'ignored_field_present', '命中 ignoreIfPresent，已忽略', rawEvent, '');
    }
    let sessionId, roundId, eventId, requestId, cwd, title, ts;
    try {
      sessionId = idField(raw, template.mapping.sessionId, CUSTOM_LIMITS.sessionIdMax, 'sessionId', '载荷中缺少会话 ID');
      if (sessionId === null) return rejected('missing_session_id', '/mapping/sessionId', '载荷中缺少会话 ID');
      roundId = idField(raw, template.mapping.roundId, CUSTOM_LIMITS.idFieldMax, 'roundId', '字段类型无效');
      eventId = idField(raw, template.mapping.eventId, CUSTOM_LIMITS.eventIdMax, 'eventId', '字段类型无效');
      requestId = idField(raw, template.mapping.requestId, CUSTOM_LIMITS.idFieldMax, 'requestId', '字段类型无效');
      cwd = textField(raw, template.mapping.cwd, CUSTOM_LIMITS.cwdMax, 'cwd');
      title = textField(raw, template.mapping.title, CUSTOM_LIMITS.titleMax, 'title');
      ts = timestampField(raw, template.mapping.timestamp);
    } catch (failure) {
      if (failure instanceof FieldError) return {...failure.outcome, event: rawEvent, sessionId: failure.outcome.path.endsWith('sessionId') ? null : sessionId ?? null};
      throw failure;
    }
    if (ts === null) ts = now;
    const source = customSource(template.id);
    const id = key(source, sessionId);
    const current = sessions.get(id);

    if (action.action === 'start') {
      const round = roundId ?? `custom:${ts}`;
      if (current && eventId && seen(current, round, eventId)) {
        return ignored('start', 'duplicate_event', '重复事件已忽略', rawEvent, sessionId, round);
      }
      if (current && current.roundId === round) {
        return ignored('start', 'duplicate_start', '同轮次的重复 start 已忽略', rawEvent, sessionId, round);
      }
      const state = newState(round);
      if (eventId) remember(state, round, eventId);
      store(id, state);
      return accepted('start', rawEvent, sessionId, round, [
        standardEvent(template, source, sessionId, round, ts, cwd, title, {type: 'start'}),
      ]);
    }

    if (!current) return ignored(action.action, 'no_active_round', '没有活跃轮次，事件未创建会话', rawEvent, sessionId);
    if (roundId !== null && roundId !== undefined && roundId !== current.roundId) {
      return ignored(action.action, 'late_round', '事件轮次与当前轮次不一致，已忽略', rawEvent, sessionId, current.roundId);
    }
    if (!current.active) {
      return ignored(action.action, 'round_ended', '轮次已结束，事件已忽略', rawEvent, sessionId, current.roundId);
    }
    const round = current.roundId;
    if (eventId && seen(current, round, eventId)) {
      return ignored(action.action, 'duplicate_event', '重复事件已忽略', rawEvent, sessionId, round);
    }
    let events;
    if (action.action === 'wait') {
      const callId = requestId ?? WAIT_CALL_ID;
      pushPending(current, callId);
      events = [standardEvent(template, source, sessionId, round, ts, cwd, title, {
        type: 'wait', callId, tool: 'custom', text: action.reason === 'permission' ? '等待权限确认' : '等待用户输入',
      })];
    } else if (action.action === 'resume') {
      const callId = requestId ?? WAIT_CALL_ID;
      if (!takePending(current, callId)) {
        return ignored('resume', 'no_pending_wait', '没有匹配的等待项，resume 已忽略', rawEvent, sessionId, round);
      }
      events = [standardEvent(template, source, sessionId, round, ts, cwd, title, {type: 'resolve', callId})];
    } else if (action.action === 'finish') {
      current.active = false;
      current.pending = [];
      events = [standardEvent(template, source, sessionId, round, ts, cwd, title, {type: 'end', status: action.status})];
    } else {
      current.active = false;
      current.pending = [];
      events = [standardEvent(template, source, sessionId, round, ts, cwd, title, {type: 'end', status: 'aborted', endedBy: rawEvent})];
    }
    if (eventId) remember(current, round, eventId);
    return accepted(action.action, rawEvent, sessionId, round, events);
  }

  return {
    apply,
    sessionCount: () => sessions.size,
    sessionState(source, sessionId) {
      const state = sessions.get(key(source, sessionId));
      return state ? {roundId: state.roundId, active: state.active, pending: state.pending.length} : null;
    },
    forget(id) {
      if (!sessions.delete(id)) return false;
      const index = order.indexOf(id);
      if (index >= 0) order.splice(index, 1);
      return true;
    },
    forgetSource(source) {
      const prefix = `${source}:`;
      let removed = 0;
      for (const id of [...sessions.keys()]) {
        if (!id.startsWith(prefix)) continue;
        sessions.delete(id);
        const index = order.indexOf(id);
        if (index >= 0) order.splice(index, 1);
        removed += 1;
      }
      return removed;
    },
  };
}
