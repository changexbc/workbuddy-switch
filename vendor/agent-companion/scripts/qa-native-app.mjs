import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {defaultSettings} from '../src/settings-config.js';
const root=path.resolve(import.meta.dirname,'..');
const home=await fs.mkdtemp(path.join(os.tmpdir(),'agent-companion-native-'));
await fs.mkdir(path.join(root,'artifacts'),{recursive:true});
const out=await fs.mkdtemp(path.join(root,'artifacts/native-'));
const settings=defaultSettings();
for(const source of Object.values(settings.sources))source.enabled=false;
settings.sources.codex.enabled=true;
settings.notifications.desktop=false;
await fs.mkdir(path.join(home,'.agent-studio'),{recursive:true});
await fs.mkdir(path.join(home,'.codex'));
await fs.writeFile(path.join(home,'.agent-studio/settings.json'),JSON.stringify(settings));
const binary=path.join(root,'src-tauri/target/release/bundle/macos/Agent Companion.app/Contents/MacOS/agent-companion');
const app=spawn(binary,[],{env:{...process.env,AGENT_STUDIO_HOME:home,AGENT_COMPANION_QA:out},stdio:['ignore','pipe','pipe']});
let stderr='';app.stderr.on('data',v=>stderr+=v);app.stdout.resume();
let endpoint;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
const exit=new Promise((resolve,reject)=>{app.on('error',reject);app.on('exit',(code,signal)=>resolve({code,signal}));});
let timeout;
try {
 for(let i=0;i<100;i++){
  try{endpoint=JSON.parse(await fs.readFile(path.join(home,'.agent-studio/runtime-v1.json'),'utf8'));break;}catch{}
  await delay(100);
 }
 assert(endpoint,`native app starts a runtime in the isolated home; exit=${app.exitCode}; stderr=${stderr}`);
 const hook=path.join(home,'.agent-studio/bin/agent-studio-runtime-v1');
 for(const event of [
  {hook_event_name:'UserPromptSubmit',prompt:'独立应用隔离验证'},
  {hook_event_name:'PreToolUse',tool_name:'request_user_input',tool_use_id:'q1',tool_input:{questions:[{question:'请选择下一步',options:[{label:'继续'},{label:'暂停'}]}]}}
 ])execFileSync(hook,['hook','--home',home],{input:JSON.stringify({session_id:'companion-native-fixture',turn_id:'r1',...event})});
 const result=await Promise.race([exit,new Promise((_,reject)=>{timeout=setTimeout(()=>reject(Error('native QA timeout; build with --features diagnostics')),45000);})]);
 assert.equal(result.code,0,stderr);
 const labels=JSON.parse(await fs.readFile(path.join(out,'windows.json')));
 assert.deepEqual(labels.sort(),['agent-studio-rail','agent-studio-settings']);
 assert.equal(JSON.parse(await fs.readFile(path.join(out,'unsupported-view.json'))),'未知视图');
 const rail=JSON.parse(await fs.readFile(path.join(out,'agent-studio-rail/report.json')));
 const preferences=JSON.parse(await fs.readFile(path.join(out,'agent-studio-settings/report.json')));
 assert.equal(rail.avatars,1);assert.equal(rail.wait,1);assert.match(rail.text,/需要你确认/); // Existing native Codex adapter emits a generic question label.
 assert.equal(rail.connection,'connected');assert.equal(preferences.settingsReady,true);
 assert.match(preferences.text,/监听与接入/,'real WebView renders integration management');
 assert.match(preferences.text,/已接入/,'native integration RPC reports installed status while details are collapsed');
 assert(!/接入状态响应无效|不支持的命令|当前运行环境不支持接入管理/.test(preferences.text),'native integration bridge is supported');
 // A blocked rail hides every real element with `visibility:hidden`, which is
 // how `rail.text` once came back empty while the avatars were still in the DOM.
 // Asserting the actual cause beats asserting the symptom.
 //
 // Note that `AGENT_STUDIO_HOME` does not isolate this: the welcome-seen flag
 // lives in the WebView's own localStorage, alongside the bundle identifier, so
 // a run on a machine that has already played the welcome skips it. Both paths
 // are valid — `phase` is `idle` when skipped and a finish reason when played —
 // and either way nothing may still be blocking by the time the report is taken.
 assert.equal(rail.welcome.blocking,false,`the rail is not hidden behind the welcome overlay (phase ${rail.welcome.phase})`);
 assert.equal(rail.welcome.running,false,'the welcome animation has finished');
 assert.deepEqual(rail.menu,[],'native rail has no right-click menu');
 assert(![...rail.resources,...preferences.resources].some(url=>/\.glb|\.exr|three|\/models\//i.test(url)));
 await fs.writeFile(path.join(out,'summary.json'),JSON.stringify({passed:true,labels,rail,settings:preferences},null,2));
 console.log(`PASS: packaged native rail, Hook question, settings, no office window; reports ${out}`);
} finally {
 clearTimeout(timeout);
 if(app.exitCode===null)app.kill('SIGTERM');
 await exit;
 if(endpoint?.pid){try{process.kill(endpoint.pid,'SIGTERM');}catch{}}
 await fs.rm(home,{recursive:true,force:true});
}
