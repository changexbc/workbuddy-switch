import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {Hub} from '../collector/lib/hub.js';
import {CodexReadStateObserver} from '../collector/lib/codex-read-state.js';
const doc=(ids,identity='account',host='local:machine')=>JSON.stringify({'electron-thread-read-state-v1':{version:1,unreadByIdentity:{[identity]:{[host]:ids}}}});
async function setup(t){
 const dir=await fs.mkdtemp(path.join(os.tmpdir(),'codex-read-state-'));t.after(()=>fs.rm(dir,{recursive:true,force:true}));
 const file=path.join(dir,'state.json');let now=1000;const counts={stat:0,open:0};
 const io={stat:async(...args)=>{counts.stat++;return fs.stat(...args)},open:async(...args)=>{counts.open++;return fs.open(...args)}};
 const observer=new CodexReadStateObserver(file,{io,now:()=>now}),hub=new Hub();
 const finish=(round='r')=>{hub.ingest({source:'codex',sessionId:'x',roundId:round,type:'start',ts:now});hub.ingest({source:'codex',sessionId:'x',roundId:round,type:'end',status:'done',ts:now});};
 return {file,counts,observer,hub,finish,tick:()=>{now+=60_000}};
}
test('unread observation is idle without completions, throttled, and reads only changed files',async t=>{
 const x=await setup(t);await fs.writeFile(x.file,doc(['x']));await x.observer.poll(x.hub);assert.deepEqual(x.counts,{stat:0,open:0});
 x.finish();await x.observer.poll(x.hub);assert.deepEqual(x.counts,{stat:1,open:1});
 await x.observer.poll(x.hub);assert.deepEqual(x.counts,{stat:1,open:1});
 x.tick();await x.observer.poll(x.hub);assert.deepEqual(x.counts,{stat:2,open:1});
 await fs.writeFile(x.file,doc([]));x.tick();await x.observer.poll(x.hub);
 assert.equal(x.hub.sessions.get('codex:x').viewedRoundId,'r');
 const counts={...x.counts};x.tick();await x.observer.poll(x.hub);assert.deepEqual(x.counts,counts);
});
test('initial absence, remote state, account change, and malformed file never imply read',async t=>{
 const x=await setup(t);x.finish();await fs.writeFile(x.file,doc([]));await x.observer.poll(x.hub);assert.equal(x.hub.sessions.get('codex:x').viewedRoundId,undefined);
 x.tick();await fs.writeFile(x.file,doc(['x']));await x.observer.poll(x.hub);
 x.tick();await fs.writeFile(x.file,doc([],'other-account'));await x.observer.poll(x.hub);assert.equal(x.hub.sessions.get('codex:x').viewedRoundId,undefined);
 x.tick();await fs.writeFile(x.file,doc(['x']));await x.observer.poll(x.hub);
 x.tick();await fs.writeFile(x.file,'{broken');await x.observer.poll(x.hub);
 x.tick();await fs.writeFile(x.file,doc([]));await x.observer.poll(x.hub);assert.equal(x.hub.sessions.get('codex:x').viewedRoundId,undefined);
 x.tick();await fs.writeFile(x.file,doc(['x'],'account','remote:machine'));await x.observer.poll(x.hub);
 x.tick();await fs.writeFile(x.file,doc([],'account','remote:machine'));await x.observer.poll(x.hub);assert.equal(x.hub.sessions.get('codex:x').viewedRoundId,undefined);
});
test('an old round observation cannot hide a newer completion',async t=>{
 const x=await setup(t);x.finish();await fs.writeFile(x.file,doc(['x']));await x.observer.poll(x.hub);
 x.tick();x.finish('new');await fs.writeFile(x.file,doc([]));await x.observer.poll(x.hub);
 assert.notEqual(x.hub.sessions.get('codex:x').viewedRoundId,'new');
});
