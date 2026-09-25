import {FINISHED_HOLD_MS} from '../src/monitor/session-model.js';
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { Hub, STALE_MS } from '../collector/lib/hub.js';
import { Tailer } from '../collector/lib/tail.js';
import { codexRecord } from '../collector/lib/codex.js';
import { WorkBuddyPoller } from '../collector/lib/workbuddy.js';
import { CodeBuddyIdePoller } from '../collector/lib/codebuddy-ide.js';
import { defaultCodegDbPaths } from '../collector/lib/codeg.js';
import { createCollector } from '../collector/lib/collector.js';
import { startServer } from '../collector/server.js';
import { openDesktopSource } from '../collector/lib/open-app.js';
import { assignSessions, createNotificationTracker, COMPLETION_HOLD_MS } from '../src/monitor/model.js';
async function temp(t) { const dir=await fs.mkdtemp(path.join(os.tmpdir(),'astra-monitor-'));t.after(()=>fs.rm(dir,{recursive:true,force:true}));return dir; }
const store=()=>{const values=new Map();return {getItem:k=>values.get(k),setItem:(k,v)=>values.set(k,v)};};
const send=(hub,ev)=>hub.ingest({source:'codex',sessionId:'full-id',ts:1000,...ev});

test('explicit lifecycle, stable full identities, silence never completes, and no progress estimates',()=>{
 let now=1000;const hub=new Hub({now:()=>now});
 send(hub,{type:'start',roundId:'turn1'});send(hub,{type:'step',eventId:'call1',label:'read'});
 send(hub,{type:'step',eventId:'call1',label:'read'});
 now+=STALE_MS+1;
 assert.equal(hub.snapshot().sessions[0].status,'unknown');assert.equal(hub.snapshot().sessions[0].progress,null);assert.equal(hub.snapshot().sessions[0].steps.length,1);
 send(hub,{type:'wait',callId:'q1',tool:'ask',ts:now});send(hub,{type:'step',eventId:'parallel',ts:now});assert.equal(hub.snapshot().sessions[0].status,'wait');
 send(hub,{type:'resolve',callId:'other',ts:now});assert.equal(hub.snapshot().sessions[0].status,'wait');
 send(hub,{type:'resolve',callId:'q1',ts:now});assert.equal(hub.snapshot().sessions[0].status,'running');
 send(hub,{type:'end',status:'aborted',ts:now});assert.equal(hub.snapshot().sessions[0].status,'aborted');
 send(hub,{type:'start',roundId:'turn2',ts:now+1});assert.equal(hub.snapshot().sessions[0].roundId,'turn2');
 send(hub,{type:'end',roundId:'turn1',status:'done',ts:now+2});assert.equal(hub.snapshot().sessions[0].status,'running');
 send(hub,{type:'start',roundId:'another',sessionId:'full-id-other',ts:now});assert.equal(hub.snapshot().sessions.length,2);
});

test('Codex events pair questions and do not treat a tool failure as task failure',()=>{
 const hub=new Hub(),ctx={sessionId:'c',cwd:'/project'};const feed=(type,p,ts=1000)=>codexRecord({timestamp:ts,type,payload:p},ctx,e=>hub.ingest(e));
 feed('event_msg',{type:'task_started',turn_id:'t'});
 feed('response_item',{type:'function_call',name:'functions.request_user_input',call_id:'q',arguments:JSON.stringify({questions:[{title:'Choose a color'}]})});
 assert.equal(hub.snapshot().sessions[0].pending[0].text,'Choose a color');
 feed('response_item',{type:'function_call_output',call_id:'x',output:'ERROR: failed'});assert.equal(hub.snapshot().sessions[0].status,'wait');
 feed('response_item',{type:'function_call_output',call_id:'q',output:'blue'});assert.equal(hub.sessions.get('codex:c').status,'running');
 feed('event_msg',{type:'task_complete',turn_id:'t'});assert.equal(hub.snapshot().sessions[0].status,'done');
});

test('tail handles partial UTF-8, truncation, rotation and repeated polls',async t=>{
 const dir=await temp(t),file=path.join(dir,'log.jsonl');const tail=new Tailer(),got=[];
 const bytes=Buffer.from(JSON.stringify({text:'你好吗'})+'\n');const split=bytes.indexOf(Buffer.from('你'))+1;
 await fs.writeFile(file,bytes.subarray(0,split));await tail.pump(file,r=>got.push(r));assert.equal(got.length,0);
 await fs.appendFile(file,bytes.subarray(split));await tail.pump(file,r=>got.push(r));assert.equal(got[0].text,'你好吗');
 await tail.pump(file,r=>got.push(r));assert.equal(got.length,1);
 await fs.writeFile(file,'{"n":1}\n');await tail.pump(file,r=>got.push(r));assert.equal(got[1].n,1);
 await fs.rename(file,file+'.old');await fs.writeFile(file,'{"n":2}\n');await tail.pump(file,r=>got.push(r));assert.equal(got[2].n,2);
});

test('large Codex headers are read and sessions from a bounded tail remain identifiable',async t=>{
 const home=await temp(t),dir=path.join(home,'.codex/sessions');await fs.mkdir(dir,{recursive:true});
 const file=path.join(dir,'session.jsonl');
 const rows=[{type:'session_meta',timestamp:Date.now(),payload:{id:'full-codex-id',cwd:'/test',base_instructions:'x'.repeat(24000)}},{type:'event_msg',timestamp:Date.now(),payload:{type:'task_started',turn_id:'t'}},{type:'response_item',timestamp:Date.now(),payload:{type:'function_call',name:'read',call_id:'c'}}];
 await fs.writeFile(file,rows.map(r=>JSON.stringify(r)).join('\n')+'\n');
 const c=createCollector({home});
 c.ingestCodexHook({session_id:'full-codex-id',cwd:'/test',transcript_path:file,hook_event_name:'UserPromptSubmit',prompt:'Do work'});
 await c.poll();assert.equal(c.hub.snapshot().sessions[0].id,'codex:full-codex-id');assert.equal(c.hub.snapshot().sources.workbuddy.state,'ok');assert.equal(c.hub.snapshot().sources['codebuddy-ide'].state,'ok');assert.equal(c.hub.snapshot().sources.codeg.state,'partial');
});

test('Codex uses hook prompt labels without querying the sqlite title',async t=>{
 const home=await temp(t);await fs.mkdir(path.join(home,'.codex'),{recursive:true});
 const db=new DatabaseSync(path.join(home,'.codex/state_5.sqlite'));
 db.exec('CREATE TABLE threads (id TEXT, rollout_path TEXT, title TEXT, name TEXT, first_user_message TEXT)');
 db.prepare('INSERT INTO threads VALUES (?,?,?,?,?)').run('thr1','','raw prompt','添加下班驾车离场动画','long first message');
 db.close();
 const c=createCollector({home});
 await c.ingestCodexHook({session_id:'thr1',cwd:'/proj',hook_event_name:'UserPromptSubmit',prompt:'来自 Hook 的任务'});
 assert.equal(c.hub.snapshot().sessions[0].title,'来自 Hook 的任务');
});

test('Codex ignores idle jsonl until a hook names the live session',async t=>{
 const home=await temp(t),dir=path.join(home,'.codex/sessions');await fs.mkdir(dir,{recursive:true});
 const file=path.join(dir,'idle.jsonl');
 await fs.writeFile(file,JSON.stringify({type:'session_meta',timestamp:Date.now(),payload:{id:'idle',cwd:'/old'}})+'\n');
 const c=createCollector({home});await c.poll();
 assert.equal(c.hub.snapshot().sessions.some(s=>s.source==='codex'),false);
 c.ingestCodexHook({session_id:'idle',cwd:'/old',transcript_path:file,hook_event_name:'UserPromptSubmit',prompt:'Resume'});
 await c.poll();
 const session=c.hub.snapshot().sessions.find(s=>s.id==='codex:idle');
 assert.equal(session.status,'running');assert.equal(session.title,'Resume');
});

test('Codex anonymous approvals clear on model continuation, including checkpoint replay', () => {
 const hub = new Hub();
 const emit = ev => hub.ingest({source:'codex',sessionId:'resume',...ev});
 emit({type:'start',roundId:'r',ts:100});
 emit({type:'wait',callId:'perm:110',ts:110});
 emit({type:'wait',callId:'question',ts:111});
 emit({type:'tokens',tokens:10,ts:200});
 const ctx = {sessionId:'resume'};
 codexRecord({type:'response_item',timestamp:109,payload:{type:'reasoning'}},ctx,ev=>hub.ingest(ev));
 assert.equal(hub.sessions.get('codex:resume').pending.length,2);
 codexRecord({type:'response_item',timestamp:120,payload:{type:'reasoning'}},ctx,ev=>hub.ingest(ev));
 assert.deepEqual(hub.sessions.get('codex:resume').pending.map(p=>p.id),['question']);
 assert.equal(hub.sessions.get('codex:resume').status,'wait');
 emit({type:'resolve',callId:'question',ts:201});
 assert.equal(hub.sessions.get('codex:resume').status,'running');
 emit({type:'wait',callId:'perm:210',ts:210});
 emit({type:'tokens',ts:250});
 emit({type:'activity',ts:220});
 assert.equal(hub.sessions.get('codex:resume').status,'running');
 assert.equal(hub.sessions.get('codex:resume').updatedAt,250);
});

test('Codex permission checks are neutral and Stop completes without scanning logs',async t=>{
 const home=await temp(t),c=createCollector({home});
 c.ingestCodexHook({session_id:'live',cwd:'/proj',hook_event_name:'UserPromptSubmit',prompt:'Ship it'});
 await c.poll();assert.equal(c.hub.snapshot().sessions[0].status,'running');
 c.ingestCodexHook({session_id:'live',cwd:'/proj',hook_event_name:'PermissionRequest',tool_name:'Bash',tool_use_id:'p1'});
 await c.poll();assert.equal(c.hub.snapshot().sessions[0].status,'running');
 assert.equal(c.hub.snapshot().sessions[0].permissionChecks.length,1);
 c.ingestCodexHook({session_id:'live',cwd:'/proj',hook_event_name:'Stop'});
 await c.poll();assert.equal(c.hub.snapshot().sessions[0].status,'done');
});

test('WorkBuddy hooks: pending ask, paired reply, stop, and no historical restore',async t=>{
 const hub=new Hub(),poller=new WorkBuddyPoller(hub,{home:await temp(t)});
 const start=Date.now();
 poller.ingestHook({session_id:'s',cwd:'/work',timestamp:start,hook_event_name:'UserPromptSubmit',prompt:'Do work'});
 poller.ingestHook({session_id:'s',cwd:'/work',timestamp:start+1,hook_event_name:'PreToolUse',tool_name:'AskUserQuestion',tool_use_id:'ask1',tool_input:{questions:[{question:'Continue?'}]}});
 assert.equal(hub.snapshot().sessions[0].status,'wait');assert.equal(hub.snapshot().sessions[0].pending[0].id,'ask1');
 poller.ingestHook({session_id:'s',timestamp:start+2,hook_event_name:'PostToolUse',tool_name:'AskUserQuestion',tool_use_id:'ask1'});
 assert.equal(hub.snapshot().sessions[0].status,'running');
 poller.ingestHook({session_id:'s',timestamp:start+3,hook_event_name:'Stop'});
 assert.equal(hub.snapshot().sessions[0].status,'done');
 const again=new Hub();await new WorkBuddyPoller(again,{home:await temp(t)}).poll();
 assert.equal(again.snapshot().sessions.length,0);
});

function writeCodeBuddyDb(dbPath, sessions) {
  const db=new DatabaseSync(dbPath);
  db.exec('CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)');
  const ins=db.prepare('INSERT INTO ItemTable VALUES (?,?)');
  for(const row of sessions)ins.run(`session:${row.conversationId}`,JSON.stringify(row));
  db.close();
}

test('Codeg desktop db lives under Application Support/app.codeg',()=>{
 const paths=defaultCodegDbPaths('/Users/apple');
 assert.equal(paths[0],'/Users/apple/Library/Application Support/app.codeg/codeg.db');
 assert.ok(paths.includes('/Users/apple/Library/Application Support/codeg/codeg.db'));
});

test('Codeg-hosted Codex threads stay on the Codeg seat instead of a Codex duplicate',()=>{
 const hub=new Hub({now:()=>2000});
 hub.ingest({source:'codeg',sessionId:'106',type:'start',roundId:'observed:1',ts:2000,cwd:'/proj',title:'test',folderId:32,agentType:'codex',externalId:'01a09098-a704-7293-a232-f1c74190e489',webPort:3080});
 hub.ingest({source:'codex',sessionId:'01a09098-a704-7293-a232-f1c74190e489',type:'start',roundId:'turn',ts:2001,cwd:'/proj',title:'test'});
 const sessions=hub.snapshot().sessions;
 assert.equal(sessions.length,1);
 assert.equal(sessions[0].id,'codeg:106');
 assert.equal(sessions[0].agentType,'codex');
 assert.equal(sessions[0].folderId,32);
 hub.ingest({source:'codex',sessionId:'native-cli',type:'start',roundId:'turn',ts:2002,cwd:'/other'});
 assert.deepEqual(hub.snapshot().sessions.map(s=>s.id).sort(),['codeg:106','codex:native-cli']);
});

test('createCollector ignores CodeBuddy database and accepts IDE hooks',async t=>{
 const home=await temp(t),now=Date.now();
 const dbDir=path.join(home,'Library/Application Support/CodeBuddy CN');await fs.mkdir(dbDir,{recursive:true});
 writeCodeBuddyDb(path.join(dbDir,'codebuddy-sessions.vscdb'),[{conversationId:'old',cwd:'/proj',status:'Working',createdAt:now,updatedAt:now}]);
 const c=createCollector({home});await c.poll();
 assert.equal(c.hub.snapshot().sessions.filter(s=>s.source==='codebuddy-ide').length,0);
 await c.ingestCodebuddyIdeHook({client:'CodeBuddyIDE',session_id:'ide-1',hook_event_name:'UserPromptSubmit',generation_id:'g',prompt:'Hook title'});
 assert.equal(c.hub.snapshot().sessions.find(s=>s.source==='codebuddy-ide').status,'running');
 const settings=c.getSettings();settings.sources['codebuddy-ide'].enabled=false;await c.updateSettings(settings);
 await c.ingestCodebuddyIdeHook({client:'CodeBuddyIDE',session_id:'ide-1',hook_event_name:'UserPromptSubmit'});
 assert.equal(c.hub.snapshot().sessions.filter(s=>s.source==='codebuddy-ide').length,0);
});

test('notifications: initial history suppressed, unresolved ask once, refresh/reconnect dedup',()=>{
 const hub=new Hub({now:()=>1000}),storage=store();send(hub,{type:'start',roundId:'t'});send(hub,{type:'end',status:'done'});hub.ready=true;
 const tracker=createNotificationTracker({storage});assert.equal(tracker.ingest(hub.snapshot()).length,0);
 send(hub,{type:'start',roundId:'t2',ts:1001});send(hub,{type:'wait',callId:'q',tool:'ask',ts:1002});assert.equal(tracker.ingest(hub.snapshot()).length,1);
 assert.equal(tracker.ingest(hub.snapshot()).length,0);assert.equal(createNotificationTracker({storage}).ingest(hub.snapshot()).length,0);
 send(hub,{type:'resolve',callId:'q',ts:1003});send(hub,{type:'end',status:'done',ts:1004});assert.equal(tracker.ingest(hub.snapshot()).length,1);
 const unseen=createNotificationTracker({storage:store()});assert.equal(unseen.ingest(hub.snapshot()).length,0);
});

test('eight slots stay stable across sorting, overflow stays in data and fresh wait gets released slot',()=>{
 const sessions=Array.from({length:10},(_,i)=>({id:String(i),status:'running',updatedAt:1000-i}));const slots=assignSessions([],sessions,2000);
 assert.equal(slots.length,8);assert.equal(new Set(slots).size,8);
 assert.deepEqual(assignSessions(slots,[...sessions].reverse(),2000),slots);
 sessions[0].status='done';sessions[0].endedAt=1000;sessions[9].status='wait';
 assert.equal(assignSessions(slots,sessions,10000)[0],'9');assert.equal(sessions.length,10);
});

test('openDesktopSource only launches allowlisted desktop bundles',async()=>{
 const opened=[];
 await openDesktopSource('codeg',{openApp:id=>opened.push(id)});
 assert.deepEqual(opened,['app.codeg']);
 await openDesktopSource('codebuddy-ide',{openApp:id=>opened.push(id)});
 assert.deepEqual(opened,['app.codeg','com.tencent.codebuddy']);
 await openDesktopSource('codebuddy-ide',{openApp:id=>opened.push(id),edition:'domestic'});
 assert.deepEqual(opened,['app.codeg','com.tencent.codebuddy','com.tencent.codebuddycn']);
 await assert.rejects(()=>openDesktopSource('codex',{openApp:id=>opened.push(id)}),error=>error.code==='UNSUPPORTED_SOURCE');
});

test('HTTP serves only dist, blocks cross origin/host access and streams versioned snapshots',async t=>{
 const root=await temp(t);await fs.writeFile(path.join(root,'desktop.html'),'safe');const hub=new Hub();hub.ready=true;
 const hooks=[];const opened=[];
 const runtime=await startServer({port:0,staticRoot:root,openApp:bundleId=>opened.push(bundleId),collector:{hub,ingestCodexHook:p=>hooks.push(p),ingestWorkbuddyHook:p=>hooks.push({...p,via:'workbuddy'}),ingestCodebuddyIdeHook:p=>hooks.push({...p,via:'ide'}),async start(){},stop(){}}});t.after(()=>runtime.close());
 const base=`http://127.0.0.1:${runtime.server.address().port}`;
 assert.equal(await (await fetch(base)).text(),'safe');assert.equal((await fetch(base+'/collector/server.js')).status,404);
 assert.equal((await fetch(base+'/api/state',{headers:{Origin:'https://evil.example'}})).status,403);
 assert.equal((await (await fetch(base+'/api/state')).json()).version,1);
 assert.equal((await fetch(base+'/api/codex-hook',{method:'POST',headers:{Origin:'https://evil.example'},body:'{}'})).status,403);
 assert.equal((await fetch(base+'/api/codex-hook',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({session_id:'h1',hook_event_name:'Stop'})})).status,204);
 assert.equal(hooks[0].session_id,'h1');
 assert.equal((await fetch(base+'/api/workbuddy-hook',{method:'POST',headers:{Origin:'https://evil.example'},body:'{}'})).status,403);
 assert.equal((await fetch(base+'/api/workbuddy-hook',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({session_id:'w1',hook_event_name:'Stop'})})).status,204);
 assert.equal(hooks[1].session_id,'w1');assert.equal(hooks[1].via,'workbuddy');
 assert.equal((await fetch(base+'/api/codebuddy-ide-hook',{method:'POST',headers:{Origin:'https://evil.example'},body:'{}'})).status,403);
 assert.equal((await fetch(base+'/api/codebuddy-ide-hook',{method:'POST',body:JSON.stringify({session_id:'i1',hook_event_name:'Stop'})})).status,204);
 assert.equal(hooks[2].session_id,'i1');assert.equal(hooks[2].via,'ide');
 const abort=new AbortController();const res=await fetch(base+'/events',{signal:abort.signal});const reader=res.body.getReader();const chunk=await reader.read();assert.match(new TextDecoder().decode(chunk.value),/data:.*"version":1/);abort.abort();
 assert.equal((await fetch(base+'/api/open-session?source=other',{method:'POST'})).status,400);
 assert.deepEqual(await (await fetch(base+'/api/open-session?source=codeg&sessionId=214',{method:'POST'})).json(),{ok:true,app:'codeg'});
 assert.deepEqual(await (await fetch(base+'/api/open-session?source=codebuddy-ide',{method:'POST'})).json(),{ok:true,app:'codebuddy-ide'});
 assert.deepEqual(await (await fetch(base+'/api/open-session?source=codebuddy-ide&edition=domestic',{method:'POST'})).json(),{ok:true,app:'codebuddy-ide'});
 assert.deepEqual(opened,['app.codeg','com.tencent.codebuddy','com.tencent.codebuddycn']);
});

test('bounded event history never hides unresolved questions and unknown bindings do not churn',()=>{
 const hub=new Hub({now:()=>1000});send(hub,{type:'start',roundId:'t'});send(hub,{type:'wait',callId:'ask',tool:'ask'});
 for(let i=0;i<510;i++){send(hub,{sessionId:'history',type:'start',roundId:'t'+i,ts:1001+i});send(hub,{sessionId:'history',type:'end',status:'done',ts:1001+i});}
 hub.ready=true;assert.equal(createNotificationTracker().ingest(hub.snapshot()).filter(x=>x.kind==='wait').length,1);
 const sessions=Array.from({length:10},(_,i)=>({id:String(i),status:'unknown',updatedAt:10-i}));const slots=assignSessions([],sessions,90000);
 assert.deepEqual(assignSessions(slots,[...sessions].reverse(),100000),slots);
});

test('WorkBuddy resumes in a new stable round and cannot retain an obsolete ask',()=>{
 const hub=new Hub(),poller=new WorkBuddyPoller(hub,{home:'/unused'});const now=Date.now();
 poller.ingestHook({session_id:'s',cwd:'/test',timestamp:now,hook_event_name:'UserPromptSubmit',prompt:'first',turn_id:'r1'});
 poller.ingestHook({session_id:'s',timestamp:now+1,hook_event_name:'PreToolUse',tool_name:'AskUserQuestion',tool_use_id:'old',turn_id:'r1'});
 assert.equal(hub.snapshot().sessions[0].status,'wait');
 poller.ingestHook({session_id:'s',cwd:'/test',timestamp:now+2,hook_event_name:'UserPromptSubmit',prompt:'second',turn_id:'r2'});
 const s=hub.snapshot().sessions[0];assert.equal(s.roundId,'r2');assert.equal(s.status,'running');assert.equal(s.pending.length,0);
});

test('asynchronous Codex records never wait or resolve a synchronous question',()=>{
 const hub=new Hub(),ctx={sessionId:'async'},events=[];
 const feed=p=>codexRecord({timestamp:Date.now(),type:'response_item',payload:p},ctx,e=>{events.push(e);hub.ingest(e)});
 feed({type:'task_started',turn_id:'a'});
 for(const name of ['request_user_input_async','functions.request_user_input_async','mcp__codex__request_user_input_async']) {
  feed({type:'function_call',name,call_id:name});
  assert.equal(hub.snapshot().sessions[0].status,'running');
  feed({type:'function_call_output',call_id:name,output:'queued'});
 }
 assert.equal(events.filter(e=>['wait','resolve'].includes(e.type)).length,0);
 feed({type:'function_call',name:'functions.request_user_input',call_id:'sync'});
 feed({type:'custom_tool_call',name:'functions.request_user_input_async',call_id:'parallel'});
 feed({type:'custom_tool_call_output',call_id:'parallel',output:'queued'});
 feed({type:'message',role:'user',content:[{text:'A follow-up'}]});
 assert.deepEqual(hub.snapshot().sessions[0].pending.map(p=>p.id),['sync']);
 assert.equal(events.filter(e=>e.type==='resolve').length,0);
 feed({type:'function_call_output',call_id:'sync',output:'answered'});
 assert.equal(hub.snapshot().sessions[0].status,'running');
});

test('WorkBuddy questions preserve option labels and descriptions through the monitor snapshot',async()=>{
 const {questionDetails}=await import('../collector/lib/codex.js');
 const questions=questionDetails(JSON.stringify({questions:[{question:'请选择',options:[{label:'选项 A',description:'保留当前设置'},'选项 B']}]}));
 const hub=new Hub();send(hub,{type:'start',roundId:'options'});send(hub,{type:'wait',callId:'q',questions,text:'请选择'});
 const pending=hub.snapshot().sessions[0].pending;assert.equal(pending[0].questions[0].options[0].description,'保留当前设置');
 assert.deepEqual(pending[0].questions[0].options.map(option => option.label),['选项 A','选项 B']);
});

test('only active rounds get new seats; terminal rounds release after the completion hold without rebinding',()=>{
 const history=['done','error','aborted','unknown'].map((status,i)=>({id:`h${i}`,status,updatedAt:1000,endedAt:1000}));
 assert.deepEqual(assignSessions([],history,1100),Array(8).fill(null));
 for(const status of ['done','error','aborted']){
  const session={id:'s',status,updatedAt:1000,endedAt:1000};
  assert.deepEqual(assignSessions(['s'],[session],1000+COMPLETION_HOLD_MS-1,1),['s']);
  const released=assignSessions(['s'],[session],1000+COMPLETION_HOLD_MS,1);assert.deepEqual(released,[null]);
  assert.deepEqual(assignSessions(released,[session],1000+COMPLETION_HOLD_MS+1000,1),[null]);
 }
 for(const status of ['wait','running','unknown','offline'])assert.deepEqual(assignSessions(['s'],[{id:'s',status,updatedAt:1}],9999999,1),['s']);
 assert.deepEqual(assignSessions(['s'],[{id:'s',status:'idle',updatedAt:1}],1000,1),[null]);
 assert.deepEqual(assignSessions(['s'],[{id:'s',status:'done',endedAt:0,updatedAt:0}],1000,1,new Map(),new Set(['s'])),[null]);
});

test('free seats precede completion preemption; resumed sessions prefer their previous free resident',()=>{
 const completed={id:'old',status:'done',endedAt:1000,updatedAt:1000},fresh={id:'new',status:'running',updatedAt:1100};
 assert.deepEqual(assignSessions(['old',null],[completed,fresh],1200,2),['old','new']);
 assert.deepEqual(assignSessions(['old'],[completed,fresh],1200,1),['new']);
 assert.deepEqual(assignSessions([null,null],[fresh],1200,2,new Map([['new',1]])),[null,'new']);
 assert.deepEqual(assignSessions(['busy',null],[fresh,{id:'busy',status:'wait',updatedAt:1}],1200,2,new Map([['new',0]])),['busy','new']);
});

test('Codex hook install merges without dropping existing project hooks',async t=>{
 const {installCodexHooks,HOOK_EVENTS,HOOK_SCRIPT_NAME}=await import('../collector/lib/codex-live.js');
 const home=await temp(t);await fs.mkdir(path.join(home,'.codex'),{recursive:true});
 await fs.writeFile(path.join(home,'.codex','hooks.json'),JSON.stringify({hooks:{Stop:[{hooks:[{type:'command',command:'echo keep'}]}]}}));
 const {hooksPath}=await installCodexHooks(home,{monitorUrl:'http://127.0.0.1:8850'});
 const doc=JSON.parse(await fs.readFile(hooksPath,'utf8'));
 assert.equal(doc.hooks.Stop.some(g=>JSON.stringify(g).includes('echo keep')),true);
 for(const event of HOOK_EVENTS)assert.equal(doc.hooks[event].some(g=>JSON.stringify(g).includes(HOOK_SCRIPT_NAME)),true);
 assert.equal(doc.hooks.SessionEnd.find(g=>JSON.stringify(g).includes(HOOK_SCRIPT_NAME)).hooks[0].async,undefined);
 assert.equal(doc.hooks.Stop.find(g=>JSON.stringify(g).includes(HOOK_SCRIPT_NAME)).hooks[0].async,undefined);
 assert.equal(await fs.readFile(path.join(home,'.codex/hooks/astra-office-monitor.url'),'utf8'),'http://127.0.0.1:8850');
});

test('legacy installer yields to native hooks and preserves trust positions and unrelated handlers',async t=>{
 const {installCodexHooks,HOOK_EVENTS}=await import('../collector/lib/codex-live.js');
 const home=await temp(t);const dir=path.join(home,'.codex');await fs.mkdir(dir,{recursive:true});
 const native={type:'command',command:"'/tmp/agent-studio-runtime-v1' hook --home '/tmp'",statusMessage:'Agent Studio'};
 const keep={type:'command',command:'echo keep'};
 const hooks=Object.fromEntries(HOOK_EVENTS.map(event=>[event,[{hooks:[native]},{matcher:'*',hooks:[{type:'command',command:'/usr/bin/python3 /tmp/astra-office-status.py'},keep]}]]));
 const file=path.join(dir,'hooks.json');await fs.writeFile(file,JSON.stringify({hooks}));
 assert.equal((await installCodexHooks(home)).native,true);
 const first=await fs.readFile(file,'utf8');const cleaned=JSON.parse(first);
 for(const event of HOOK_EVENTS){assert.deepEqual(cleaned.hooks[event][0].hooks,[native]);assert.deepEqual(cleaned.hooks[event][1],{matcher:'*',hooks:[keep]});}
 const stat=await fs.stat(file);await installCodexHooks(home);
 assert.equal(await fs.readFile(file,'utf8'),first);
 assert.equal((await fs.stat(file)).mtimeMs,stat.mtimeMs);
 await assert.rejects(fs.access(path.join(dir,'hooks/astra-office-status.py')));
});


test('Codex plan question survives a later hook and resolves despite newer telemetry', () => {
 const hub=new Hub(),ctx={sessionId:'plan',roundId:'r'};
 const emit=ev=>hub.ingest({source:'codex',sessionId:'plan',...ev});
 emit({type:'start',roundId:'r',ts:133});
 const feed=(timestamp,payload)=>codexRecord({type:'response_item',timestamp,payload},ctx,e=>hub.ingest(e));
 feed(100,{type:'function_call',name:'request_user_input',call_id:'q',arguments:JSON.stringify({questions:[{question:'选一个',options:[{label:'A'}]}]})});
 assert.equal(hub.sessions.get('codex:plan').status,'wait');
 assert.equal(hub.sessions.get('codex:plan').pending[0].questions[0].options[0].label,'A');
 emit({type:'start',roundId:'r',ts:140});
 assert.equal(hub.sessions.get('codex:plan').status,'wait');
 emit({type:'tokens',ts:200});
 feed(150,{type:'function_call_output',call_id:'q',output:'answer'});
 assert.equal(hub.sessions.get('codex:plan').status,'running');
 assert.equal(hub.sessions.get('codex:plan').updatedAt,200);
 emit({type:'start',roundId:'new',ts:300});
 feed(250,{type:'function_call',name:'request_user_input',call_id:'old'});
 assert.equal(hub.sessions.get('codex:plan').pending.length,0);
});
