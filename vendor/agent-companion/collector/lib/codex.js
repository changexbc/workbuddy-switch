// Codex mapping migrated from hy4's adapter, preserving explicit lifecycle events.
export function timestamp(value, fallback = Date.now()) { const n = typeof value === 'number' ? value : Date.parse(value); return Number.isFinite(n) && n > 0 ? n : fallback; }
function contentText(content) { return typeof content === 'string' ? content : Array.isArray(content) ? content.map(c => c.text || '').join('\n') : ''; }
export function questionDetails(input) {
  try {
    const p=typeof input==='string'?JSON.parse(input):input;
    return (Array.isArray(p?.questions)?p.questions:[]).map(q=>({
      text:String(q.question||q.title||''), header:String(q.header||''),
      options:(Array.isArray(q.options)?q.options:[]).map(o=>typeof o==='string'?{label:o,description:''}:{label:String(o.label||''),description:String(o.description||'')})
    }));
  } catch { return []; }
}
function question(input) { return questionDetails(input).map(q=>q.text).filter(Boolean).join('\n')||'等待用户输入'; }
export function codexRecord(rec, ctx, emit) {
  const p = rec.payload || {}, type = p.type, ts = timestamp(rec.timestamp);
  if (rec.type === 'session_meta') { ctx.sessionId = p.id || p.session_id || ctx.sessionId; ctx.cwd = p.cwd || ctx.cwd; }
  if (rec.type === 'turn_context') { ctx.cwd = p.cwd || ctx.cwd; ctx.roundId = p.turn_id || ctx.roundId; }
  const send = ev => emit({ source: 'codex', sessionId: ctx.sessionId, cwd: ctx.cwd, ts,
    ...(['wait','resolve'].includes(ev.type) && ctx.roundId ? {roundId:ctx.roundId} : {}), ...ev });
  if (rec.type === 'session_meta' || rec.type === 'turn_context') { send({ type: 'meta', roundId: ctx.roundId }); return; }
  if (type === 'task_started') { ctx.asyncQuestions = new Set(); ctx.roundId = p.turn_id || `turn:${ts}`; send({ type: 'start', roundId: ctx.roundId }); }
  else if (['task_complete','task_failed','turn_aborted'].includes(type)) send({ type: 'end', roundId: p.turn_id || ctx.roundId, status: type === 'task_complete' ? 'done' : type === 'turn_aborted' ? 'aborted' : 'error' });
  else if (type === 'user_message' || type === 'message' && p.role === 'user') {
    // User messages carry titles; task_started is the sole explicit round boundary.
    const text = p.message || contentText(p.content);
    if (text.trim() && !text.trim().startsWith('<') && !text.startsWith('The following is the Codex agent history')) send({ type: 'meta', title: text });
  } else if (['function_call','custom_tool_call'].includes(type)) {
    const name = p.name || 'tool', callId = p.call_id || p.id || `${ts}:${rec.ordinal ?? name}`;
    // Remember async calls only to suppress resolve events from their outputs.
    if (/(?:^|__|\.)request_user_input_async$/.test(name)) (ctx.asyncQuestions ||= new Set()).add(callId);
    if (/(?:^|__|\.)(request_user_input|AskUserQuestion|ask_user_question|RequestUserInput)$/.test(name)) {
      send({ type: 'wait', callId, tool: name, text: question(p.arguments || p.input), questions: questionDetails(p.arguments || p.input) });
    }
    else send({ type: 'step', eventId: callId, label: name });
  } else if (['function_call_output','custom_tool_call_output'].includes(type)) {const callId=p.call_id||p.id;if(!ctx.asyncQuestions?.has(callId))send({type:'resolve',callId});}
  else if (type === 'token_count') send({ type: 'tokens', tokens: p.info?.total_token_usage?.total_tokens });
  else if (type === 'reasoning' || type === 'message') send({ type: 'activity' });
}
export { question, contentText };
