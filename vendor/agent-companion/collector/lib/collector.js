import path from 'node:path';
import os from 'node:os';
import fs from 'node:fs/promises';
import {existsSync} from 'node:fs';
import { Hub } from './hub.js';
import { WorkBuddyPoller, defaultWorkBuddyPaths } from './workbuddy.js';
import { CodeBuddyIdePoller, defaultCodeBuddyPaths } from './codebuddy-ide.js';
import { CodexLivePoller } from './codex-live.js';
import { CodexReadStateObserver } from './codex-read-state.js';
import { CodegHooks, defaultCodegDbPaths } from './codeg.js';
import {createSettingsStore} from './settings.js';
import {SOURCE_IDS,validateSettings} from '../../src/settings-config.js';
import {CUSTOM_LIMITS,CUSTOM_SOURCE_PREFIX,customSource,isCustomId,createCustomEngine,outcomeJson,templateCapabilities,templateEventTable,validateTemplate} from './custom.js';
import {createCustomStore} from './custom-store.js';
const DETAIL_MAX=200;
const hookBinary=home=>path.join(home,'.agent-studio/bin',process.platform==='win32'?'agent-studio-runtime-v1.exe':'agent-studio-runtime-v1');
const shellQuote=value=>`'${String(value).replaceAll("'","'\\''")}'`;
export function createCollector({ home=os.homedir(),intervalMs=2000,monitorUrl=`http://127.0.0.1:${process.env.MONITOR_PORT||8849}`,settingsFile=path.join(home,'.agent-studio','settings.json')}={}){
 const hub=new Hub(),store=createSettingsStore(settingsFile);let initialized=false,started=false,pollers={},timer,stopped=false,chain=Promise.resolve(),codexReadState;
 const custom=createCustomStore(home),customEngine=createCustomEngine(),customDiagnostics=[],customStats=new Map();
 const serial=fn=>{const result=chain.then(fn);chain=result.catch(()=>{});return result;};
 const resolve=p=>p.startsWith('~/')?path.join(home,p.slice(2)):p;
 function paths(id,config){const custom=config.sources[id].path;if(custom && !(id==='codebuddy-ide'&&custom.endsWith('.vscdb'))){const p=resolve(custom);if(!path.isAbsolute(p))throw Error('数据路径必须为绝对路径或以 ~/ 开头');return [p];}return id==='codex'?[path.join(home,'.codex')]:id==='workbuddy'?defaultWorkBuddyPaths(home):id==='codeg'?defaultCodegDbPaths(home):defaultCodeBuddyPaths(home);}
 function make(id,c){const p=paths(id,c)[0],custom=!!c.sources[id].path;
  if(id==='codex'){codexReadState=new CodexReadStateObserver(path.join(p,'.codex-global-state.json'));return new CodexLivePoller(hub,{home,monitorUrl,dataDir:p});}
  if(id==='workbuddy')return new WorkBuddyPoller(hub,{home,monitorUrl,dataDir:custom?p:null});
  if(id==='codeg')return new CodegHooks(hub,{home,dbPaths:custom?[p]:defaultCodegDbPaths(home)});
  return new CodeBuddyIdePoller(hub,{home,monitorUrl,dataDir:custom && !String(c.sources[id].path).endsWith('.vscdb')?p:null});
 }
 function clear(id){for(const [key,s]of hub.sessions)if(s.source===id)hub.sessions.delete(key);for(const [key,e]of hub.events)if(e.sessionId.startsWith(id+':'))hub.events.delete(key);if(id==='codeg')hub.hiddenCodegCodexIds.clear();}
 function pushCustomDiagnostic(value){while(customDiagnostics.length>=CUSTOM_LIMITS.diagnosticsMax)customDiagnostics.shift();customDiagnostics.push(value);}
 function customReject(id,reason,detail){
  pushCustomDiagnostic({at:Date.now(),source:isCustomId(id)?customSource(id):CUSTOM_SOURCE_PREFIX,event:null,outcome:'rejected',reason,detail});
  return {outcome:'rejected',action:null,reason,detail,path:'',event:null,sessionId:null,roundId:null,events:[]};
 }
 function recordCustom(id,outcome){
  const at=Date.now(),stat=customStats.get(id)||{lastReceivedAt:null,lastMappedAt:null};
  stat.lastReceivedAt=at;if(outcome.outcome==='accepted')stat.lastMappedAt=at;customStats.set(id,stat);
  pushCustomDiagnostic({at,source:customSource(id),event:outcome.event,outcome:outcome.outcome,reason:outcome.reason,detail:[...String(outcome.detail||'')].slice(0,DETAIL_MAX).join('')});
 }
 // The source is the trusted command argument, never the payload, so a raw
 // payload cannot impersonate a built-in source.
 function ingestCustomHook(payload){
  const requested=typeof payload?.integration==='string'?payload.integration:'';
  const entry=isCustomId(requested)&&!SOURCE_IDS.includes(requested)?custom.get(requested):undefined;
  if(!entry)return customReject(requested,'unknown_integration','未注册的自定义来源');
  if(!entry.enabled)return customReject(requested,'integration_disabled','该自定义来源已停用');
  const raw=payload.payload===undefined?null:payload.payload;
  const size=Buffer.byteLength(JSON.stringify(raw)??'');
  if(size>CUSTOM_LIMITS.payloadBytes)return customReject(requested,'payload_too_large','载荷超过 1 MiB 上限');
  const outcome=outcomeJson(customEngine.apply(entry.template,raw,Date.now()));
  recordCustom(requested,outcome);
  for(const event of outcome.events)hub.ingest(event);
  return outcome;
 }
 const customCommand=id=>`${shellQuote(hookBinary(home))} custom-hook --home ${shellQuote(home)} --integration ${id}`;
 function customStatus(){
  const templates=[...custom.entries].sort(([a],[b])=>a<b?-1:a>b?1:0).map(([id,entry])=>({
   id,source:customSource(id),name:entry.template.name,enabled:entry.enabled,importedAt:entry.importedAt,
   capabilities:templateCapabilities(entry.template),events:templateEventTable(entry.template),
   lastReceivedAt:customStats.get(id)?.lastReceivedAt??null,lastMappedAt:customStats.get(id)?.lastMappedAt??null,
   command:customCommand(id),
  }));
  return {version:1,storage:{ok:!custom.error,error:custom.error??null},binaryInstalled:existsSync(hookBinary(home)),templates,diagnostics:[...customDiagnostics]};
 }
 async function customManage(payload){
  const action=typeof payload?.action==='string'?payload.action:'';
  const requested=payload?.id;
  if(action==='import'){
   const validated=validateTemplate(payload?.template);
   if(!validated.ok)throw Error(validated.path?`模板字段 ${validated.path} 无效：${validated.message}`:`模板无效：${validated.message}`);
   await custom.import(validated.template,Date.now());
   // The rail hides sessions whose source has no health row. Publish it now;
   // waiting for the next poll drops the first events.
   pollCustom();
  }else if(action==='enable'||action==='disable'){
   if(!isCustomId(requested))throw Error('未知的自定义来源');
   await custom.setEnabled(requested,action==='enable');
   forgetCustomSessions(requested);
   if(action==='enable')pollCustom();else hub.health(customSource(requested),'disabled','已停用');
  }else if(action==='remove'){
   if(!isCustomId(requested))throw Error('未知的自定义来源');
   if(!await custom.remove(requested))throw Error('未知的自定义来源');
   forgetCustomSessions(requested);customStats.delete(requested);delete hub.sources[customSource(requested)];
  }else throw Error('未知的自定义接入操作');
  return customStatus();
 }
 // Side-effect free: the same pure mapping logic, a throwaway engine, no hub or registry writes.
 function customPreview(payload){
  const validated=validateTemplate(payload?.template);
  if(!validated.ok)return {ok:false,outcome:'rejected',reason:'template_invalid',path:validated.path,error:validated.message,events:[]};
  const engine=createCustomEngine();
  const outcome=outcomeJson(engine.apply(validated.template,payload?.payload,Date.now()));
  return {...outcome,ok:outcome.outcome==='accepted',...(outcome.outcome==='accepted'?{}:{error:outcome.detail}),capabilities:templateCapabilities(validated.template)};
 }
 // The rail only shows sessions of a healthy source, so every enabled template
 // publishes a health row.
 function pollCustom(){
  for(const [id,entry]of custom.entries){
   const source=customSource(id);
   if(entry.enabled)hub.health(source,'ok','等待自定义 Hook 事件');else hub.health(source,'disabled','已停用');
  }
 }
 function forgetCustomSessions(id){
  const source=customSource(id);
  customEngine.forgetSource(source);
  for(const [key,s]of hub.sessions)if(s.source===source)hub.sessions.delete(key);
 }
 let effectiveSettings;
 async function rebuild(before,next){
  let policy={};
  try { policy=JSON.parse(await fs.readFile(path.join(home,'.agent-studio/integrations.json'),'utf8')); }
  catch(error) { if(error.code!=='ENOENT')throw error; }
  if(!policy||Array.isArray(policy)||typeof policy!=='object'||Object.entries(policy).some(([id,v])=>!SOURCE_IDS.includes(id)||typeof v!=='boolean'))throw Error('接入策略无效');
  before=effectiveSettings;
  next=structuredClone(next);
  for(const id of SOURCE_IDS)if(policy[id]===false)next.sources[id].enabled=false;
  effectiveSettings=next;
  for(const id of SOURCE_IDS){
   if(before&&JSON.stringify(before.sources[id])===JSON.stringify(next.sources[id]))continue;
   if(id==='codeg'&&pollers[id])await pollers[id].disable();
   clear(id);
   if(id!=='codeg'||next.sources[id].enabled)delete pollers[id];
   if(next.sources[id].enabled){
    pollers[id]=make(id,next);
    if(started)await pollers[id].install(monitorUrl);
   }else{
    // Keep the disabled adapter only to retry removal of our own callback.
    if(id==='codeg'&&!pollers[id]){
     pollers[id]=make(id,next);pollers[id].enabled=false;
     if(started)await pollers[id].install(monitorUrl);
    }
    hub.health(id,'disabled','已关闭监听');
   }
  }
 }
 async function ensure(){if(initialized)return;const c=await store.load();await custom.load();await rebuild(null,c);initialized=true;}
 async function pollNow(){await ensure();for(const id of SOURCE_IDS)if(pollers[id])await pollers[id].poll();if(pollers.codex)await codexReadState.poll(hub);hub.ready=true;pollCustom();}
 return {hub,poll:()=>serial(pollNow),getSettings:()=>store.value,
  setMonitorUrl(value){monitorUrl=value;},
  codegWebhookPath:()=>pollers.codeg?.url?new URL(pollers.codeg.url).pathname:null,
  ingestCodegHook:payload=>serial(async()=>{await ensure();return pollers.codeg?.ingestHook(payload);}),
  ingestCodexHook:payload=>serial(async()=>{await ensure();const sid=payload?.session_id||payload?.sessionId,event=payload?.hook_event_name||payload?.hookEventName;if(['SessionStart','UserPromptSubmit'].includes(event)&&pollers.codeg?.isChildCodexSession(sid))hub.hideCodegChildCodex(sid);return pollers.codex?.ingestHook(payload);}),
  ingestCodebuddyIdeHook:payload=>serial(async()=>{await ensure();return pollers['codebuddy-ide']?.ingestHook(payload);}),
  ingestWorkbuddyHook:payload=>serial(async()=>{await ensure();return pollers.workbuddy?.ingestHook(payload);}),
  ingestCustomHook:payload=>serial(async()=>{await ensure();return ingestCustomHook(payload);}),
  customIntegrationsGet:()=>serial(async()=>{await ensure();return customStatus();}),
  customIntegrationsSet:payload=>serial(async()=>{await ensure();return customManage(payload);}),
  customPreview:payload=>serial(async()=>{await ensure();return customPreview(payload);}),
  updateSettings:input=>serial(async()=>{await ensure();const next=validateSettings(input);for(const id of SOURCE_IDS)paths(id,next);const before=store.value;await store.save(next);await rebuild(before,next);await pollNow();return store.value;}),
  async checkSource({source,path:custom=''}){if(!SOURCE_IDS.includes(source)||typeof custom!=='string')throw Error('未知 Agent 来源');const config=store.value;config.sources[source].path=custom;const candidates=paths(source,config),found=[];for(const p of candidates){try{const st=await fs.stat(p);await fs.access(p,fs.constants.R_OK);if(['codex','workbuddy','codebuddy-ide'].includes(source)?!st.isDirectory():!st.isFile()&&!st.isDirectory())continue;found.push(p);}catch{}}return {ok:found.length>0,paths:found,detail:found.length?'路径可读取；会话状态以监听结果为准':'未找到可读取的数据路径，请检查路径或先运行该 Agent'};},
  async start(){await serial(async()=>{await ensure();started=true;await pollers.codeg?.install(monitorUrl);await pollers.codex?.install(monitorUrl);await pollers.workbuddy?.install(monitorUrl);await pollers['codebuddy-ide']?.install(monitorUrl);await pollNow();});const loop=async()=>{if(stopped)return;try{await serial(pollNow);}catch{}if(!stopped)timer=setTimeout(loop,intervalMs);};timer=setTimeout(loop,intervalMs);},
  async stop(){stopped=true;clearTimeout(timer);await serial(async()=>{await pollers.codeg?.disable();});}
 };
}
