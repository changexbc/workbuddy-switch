import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {Hub} from '../collector/lib/hub.js';
import {CodexLivePoller} from '../collector/lib/codex-live.js';

test('pure Codex hook lifecycle handles answers, approvals, concurrency, ordering, and interruption without file reads',async t=>{
 const fixture=JSON.parse(await fs.readFile(new URL('./fixtures/codex-hooks.json',import.meta.url),'utf8'));
 const home=await fs.mkdtemp(path.join(os.tmpdir(),'codex-hook-only-'));t.after(()=>fs.rm(home,{recursive:true,force:true}));
 const hub=new Hub(),poller=new CodexLivePoller(hub,{home});
 const methods=['readFile','open','stat','access','readdir'];const originals=Object.fromEntries(methods.map(k=>[k,fs[k]]));
 try {
  for(const k of methods)fs[k]=()=>{throw Error(`Unexpected filesystem access: ${k}`)};
  for(const [i,c] of fixture.entries()) {
   poller.ingestHook({session_id:'x',turn_id:'r',cwd:'/project',timestamp:Date.now()+i,transcript_path:'/must-not-read.jsonl',...c.hook});
   await poller.poll();
   assert.equal(hub.sessions.get('codex:x').status,c.status,`case ${i}`);
   assert.equal(hub.sessions.get('codex:x').pending.length,c.pending,`case ${i}`);
  }
  const restarted=new CodexLivePoller(new Hub(),{home});await restarted.poll();assert.equal(restarted.hub.sessions.size,0);
 } finally {for(const k of methods)fs[k]=originals[k];}
});

test('permission checks are neutral, silent, and do not hide a real question', async()=>{
 const {sessionPresentation}=await import('../src/monitor/presentation.js');
 const {createNotificationTracker}=await import('../src/monitor/model.js');
 const hub=new Hub();hub.ready=true;
 const tracker=createNotificationTracker();tracker.ingest(hub.snapshot());
 const poller=new CodexLivePoller(hub,{home:'/unused'});
 let ts=Date.now();const hook=(event,extra={})=>poller.ingestHook({session_id:'x',turn_id:'r',hook_event_name:event,timestamp:++ts,...extra});
 hook('UserPromptSubmit');
 hook('PermissionRequest',{tool_name:'Bash',tool_use_id:'a'});
 let session=hub.sessions.get('codex:x');
 assert.equal(session.status,'running');assert.equal(sessionPresentation(session).statusLabel,'权限检查中');
 assert.deepEqual(tracker.ingest(hub.snapshot()),[]);assert.equal(session.pending.length,0);
 hook('PreToolUse',{tool_name:'request_user_input',tool_use_id:'q'});
 assert.equal(sessionPresentation(session).statusLabel,'待确认');
 assert.equal(tracker.ingest(hub.snapshot()).filter(e=>e.kind==='wait').length,1);
 hook('PostToolUse',{tool_name:'Bash',tool_use_id:'a'});
 assert.equal(session.status,'wait');assert.equal(session.permissionChecks.length,0);
 hook('PostToolUse',{tool_name:'request_user_input',tool_use_id:'q'});
 assert.equal(sessionPresentation(session).statusLabel,'运行中');
 hook('PermissionRequest',{tool_name:'Bash',tool_use_id:'b'});
 hook('Stop');assert.equal(session.permissionChecks.length,0);
});

test('optional async questions never notify or clear an existing synchronous wait', async()=>{
 const {createNotificationTracker}=await import('../src/monitor/model.js');
 const hub=new Hub();hub.ready=true;
 const tracker=createNotificationTracker();tracker.ingest(hub.snapshot());
 const poller=new CodexLivePoller(hub,{home:'/unused'});
 let ts=Date.now();const hook=(event,tool='',id='')=>poller.ingestHook({session_id:'x',turn_id:'r',hook_event_name:event,tool_name:tool,tool_use_id:id,timestamp:++ts});
 hook('UserPromptSubmit');
 for(const name of ['request_user_input_async','functions.request_user_input_async','mcp__codex__request_user_input_async']) {
  for(const event of ['PreToolUse','PostToolUse','PreToolUse']) {
   hook(event,name,name);
   assert.equal(hub.sessions.get('codex:x').status,'running');
   assert.deepEqual(hub.sessions.get('codex:x').pending,[]);
   assert.deepEqual(tracker.ingest(hub.snapshot()),[]);
   assert.deepEqual(hub.snapshot().events,[]);
  }
 }
 hook('PreToolUse','functions.request_user_input','sync');
 assert.equal(tracker.ingest(hub.snapshot()).filter(e=>e.kind==='wait').length,1);
 for(const event of ['PreToolUse','PostToolUse','PreToolUse']) {
  hook(event,'functions.request_user_input_async','parallel');
  assert.equal(hub.sessions.get('codex:x').status,'wait');
  assert.deepEqual(hub.sessions.get('codex:x').pending.map(p=>p.id),['sync']);
  assert.deepEqual(tracker.ingest(hub.snapshot()),[]);
 }
 hook('PostToolUse','functions.request_user_input','sync');
 assert.equal(hub.sessions.get('codex:x').status,'running');
});
