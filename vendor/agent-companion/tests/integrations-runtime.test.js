import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';
import { defaultSettings } from '../src/settings-config.js';
import { createCollector } from '../collector/lib/collector.js';
const binary=process.env.CODEG_RUNTIME_BINARY;
const wait=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn){for(let i=0;i<100;i++){let v=await fn();if(v)return v;await wait(50);}throw Error('Timed out');}
async function start(home,t){
 await fs.rm(path.join(home,'.agent-studio/runtime-v1.json'),{force:true});
 const child=spawn(binary,['serve','--home',home],{stdio:'ignore'});
 t.after(()=>child.kill());
 const endpoint=await until(async()=>{try{return JSON.parse(await fs.readFile(path.join(home,'.agent-studio/runtime-v1.json'),'utf8'));}catch{return null;}});
 const rpc=async(command,payload={})=>{const response=await fetch(`http://127.0.0.1:${endpoint.port}/rpc`,{method:'POST',headers:{Authorization:`Bearer ${endpoint.token}`},body:JSON.stringify({command,payload})});const result=await response.json();if(result.error)throw Error(result.error);return result.value;};
 await rpc('hello',{client:'integration-test'});
 return {rpc,endpoint,async stop(){child.kill();await new Promise(r=>child.once('exit',r));}};
}
async function fixture(t){
 const home=await fs.mkdtemp(path.join(os.tmpdir(),'integration-management-'));
 t.after(()=>fs.rm(home,{recursive:true,force:true}));
 await fs.mkdir(path.join(home,'.agent-studio'),{recursive:true});
 const settings=defaultSettings();for(const s of Object.values(settings.sources))s.enabled=false;
 await fs.writeFile(path.join(home,'.agent-studio/settings.json'),JSON.stringify(settings));
 return {home,settings};
}
test('native RPC hook uninstall is precise, durable across settings/restart, and malformed files stay intact',{skip:!binary},async t=>{
 const {home,settings}=await fixture(t);
 for(const dir of ['.codex','.workbuddy-ai','.codebuddy'])await fs.mkdir(path.join(home,dir));
 let runtime=await start(home,t);
 for(const source of ['codex','workbuddy','codebuddy-ide']) {
  const installed=await runtime.rpc('integrations_set',{source,action:'install'});
  const row=installed.sources.find(s=>s.source===source);assert.equal(row.status,'installed');
  const file=row.locations[0],doc=JSON.parse(await fs.readFile(file,'utf8'));
  const other={type:'command',command:'echo agent-studio-runtime hook --source '+source};
  doc.hooks.Stop[0].hooks.push(other);doc.keep='unchanged';
  await fs.writeFile(file,JSON.stringify(doc));
  const result=await runtime.rpc('integrations_set',{source,action:'uninstall'});
  assert.equal(result.sources.find(s=>s.source===source).automatic,false);
  const after=JSON.parse(await fs.readFile(file,'utf8'));assert.equal(after.keep,'unchanged');assert.deepEqual(after.hooks.Stop[0].hooks,[other]);
  settings.sources[source].enabled=true;
 }
 await runtime.rpc('settings_set',settings);
 await runtime.stop();runtime=await start(home,t);
 assert.ok((await runtime.rpc('integrations_get')).sources.filter(s=>s.source!=='codeg').every(s=>s.status==='not_installed'&&!s.automatic));
 const file=path.join(home,'.codex/hooks.json');await fs.writeFile(file,'broken');
 assert.equal((await runtime.rpc('integrations_get')).sources[0].status,'error');
 await assert.rejects(runtime.rpc('integrations_set',{source:'codex',action:'uninstall'}));assert.equal(await fs.readFile(file,'utf8'),'broken');
 assert.equal((await fetch(`http://127.0.0.1:${runtime.endpoint.port}/rpc`,{method:'POST',body:JSON.stringify({command:'integrations_set',payload:{source:'codex',action:'install'}})})).status,403);
 await runtime.stop();
 // The alternate Node collector must honor native opt-out as well.
 const collector=createCollector({home});await collector.start();await collector.updateSettings(settings);await collector.stop();assert.equal(await fs.readFile(file,'utf8'),'broken');
});
test('Codeg offline unregister remains pending and retries without touching unrelated callbacks',{skip:!binary},async t=>{
 const {home,settings}=await fixture(t);settings.sources.codeg.enabled=true;
 await fs.writeFile(path.join(home,'.agent-studio/settings.json'),JSON.stringify(settings));
 let hooks=[{url:'https://example.test/keep',enabled:true}],offline=false;
 const events=['user_prompt_sent','question_request','permission_request','turn_complete','error'];
 const server=http.createServer(async(req,res)=>{
  let chunks=[];for await(const chunk of req)chunks.push(chunk);let p=JSON.parse(Buffer.concat(chunks).toString()||'{}');
  if(offline){res.writeHead(503).end();return;}
  let value=null;switch(req.url){case '/api/get_chat_event_webhooks':value=hooks;break;case '/api/set_chat_event_webhooks':hooks=p.webhooks;break;case '/api/get_chat_event_filter':value=events;break;default:assert.fail(req.url);}
  res.setHeader('Content-Type','application/json');res.end(JSON.stringify(value));
 });
 await new Promise(r=>server.listen(0,'127.0.0.1',r));t.after(()=>new Promise(r=>server.close(r)));
 const dir=path.join(home,'Library/Application Support/app.codeg');await fs.mkdir(dir,{recursive:true});
 const db=new DatabaseSync(path.join(dir,'codeg.db'));db.exec('CREATE TABLE app_metadata(key TEXT,value TEXT)');
 db.prepare('INSERT INTO app_metadata VALUES (?,?)').run('web_service_port',String(server.address().port));db.prepare('INSERT INTO app_metadata VALUES (?,?)').run('web_service_token','test');db.close();
 let runtime=await start(home,t);await until(()=>hooks.length===2);
 assert.equal((await runtime.rpc('integrations_get')).sources.find(s=>s.source==='codeg').status,'installed');
 const callback = hooks.find(h=>h.url.startsWith('http://127.0.0.1:')).url;
 offline=true;let result=await runtime.rpc('integrations_set',{source:'codeg',action:'uninstall'});
 assert.equal(result.sources.find(s=>s.source==='codeg').status,'pending');
 const callbackPost=()=>fetch(callback,{method:'POST',body:JSON.stringify({source:'codeg',connection_id:'retry-check',event:'turn_complete'})});
 assert.equal((await callbackPost()).status,410);
 await assert.rejects(runtime.rpc('integrations_set',{source:'codeg',action:'install'}),/不可用/);
 // Registration failed, but its durable opt-in must reopen the callback for later retry.
 assert.equal((await callbackPost()).status,204);
 await runtime.rpc('integrations_set',{source:'codeg',action:'uninstall'});
 await runtime.stop();runtime=await start(home,t);offline=false;
 result=await runtime.rpc('integrations_set',{source:'codeg',action:'uninstall'});
 assert.equal(result.sources.find(s=>s.source==='codeg').status,'not_installed');assert.deepEqual(hooks,[{url:'https://example.test/keep',enabled:true}]);
 await runtime.rpc('settings_set',settings);await wait(2200);assert.equal(hooks.length,1);
 result=await runtime.rpc('integrations_set',{source:'codeg',action:'install'});assert.equal(result.sources.find(s=>s.source==='codeg').status,'installed');assert.equal(hooks.length,2);
 const journal=path.join(home,'.agent-studio/codeg-webhook-native.json');
 const saved=await fs.readFile(journal,'utf8');
 await fs.writeFile(journal,JSON.stringify({owned:[42]}));
 assert.equal((await runtime.rpc('integrations_get')).sources.find(s=>s.source==='codeg').status,'error');
 await runtime.rpc('integrations_set',{source:'codeg',action:'uninstall'});
 assert.equal(await fs.readFile(journal,'utf8'),JSON.stringify({owned:[42]}));
 assert.equal(hooks.length,2);
 await fs.writeFile(journal,saved);
 await runtime.rpc('integrations_set',{source:'codeg',action:'uninstall'});
 await runtime.stop();
});
