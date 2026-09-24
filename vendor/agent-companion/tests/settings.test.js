import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {defaultSettings,validateSettings} from '../src/settings-config.js';
import {createSettingsStore} from '../collector/lib/settings.js';
import {createCollector} from '../collector/lib/collector.js';
import {startServer} from '../collector/server.js';
import {assignSessions} from '../src/monitor/model.js';
test('settings persist atomically and reject invalid saves without replacing the last valid value',async()=>{
 const dir=await fs.mkdtemp(path.join(os.tmpdir(),'studio-settings-'));try{
  const file=path.join(dir,'settings.json'),store=createSettingsStore(file);await store.load();const c=defaultSettings();c.sources.codex.enabled=false;c.scene.speed=4;c.scene.renderResolution='low';await store.save(c);
  const reread=createSettingsStore(file);assert.deepEqual(await reread.load(),c);
  await assert.rejects(store.save({...c,scene:{...c.scene,speed:999}}));assert.deepEqual(store.value,c);
  assert.throws(()=>validateSettings({...c,schedule:{...c.schedule,start:'18:00',end:'09:00'}}));
 }finally{await fs.rm(dir,{recursive:true,force:true});}
});
test('settings API actually disables sources, ignores disabled hooks, checks paths and survives restart',async()=>{
 const home=await fs.mkdtemp(path.join(os.tmpdir(),'studio-collector-'));let runtime;
 try{
  await fs.mkdir(path.join(home,'.workbuddy'),{recursive:true});
  const config=defaultSettings();for(const s of Object.values(config.sources))s.enabled=false;
  const settingsFile=path.join(home,'settings.json');await fs.writeFile(settingsFile,JSON.stringify(config));
  const collector=createCollector({home,settingsFile,intervalMs:60000});runtime=await startServer({port:0,collector});const base=`http://127.0.0.1:${runtime.server.address().port}`;
  const send=(url,body,method='POST')=>fetch(base+url,{method,headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});
  assert.deepEqual(await (await fetch(base+'/api/settings')).json(),config);
  await send('/api/codex-hook',{hook_event_name:'UserPromptSubmit',session_id:'ignored',prompt:'test'});assert.equal(collector.hub.sessions.size,0);
  let result=await (await send('/api/settings/check',{source:'workbuddy',path:path.join(home,'.workbuddy')})).json();assert.equal(result.ok,true);
  result=await (await send('/api/settings/check',{source:'workbuddy',path:path.join(home,'missing')})).json();assert.equal(result.ok,false);
  config.sources.workbuddy.enabled=true;assert.equal((await send('/api/settings',config,'PUT')).status,200);
  collector.hub.ingest({source:'workbuddy',sessionId:'test',type:'start',ts:Date.now()});
  config.sources.workbuddy.enabled=false;assert.equal((await send('/api/settings',config,'PUT')).status,200);assert.equal(collector.hub.sessions.size,0);assert.equal(collector.hub.sources.workbuddy.state,'disabled');
  assert.equal((await send('/api/settings',{...config,scene:{speed:99}},'PUT')).status,400);
  const other=createCollector({home,settingsFile,intervalMs:60000});await other.start();assert.equal(other.getSettings().sources.workbuddy.enabled,false);other.stop();
 }finally{await runtime?.close();await fs.rm(home,{recursive:true,force:true});}
});
test('fixed source seats, discovery switch and retention control session assignment',()=>{
 const sessions=[{id:'a',source:'codex',status:'running',updatedAt:100},{id:'b',source:'workbuddy',status:'running',updatedAt:99}];
 assert.deepEqual(assignSessions([],sessions,100,2,new Map(),new Set(),{assignment:'fixed',seats:['workbuddy','codex']}),['b','a']);
 assert.deepEqual(assignSessions(['a'],sessions,100,2,new Map(),new Set(),{autoDiscover:false}),['a',null]);
 assert.deepEqual(assignSessions(['a'],[{...sessions[0],status:'done',endedAt:1}],2000,1,new Map(),new Set(),{retentionHours:0}),[null]);
});

test('avatar style defaults migrate, both styles persist, invalid style preserves saved settings',async()=>{
 const dir=await fs.mkdtemp(path.join(os.tmpdir(),'avatar-settings-'));
 try {
  const file=path.join(dir,'settings.json'),old=defaultSettings();delete old.monitor.avatarStyle;
  await fs.writeFile(file,JSON.stringify(old));const store=createSettingsStore(file);await store.load();
  assert.equal(store.value.monitor.avatarStyle,'animal');
  for(const style of ['bot','animal']){const next=store.value;next.monitor.avatarStyle=style;await store.save(next);const reload=createSettingsStore(file);await reload.load();assert.equal(reload.value.monitor.avatarStyle,style);}
  const invalid=store.value;invalid.monitor.avatarStyle='other';await assert.rejects(store.save(invalid));assert.equal(store.value.monitor.avatarStyle,'animal');
 } finally {await fs.rm(dir,{recursive:true,force:true});}
});
