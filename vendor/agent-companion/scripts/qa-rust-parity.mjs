import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { Hub } from '../collector/lib/hub.js';
const t = Date.now();
const events = [
  { type:'start', roundId:'r1', ts:t },
  { type:'step', eventId:'s', label:'read', ts:t+1 },
  { type:'wait', callId:'q', tool:'ask', text:'选择', questions:[], ts:t+2 },
  { type:'resolve', callId:'q', ts:t+3 },
  { type:'tokens', tokens:1234, ts:t+4 },
  { type:'end', roundId:'r1', status:'done', ts:t+5 },
  { type:'start', roundId:'r2', ts:t+6 },
  { type:'end', roundId:'r1', status:'error', ts:t+7 },
  { type:'wait', callId:'q2', tool:'ask', text:'继续？', ts:t+8 },
  { type:'end', roundId:'r1', status:'aborted', endedBy:'host', ts:t+9, source:'workbuddy', sessionId:'w' },
  { type:'start', roundId:'r2', ts:t+10, source:'workbuddy', sessionId:'w' },
].map(e=>({source:'codex',sessionId:'x',cwd:'/project',title:'Task',...e}));
const hub = new Hub(); hub.startedAt=1; hub.ready=true;
for(const e of events) hub.ingest(e);
const rust = JSON.parse(execFileSync('cargo',['run','--quiet','-p','agent-studio-core','--example','replay'],{input:JSON.stringify(events),encoding:'utf8'}));
const stable = snap => ({sessions:snap.sessions.map(({elapsed,...s})=>s),events:snap.events});
assert.deepEqual(stable(rust),stable(hub.snapshot()));
console.log('Rust/Node snapshot parity passed: rounds, waits, completion, stale replay, tokens');
