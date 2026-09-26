import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {Hub} from '../collector/lib/hub.js';
import {CodeBuddyIdePoller,mergeCodeBuddyIdeHooks,installCodeBuddyIdeHooks,HOOK_EVENTS} from '../collector/lib/codebuddy-ide.js';

test('IDE hook fixture covers generation IDs, no tool IDs, unanswered questions and session end',async()=>{
 const hub=new Hub(),p=new CodeBuddyIdePoller(hub),t=Date.now();
 assert.equal(p.ingestHook({client:'cli',session_id:'cli',hook_event_name:'UserPromptSubmit'}),false);
 const cases=JSON.parse(await fs.readFile(new URL('./fixtures/codebuddy-ide-hooks.json',import.meta.url)));
 for(const [i,c] of cases.entries()) {
  assert.equal(p.ingestHook({client:'CodeBuddyIDE',session_id:'x',cwd:'/project',timestamp:t+i,...c.hook}),c.accepted!==false,`case ${i}`);
  assert.equal(hub.sessions.get('codebuddy-ide:x')?.status??null,c.status,`case ${i}`);
 }
});
test('IDE hooks stamp international and domestic editions',()=>{
 const hub=new Hub(),p=new CodeBuddyIdePoller(hub),t=Date.now();
 p.ingestHook({client:'CodeBuddyIDE',session_id:'intl',cwd:'/project',timestamp:t,hook_event_name:'UserPromptSubmit',generation_id:'g',prompt:'hello',agent_edition:'international'});
 assert.equal(hub.sessions.get('codebuddy-ide:intl').agentType,'codebuddy');
 p.ingestHook({client:'CodeBuddyIDE',session_id:'cn',cwd:'/project',timestamp:t,hook_event_name:'UserPromptSubmit',generation_id:'g',prompt:'hello',agent_edition:'domestic'});
 assert.equal(hub.sessions.get('codebuddy-ide:cn').agentType,'codebuddycn');
});
test('IDE installer preserves unrelated hooks, backs up, and yields to native installer',async t=>{
 const home=await fs.mkdtemp(path.join(os.tmpdir(),'ide-hooks-'));t.after(()=>fs.rm(home,{recursive:true,force:true}));
 const dir=path.join(home,'.codebuddy');await fs.mkdir(dir);
 const file=path.join(dir,'settings.json');
 const before={hooks:{Stop:[{hooks:[{command:'echo keep'}]}],FinalStop:[{hooks:[{command:'echo final'}]}]},custom:true};
 await fs.writeFile(file,JSON.stringify(before));
 await installCodeBuddyIdeHooks(home,{monitorUrl:'http://127.0.0.1:8849'});
 const after=JSON.parse(await fs.readFile(file));
 assert.deepEqual(after.hooks.Stop[0],before.hooks.Stop[0]);assert.deepEqual(after.hooks.FinalStop,before.hooks.FinalStop);
 assert.deepEqual(JSON.parse(await fs.readFile(path.join(dir,'settings.agent-studio-before-ide-hooks.json'))),before);
 const mtime=(await fs.stat(file)).mtimeMs;
 await installCodeBuddyIdeHooks(home,{monitorUrl:'http://127.0.0.1:8849'});
 assert.equal((await fs.stat(file)).mtimeMs,mtime);
 const native={hooks:Object.fromEntries(HOOK_EVENTS.map(e=>[e,[{matcher:'',hooks:[{command:"'/tmp/agent-studio-runtime-v1' hook --source codebuddy-ide"}]}]]))};
 assert.deepEqual(mergeCodeBuddyIdeHooks(native,'python3 hook.py'),native);
 assert.throws(()=>mergeCodeBuddyIdeHooks({hooks:{Stop:{}}},'test'));
});
test('IDE installer writes international and domestic editions when both homes exist',async t=>{
 const home=await fs.mkdtemp(path.join(os.tmpdir(),'ide-editions-'));t.after(()=>fs.rm(home,{recursive:true,force:true}));
 await fs.mkdir(path.join(home,'.codebuddy'));
 await fs.mkdir(path.join(home,'.codebuddycn'));
 const installed=await installCodeBuddyIdeHooks(home,{monitorUrl:'http://127.0.0.1:8849'});
 assert.deepEqual(installed.settingsPaths,[path.join(home,'.codebuddy/settings.json'),path.join(home,'.codebuddycn/settings.json')]);
 for (const dir of ['.codebuddy','.codebuddycn']) {
  const after=JSON.parse(await fs.readFile(path.join(home,dir,'settings.json')));
  assert.match(after.hooks.Stop.at(-1).hooks[0].command,/astra-office-codebuddy-ide\.py/);
 }
});
test('IDE host exit ends unfinished work but leaves completed rounds alone',async()=>{
 const hub=new Hub();let mode='alive';
 // 时间戳取「虚构基准」与真实时钟的较大值：endHostSessions 会用 Date.now() 盖合成时间，
 // 虚构时间戳若落后于它会被 hub 的防回退逻辑丢弃（负载高时曾致 flaky）。
 const presence={noteHook(){mode='alive';},async observe(){return mode;}};
 const unseen={noteHook(){},async observe(){return 'unknown';}};
 const p=new CodeBuddyIdePoller(hub,{hostPresence:presence,vscodePresence:unseen}),t=Date.now();let seq=0;
 const hook=(sid,event,extra={})=>p.ingestHook({client:'CodeBuddyIDE',session_id:sid,cwd:'/project',timestamp:Math.max(t+(++seq),Date.now()),hook_event_name:event,...extra});
 hook('x','UserPromptSubmit',{generation_id:'g',prompt:'work'});
 hook('y','UserPromptSubmit',{generation_id:'g',prompt:'quick'});
 hook('y','Stop');
 await p.poll();
 assert.equal(hub.sources['codebuddy-ide'].state,'ok');
 assert.equal(hub.sessions.get('codebuddy-ide:x').status,'running');
 assert.equal(hub.sessions.get('codebuddy-ide:y').status,'done');
 mode='gone';
 await p.poll();
 assert.equal(hub.sources['codebuddy-ide'].state,'exited');
 assert.equal(hub.sessions.get('codebuddy-ide:x').status,'aborted');
 assert.equal(hub.sessions.get('codebuddy-ide:x').endedBy,'host');
 assert.equal(hub.sessions.get('codebuddy-ide:y').status,'done');
 assert.equal(hub.sessions.get('codebuddy-ide:y').endedBy,undefined);
 hook('x','UserPromptSubmit',{generation_id:'g2',prompt:'again'});
 await p.poll();
 assert.equal(hub.sources['codebuddy-ide'].state,'ok');
 assert.equal(hub.sessions.get('codebuddy-ide:x').status,'running');
 assert.equal(hub.sessions.get('codebuddy-ide:x').endedBy,undefined);
});
test('VS Code plugin hooks drive their own host kind',async()=>{
 const hub=new Hub();let ide='alive',vscode='alive';
 const idePresence={noteHook(){ide='alive';},async observe(){return ide;}};
 const vscodePresence={noteHook(){vscode='alive';},async observe(){return vscode;}};
 const p=new CodeBuddyIdePoller(hub,{hostPresence:idePresence,vscodePresence}),t=Date.now();let seq=0;
 const hook=(client,sid,event,extra={})=>p.ingestHook({client,session_id:sid,cwd:'/project',timestamp:Math.max(t+(++seq),Date.now()),hook_event_name:event,...extra});
 assert.equal(hook('VSCode','code','UserPromptSubmit',{generation_id:'g',prompt:'work'}),true);
 hook('CodeBuddyIDE','ide','UserPromptSubmit',{generation_id:'g',prompt:'work'});
 assert.equal(hub.sessions.get('codebuddy-ide:code').hostKind,'vscode');
 assert.equal(hub.sessions.get('codebuddy-ide:ide').hostKind,'codebuddy-ide');
 ide='gone';
 await p.poll();
 assert.equal(hub.sources['codebuddy-ide'].state,'ok');
 assert.equal(hub.sessions.get('codebuddy-ide:ide').status,'aborted');
 assert.equal(hub.sessions.get('codebuddy-ide:ide').endedBy,'host');
 assert.equal(hub.sessions.get('codebuddy-ide:code').status,'running');
 assert.equal(hub.sessions.get('codebuddy-ide:code').endedBy,undefined);
 vscode='gone';
 await p.poll();
 assert.equal(hub.sources['codebuddy-ide'].state,'exited');
 assert.equal(hub.sources['codebuddy-ide'].detail,'CodeBuddy IDE 与 VS Code 已退出，未完成的任务已标记中止');
 assert.equal(hub.sessions.get('codebuddy-ide:code').status,'aborted');
});
test('an exited host kind is named while an unseen kind stays unknown',async()=>{
 const hub=new Hub();
 const unseen={noteHook(){},async observe(){return 'unknown';}};
 const gone={noteHook(){},async observe(){return 'gone';}};
 const p=new CodeBuddyIdePoller(hub,{hostPresence:unseen,vscodePresence:gone}),t=Date.now();
 p.ingestHook({client:'CodeBuddyIDE',session_id:'ide',cwd:'/project',timestamp:t,hook_event_name:'UserPromptSubmit',generation_id:'g',prompt:'work'});
 p.ingestHook({client:'VSCode',session_id:'code',cwd:'/project',timestamp:t+1,hook_event_name:'UserPromptSubmit',generation_id:'g',prompt:'work'});
 await p.poll();
 assert.equal(hub.sources['codebuddy-ide'].state,'exited');
 assert.equal(hub.sources['codebuddy-ide'].detail,'VS Code 已退出，未完成的任务已标记中止');
 assert.equal(hub.sessions.get('codebuddy-ide:code').status,'aborted');
 assert.equal(hub.sessions.get('codebuddy-ide:ide').status,'running');
});
test('an unhooked IDE kind never concludes an exit',async()=>{
 const hub=new Hub();
 const unseen={noteHook(){},async observe(){return 'unknown';}};
 const p=new CodeBuddyIdePoller(hub,{hostPresence:unseen,vscodePresence:unseen});
 await p.poll();
 assert.equal(hub.sources['codebuddy-ide'].state,'ok');
 assert.equal(hub.sources['codebuddy-ide'].detail,'等待新的 CodeBuddy Hook；不恢复历史会话');
 assert.equal(hub.sessions.size,0);
});
