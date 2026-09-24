import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {createCollector} from '../collector/lib/collector.js';
import {startServer} from '../collector/server.js';
import {CUSTOM_LIMITS,validateTemplate,createCustomEngine,isCustomId,customSource} from '../collector/lib/custom.js';
import {parseCustomStore} from '../collector/lib/custom-store.js';

const fixture=JSON.parse(await fs.readFile(new URL('./fixtures/custom-hooks.json',import.meta.url)));

async function tempHome(t){
 const home=await fs.mkdtemp(path.join(os.tmpdir(),'custom-hooks-'));
 t.after(()=>fs.rm(home,{recursive:true,force:true}));
 return home;
}
function collectorFor(home){return createCollector({home});}
function applyPatch(value,steps){
 const target=structuredClone(value);
 for(const step of steps){
  const tokens=step.pointer.split('/').slice(1);
  let cursor=target;
  for(const token of tokens.slice(0,-1)){
   cursor=Array.isArray(cursor)?cursor[Number(token)]:cursor[token];
  }
  const last=tokens.at(-1);
  if(step.op==='set')cursor[last]=structuredClone(step.value);
  else delete cursor[last];
 }
 return target;
}
async function importTemplates(collector,data){
 for(const template of Object.values(data.templates))await collector.customIntegrationsSet({action:'import',template});
}
const integrationId=(data,key)=>{
 const id=data.templates[key]?.id;
 if(!id)throw Error(`unknown template ${key}`);
 return id;
};
const applyPayload=(collector,data,step)=>collector.ingestCustomHook({integration:integrationId(data,step.template),payload:structuredClone(step.payload)});

test('limits match the shared fixture',()=>{
 assert.deepEqual(CUSTOM_LIMITS,fixture.limits);
});

test('template validation matches the shared fixture',()=>{
 for(const entry of fixture.invalid){
  const result=validateTemplate(applyPatch(fixture.base,entry.mutate));
  assert.equal(result.ok,false,`${entry.name} should be rejected`);
  assert.equal(result.path,entry.path,`${entry.name} reported the wrong field`);
 }
 for(const [name,template] of Object.entries(fixture.templates)){
  const result=validateTemplate(template);
  assert.equal(result.ok,true,`template ${name} must be valid: ${result.path}${result.message}`);
 }
});

test('payload cases match the shared fixture',()=>{
 for(const entry of fixture.cases){
  const engine=createCustomEngine();
  let outcome=null;
  for(const step of entry.payloads)outcome=engine.apply(validateTemplate(fixture.templates[step.template]).template,step.payload,1700000000000);
  const expect=entry.expect;
  assert.equal(outcome.outcome,expect.outcome,`case ${entry.name} outcome`);
  if(expect.reason!==undefined)assert.equal(outcome.reason,expect.reason,`case ${entry.name} reason`);
  if(expect.action!==undefined)assert.equal(outcome.action,expect.action,`case ${entry.name} action`);
  if(expect.path!==undefined)assert.equal(outcome.path,expect.path,`case ${entry.name} path`);
 }
});

test('sequences match the shared fixture',async t=>{
 for(const sequence of fixture.sequences){
  const home=await tempHome(t);
  const collector=collectorFor(home);
  await importTemplates(collector,fixture);
  for(const step of sequence.payloads)await applyPayload(collector,fixture,step);
  await checkSequence(collector,sequence);
 }
});

async function checkSequence(collector,sequence){ const expect=sequence.expect;
 const snapshot=collector.hub.snapshot();
 for(const expected of expect.sessions){
  const session=snapshot.sessions.find(session=>session.id===expected.id);
  assert.ok(session,`${sequence.name}: session ${expected.id} missing`);
  if(expected.status!==undefined)assert.equal(session.status,expected.status,`${expected.id} status`);
  if(expected.roundId!==undefined)assert.equal(session.roundId,expected.roundId,`${expected.id} roundId`);
  if(expected.roundIdPrefix!==undefined)assert.ok(session.roundId.startsWith(expected.roundIdPrefix),`${expected.id} roundId ${session.roundId}`);
  if(expected.title!==undefined)assert.equal(session.title,expected.title,`${expected.id} title`);
  if(expected.project!==undefined)assert.equal(session.project,expected.project,`${expected.id} project`);
  if(expected.pending!==undefined)assert.equal(session.pending.length,expected.pending,`${expected.id} pending`);
  if(expected.endedBy!==undefined)assert.equal(session.endedBy,expected.endedBy,`${expected.id} endedBy`);
  else if('endedBy' in expected)assert.equal(session.endedBy,undefined,`${expected.id} endedBy must be absent`);
 }
 const events=snapshot.events.map(event=>({kind:event.kind,sessionId:event.sessionId,roundId:event.roundId}));
 for(const expected of expect.events){
  const found=events.some(event=>event.kind===expected.kind&&event.sessionId===expected.sessionId
   &&(expected.roundId!==undefined?event.roundId===expected.roundId:event.roundId.startsWith(expected.roundIdPrefix)));
  assert.ok(found,`${sequence.name}: event ${expected.kind} for ${expected.sessionId} missing`);
 }
 assert.deepEqual((await collector.customIntegrationsGet()).diagnostics.map(item=>({outcome:item.outcome,reason:item.reason})),expect.diagnostics,`${sequence.name}: diagnostics`);
}

test('source is derived from the registry, not the payload',async t=>{
 const collector=collectorFor(await tempHome(t));
 await importTemplates(collector,fixture);
 await collector.poll();
 assert.equal(collector.hub.sources['custom:example-agent'].state,'ok');
 const outcome=await collector.ingestCustomHook({integration:'example-agent',payload:{
  source:'codex',agent_source:'codex',agent_edition:'domestic',
  event_name:'prompt_submitted',session_id:'forged',round_id:'r1',ts:1700000000000,
 }});
 assert.equal(outcome.outcome,'accepted');
 assert.ok(collector.hub.sessions.has('custom:example-agent:forged'));
 assert.ok(!collector.hub.sessions.has('codex:forged'),'a payload cannot impersonate a built-in source');
 assert.ok(![...collector.hub.sessions.values()].some(session=>session.source==='codex'));
});

test('unknown or disabled integrations are rejected without sessions',async t=>{
 const collector=collectorFor(await tempHome(t));
 await importTemplates(collector,fixture);
 const unknown=await collector.ingestCustomHook({integration:'missing-agent',payload:{event_name:'prompt_submitted',session_id:'s'}});
 assert.equal(unknown.outcome,'rejected');
 assert.equal(unknown.reason,'unknown_integration');
 const builtin=await collector.ingestCustomHook({integration:'codex',payload:{event_name:'prompt_submitted',session_id:'s'}});
 assert.equal(builtin.reason,'unknown_integration');
 await collector.customIntegrationsSet({action:'disable',id:'example-agent'});
 const disabled=await collector.ingestCustomHook({integration:'example-agent',payload:{event_name:'prompt_submitted',session_id:'s',ts:1700000000000}});
 assert.equal(disabled.reason,'integration_disabled');
 assert.equal(collector.hub.sessions.size,0);
 assert.equal(collector.hub.sources['custom:example-agent'].state,'disabled');
});

test('disable and remove clear only that source',async t=>{
 const home=await tempHome(t);
 const collector=collectorFor(home);
 await importTemplates(collector,fixture);
 await collector.ingestCustomHook({integration:'example-agent',payload:{event_name:'prompt_submitted',session_id:'a',round_id:'r1',ts:1700000000000}});
 await collector.ingestCustomHook({integration:'minimal-agent',payload:{kind:'begin',sid:'b'}});
 assert.equal(collector.hub.sessions.size,2);
 await collector.customIntegrationsSet({action:'remove',id:'example-agent'});
 assert.equal(collector.hub.sessions.size,1);
 assert.ok(collector.hub.sessions.has('custom:minimal-agent:b'));
 assert.equal(collector.hub.sources['custom:example-agent'],undefined);
 assert.ok(!(await collector.customIntegrationsGet()).templates.some(item=>item.id==='example-agent'));
 await assert.rejects(()=>collector.customIntegrationsSet({action:'remove',id:'example-agent'}),/未知的自定义来源/);
 const file=path.join(home,'.agent-studio','custom-integrations.json');
 const stored=parseCustomStore(await fs.readFile(file,'utf8'));
 assert.deepEqual([...stored.keys()],['minimal-agent']);
});

test('status reports capabilities, command and event times',async t=>{
 const collector=collectorFor(await tempHome(t));
 await importTemplates(collector,fixture);
 await collector.poll();
 const status=await collector.customIntegrationsGet();
 const template=status.templates[0];
 assert.equal(template.source,'custom:example-agent');
 assert.deepEqual(template.capabilities,['close','finish:done','finish:error','resume','start','wait:input','wait:permission']);
 assert.equal(template.lastReceivedAt,null,'an imported template has not received anything yet');
 assert.equal(template.lastMappedAt,null);
 assert.match(template.command,/custom-hook/);
 assert.match(template.command,/--integration example-agent/);
 assert.match(template.command,/--home/);
 assert.equal(status.storage.ok,true);
 await collector.ingestCustomHook({integration:'example-agent',payload:{event_name:'prompt_submitted',session_id:'a',round_id:'r1',ts:1700000000000}});
 const updated=await collector.customIntegrationsGet();
 assert.equal(typeof updated.templates[0].lastReceivedAt,'number');
 assert.equal(typeof updated.templates[0].lastMappedAt,'number');
});

test('duplicate import is rejected and keeps the first template',async t=>{
 const home=await tempHome(t);
 const collector=collectorFor(home);
 await importTemplates(collector,fixture);
 const renamed={...fixture.templates.example,name:'Renamed Agent'};
 await assert.rejects(()=>collector.customIntegrationsSet({action:'import',template:renamed}),/请先删除/);
 assert.equal((await collector.customIntegrationsGet()).templates[0].name,'Example Agent');
 const reloaded=collectorFor(home);
 assert.equal((await reloaded.customIntegrationsGet()).templates[0].name,'Example Agent');
});

test('preview maps without side effects',async t=>{
 const collector=collectorFor(await tempHome(t));
 const preview=await collector.customPreview({
  template:fixture.templates.example,
  payload:{event_name:'prompt_submitted',session_id:'preview',round_id:'r1',prompt:'Hi',ts:1700000000000},
 });
 assert.equal(preview.ok,true);
 assert.equal(preview.action,'start');
 assert.equal(preview.events[0].type,'start');
 assert.equal(collector.hub.sessions.size,0,'preview must not create sessions');
 const status=await collector.customIntegrationsGet();
 assert.deepEqual(status.templates,[]);
 assert.deepEqual(status.diagnostics,[]);
 const rejected=await collector.customPreview({template:fixture.templates.example,payload:{event_name:'prompt_submitted'}});
 assert.equal(rejected.ok,false);
 assert.equal(rejected.outcome,'rejected');
 assert.equal(rejected.path,'/mapping/sessionId');
 assert.equal(rejected.error,'载荷中缺少会话 ID');
 const invalid=await collector.customPreview({template:{schemaVersion:9},payload:{}});
 assert.equal(invalid.reason,'template_invalid');
 assert.equal(invalid.path,'/schemaVersion');
});

test('damaged store is reported and built-ins still start',async t=>{
 const home=await tempHome(t);
 await fs.mkdir(path.join(home,'.agent-studio'),{recursive:true});
 await fs.writeFile(path.join(home,'.agent-studio/custom-integrations.json'),'{not json');
 const collector=collectorFor(home);
 const status=await collector.customIntegrationsGet();
 assert.equal(status.storage.ok,false);
 assert.match(status.storage.error,/保留原文件/);
 await collector.poll();
 for(const source of ['codex','workbuddy','codebuddy-ide','codeg']){
  assert.ok(collector.hub.sources[source],`built-in source ${source} must still publish health`);
 }
 await assert.rejects(()=>collector.customIntegrationsSet({action:'import',template:fixture.templates.example}),/保留原文件/);
 assert.equal(await fs.readFile(path.join(home,'.agent-studio/custom-integrations.json'),'utf8'),'{not json');
});

test('enabled templates publish health rows',async t=>{
 const collector=collectorFor(await tempHome(t));
 await importTemplates(collector,fixture);
 assert.equal(collector.hub.sources['custom:example-agent'].state,'ok','import publishes health immediately; the rail drops a source with no row');
 await collector.poll();
 assert.equal(collector.hub.sources['custom:example-agent'].state,'ok');
 assert.equal(collector.hub.sources['custom:minimal-agent'].state,'ok');
 await collector.customIntegrationsSet({action:'disable',id:'minimal-agent'});
 assert.equal(collector.hub.sources['custom:minimal-agent'].state,'disabled');
 assert.equal(collector.hub.sources['custom:example-agent'].state,'ok');
});

test('pointer limits count characters, not bytes',()=>{
 const withEvent=pointer=>({...fixture.templates.minimal,mapping:{event:pointer,sessionId:'/sid'}});
 assert.equal(validateTemplate(withEvent(`/${'指'.repeat(300)}`)).ok,true,'300 CJK characters are 900 bytes and still valid');
 assert.equal(validateTemplate(withEvent(`/${'😀'.repeat(300)}`)).ok,true,'emoji pointers count code points, not UTF-16 units');
 assert.equal(validateTemplate(withEvent(`/${'指'.repeat(CUSTOM_LIMITS.pointerMax)}`)).ok,false);
});

test('custom ids and sources stay inside the custom namespace',()=>{
 assert.ok(isCustomId('example-agent'));
 assert.ok(!isCustomId('Example'));
 assert.ok(!isCustomId('1agent'));
 assert.ok(!isCustomId('agent_1'));
 assert.ok(!isCustomId('a'.repeat(65)));
 assert.equal(customSource('example'),'custom:example');
});

test('the development server accepts a custom hook envelope',async t=>{
 const home=await tempHome(t);
 const collector=collectorFor(home);
 const runtime=await startServer({port:0,collector,staticRoot:path.join(home,'no-static-files')});
 t.after(()=>runtime.close());
 await collector.customIntegrationsSet({action:'import',template:fixture.templates.minimal});
 const response=await fetch(`http://127.0.0.1:${runtime.server.address().port}/api/custom-hook`,{
  method:'POST',headers:{'Content-Type':'application/json'},
  body:JSON.stringify({integration:'minimal-agent',payload:{kind:'begin',sid:'dev'}}),
 });
 assert.equal(response.status,200);
 const outcome=await response.json();
 assert.equal(outcome.outcome,'accepted');
 assert.ok(collector.hub.sessions.has('custom:minimal-agent:dev'));
 const rejected=await fetch(`http://127.0.0.1:${runtime.server.address().port}/api/custom-hook`,{
  method:'POST',headers:{'Content-Type':'application/json'},
  body:JSON.stringify({integration:'missing',payload:{kind:'begin',sid:'dev'}}),
 });
 assert.equal((await rejected.json()).reason,'unknown_integration');
});
