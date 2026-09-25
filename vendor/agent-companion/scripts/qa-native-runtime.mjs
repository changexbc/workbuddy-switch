import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { defaultSettings } from '../src/settings-config.js';
const home = await fs.mkdtemp(path.join(os.tmpdir(), 'agent-studio-runtime-'));
const binary = path.resolve('target/release/agent-studio-runtime');
const settings = defaultSettings();
settings.notifications.desktop = true;
for (const source of Object.keys(settings.sources)) settings.sources[source].enabled = ['codex','codebuddy-ide'].includes(source);
await fs.mkdir(path.join(home,'.agent-studio'));
await fs.mkdir(path.join(home,'.codex'));
await fs.mkdir(path.join(home,'.codebuddy'));
await fs.writeFile(path.join(home,'.codebuddy/settings.json'),JSON.stringify({hooks:{Stop:[{hooks:[{command:'echo keep-ide-hook'}]}]}}));
await fs.writeFile(path.join(home,'.agent-studio/settings.json'),JSON.stringify(settings));
await fs.writeFile(path.join(home,'.codex/hooks.json'),JSON.stringify({hooks:{Stop:[{hooks:[{type:'command',command:'echo user-hook'}]}]}}));
const child=spawn(binary,['serve'],{env:{...process.env,AGENT_STUDIO_HOME:home},stdio:'ignore'});
const delay=ms=>new Promise(r=>setTimeout(r,ms));
let endpoint;
try {
  for(let i=0;i<100;i++){try{endpoint=JSON.parse(await fs.readFile(path.join(home,'.agent-studio/runtime-v1.json'),'utf8'));break;}catch{await delay(50);}}
  assert(endpoint,'runtime endpoint created');
  const url=`http://127.0.0.1:${endpoint.port}/rpc`;
  const rpc=async(command,payload)=>{const response=await fetch(url,{method:'POST',headers:{Authorization:`Bearer ${endpoint.token}`},body:JSON.stringify({command,payload})});const body=await response.json();assert(!body.error,body.error);return body.value;};
  assert.equal((await fetch(url,{method:'POST',body:'{}'})).status,403);
  assert.equal((await fetch(url,{method:'POST',headers:{Authorization:`Bearer ${endpoint.token}`,Origin:'https://example.com'},body:'{}'})).status,403);
  await rpc('hello',{client:'standalone'});await rpc('hello',{client:'wb-switch'});
  const duplicate=spawn(binary,['serve'],{env:{...process.env,AGENT_STUDIO_HOME:home},stdio:'ignore'});
  await new Promise(resolve=>duplicate.on('exit',resolve));
  assert.equal(JSON.parse(await fs.readFile(path.join(home,'.agent-studio/runtime-v1.json'),'utf8')).pid,endpoint.pid);
  const hooks=JSON.parse(await fs.readFile(path.join(home,'.codex/hooks.json'),'utf8'));
  assert(hooks.hooks.PostToolUse?.length);assert(hooks.hooks.Interrupt?.length);
  assert(hooks.hooks.Stop.some(g=>g.hooks.some(h=>h.command==='echo user-hook')));
  const hook=path.join(home,'.agent-studio/bin/agent-studio-runtime-v1');
  execFileSync(hook,['hook','--home',home],{input:JSON.stringify({session_id:'fixture',hook_event_name:'UserPromptSubmit',turn_id:'r',prompt:'Native fixture'})});
  let state;
  for(let i=0;i<30;i++){state=await rpc('poll',{client:'standalone'});if(state.snapshot.sessions.some(s=>s.sessionId==='fixture'))break;await delay(100);}
  assert.equal(state.snapshot.sessions.find(s=>s.sessionId==='fixture').status,'running');
  execFileSync(hook,['hook','--home',home],{input:JSON.stringify({session_id:'fixture',hook_event_name:'PreToolUse',turn_id:'r',tool_name:'request_user_input',tool_use_id:'q'})});
  let delivered=[];
  for(let i=0;i<30;i++){const next=await rpc('poll',{client:'standalone'});delivered.push(...next.notifications);if(delivered.some(a=>a.kind==='wait'))break;await delay(100);}
  assert.equal(delivered.filter(a=>a.kind==='wait').length,1);
  assert.equal((await rpc('poll',{client:'standalone'})).notifications.length,0);
  const other=await rpc('poll',{client:'wb-switch'});assert.equal(other.notifications.length,0);assert.equal(other.owner,false);assert.equal(state.owner,true);
  await rpc('leave',{client:'standalone'});await delay(200);
  assert.equal((await rpc('poll',{client:'wb-switch'})).owner,true);
  const stopOutput=execFileSync(hook,['hook','--home',home],{input:JSON.stringify({session_id:'fixture',hook_event_name:'Stop',turn_id:'r'}),encoding:'utf8'});
  assert.equal(stopOutput.trim(),'{}','Codex Stop hook returns valid JSON');
  for(let i=0;i<30;i++){state=await rpc('poll',{client:'wb-switch'});if(state.snapshot.sessions.find(s=>s.sessionId==='fixture')?.status==='done')break;await delay(100);}
  assert.equal(state.snapshot.sessions.find(s=>s.sessionId==='fixture').status,'done');
  for (const [event,status] of [['PreToolUse','wait'],['PostToolUse','running']]) {
    if(event==='PreToolUse')execFileSync(hook,['hook','--home',home],{input:JSON.stringify({session_id:'question',hook_event_name:'UserPromptSubmit',turn_id:'choice'})});
    execFileSync(hook,['hook','--home',home],{input:JSON.stringify({session_id:'question',hook_event_name:event,turn_id:'choice',tool_name:'request_user_input',tool_use_id:'choice-1',tool_input:{questions:[{question:'Choose'}]}})});
    for(let i=0;i<30;i++){state=await rpc('poll',{client:'wb-switch'});if(state.snapshot.sessions.find(s=>s.sessionId==='question')?.status===status)break;await delay(100);}
    assert.equal(state.snapshot.sessions.find(s=>s.sessionId==='question').status,status);
  }
  // A trailing step proves the queued async callbacks have reached the collector.
  for (const event of ['UserPromptSubmit','PreToolUse','PostToolUse']) {
    execFileSync(hook,['hook','--home',home],{input:JSON.stringify({session_id:'optional-question',turn_id:'optional',hook_event_name:event,tool_name:'functions.request_user_input_async',tool_use_id:'optional-1',tool_input:{questions:[{title:'Optional preference'}]},tool_response:{accepted:true}})});
  }
  execFileSync(hook,['hook','--home',home],{input:JSON.stringify({session_id:'optional-question',turn_id:'optional',hook_event_name:'PreToolUse',tool_name:'Bash',tool_use_id:'after-optional'})});
  const optionalAlerts=[];
  for(let i=0;i<30;i++) {
    state=await rpc('poll',{client:'wb-switch'});
    optionalAlerts.push(...state.notifications);
    if(state.snapshot.sessions.find(s=>s.sessionId==='optional-question')?.steps.some(s=>s.id==='after-optional'))break;
    await delay(100);
  }
  const optional=state.snapshot.sessions.find(s=>s.sessionId==='optional-question');
  assert(optional?.steps.some(s=>s.id==='after-optional'));
  assert.equal(optional.status,'running');
  assert.deepEqual(optional.pending,[]);
  assert(!optionalAlerts.some(a=>a.sessionId==='codex:optional-question'&&a.kind==='wait'));
  assert(!state.snapshot.events.some(a=>a.sessionId==='codex:optional-question'&&a.kind==='wait'));
  const ideHooks=JSON.parse(await fs.readFile(path.join(home,'.codebuddy/settings.json')));
  assert(ideHooks.hooks.Stop.some(g=>g.hooks.some(h=>h.command==='echo keep-ide-hook')));
  assert(ideHooks.hooks.PreToolUse.some(g=>g.hooks.some(h=>h.command.includes('--source codebuddy-ide'))));
  const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/codebuddy-ide-hooks.json',import.meta.url)));
  const fixtureTime=Date.now();
  for(const [i,item] of fixture.entries()) {
    const output=execFileSync(hook,['hook','--home',home,'--source','codebuddy-ide'],{input:JSON.stringify({client:'CodeBuddyIDE',session_id:'ide-native',cwd:'/project',timestamp:fixtureTime+i,...item.hook}),encoding:'utf8'});
    assert.deepEqual(JSON.parse(output),{});
    for(let n=0;n<30;n++) {state=await rpc('poll',{client:'wb-switch'});if((state.snapshot.sessions.find(s=>s.sessionId==='ide-native')?.status??null)===item.status)break;await delay(50);}
    assert.equal(state.snapshot.sessions.find(s=>s.sessionId==='ide-native')?.status??null,item.status,`IDE fixture ${i}`);
  }
  const updated=await rpc('settings_get',{});updated.sources['codebuddy-ide'].enabled=false;
  await rpc('settings_set',updated);
  execFileSync(hook,['hook','--home',home,'--source','codebuddy-ide'],{input:JSON.stringify({client:'CodeBuddyIDE',session_id:'disabled-ide',hook_event_name:'UserPromptSubmit'})});
  state=await rpc('poll',{client:'wb-switch'});
  assert(!state.snapshot.sessions.some(s=>s.source==='codebuddy-ide'));
  await assert.rejects(fs.access(path.join(home,'.agent-studio/codex-resume.json')));
  await rpc('leave',{client:'wb-switch'});
  console.log('PASS: Codex + IDE native hooks, disabled IDE ignores hooks, two clients, service lock, owner handoff, isolated config, authenticated RPC');
} finally {
  child.kill('SIGTERM');
  await fs.rm(home,{recursive:true,force:true});
}
