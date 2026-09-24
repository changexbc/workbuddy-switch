// CodeBuddy IDE command hooks; never reads databases or transcripts.
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { question, questionDetails } from './codex.js';
import { HostPresence, endHostSessions } from './host-process.js';

const SOURCE = 'codebuddy-ide';
export const HOOK_EVENTS = ['SessionStart','SessionEnd','UserPromptSubmit','PreToolUse','PostToolUse','Stop','PreCompact'];
const native = c => c?.includes('agent-studio-runtime') && c.includes(' hook') && c.includes('--source codebuddy-ide');
const python = c => c?.includes('astra-office-codebuddy-ide.py');
export function codeBuddySettingsCandidates(home = os.homedir()) {
  return [
    { label: 'international', dir: path.join(home, '.codebuddy') },
    { label: 'domestic', dir: path.join(home, '.codebuddycn') },
  ];
}
export function defaultCodeBuddyPaths(home = os.homedir()) {
  return codeBuddySettingsCandidates(home).map(c => c.dir);
}
export function codeBuddyEdition(dir) {
  return path.basename(dir || '') === '.codebuddycn' ? 'domestic' : 'international';
}
export function codeBuddyAgentType(edition) {
  const type = String(edition || '').toLowerCase();
  return type === 'domestic' || type === 'codebuddycn' ? 'codebuddycn' : 'codebuddy';
}
export function mergeCodeBuddyIdeHooks(input, command) {
  if (!input || typeof input !== 'object' || Array.isArray(input) || (input.hooks != null && (typeof input.hooks !== 'object' || Array.isArray(input.hooks)))) throw Error('现有 CodeBuddy IDE Hook 配置无效，未覆盖');
  const doc = structuredClone(input); doc.hooks ||= {};
  const nativeInstalled = Object.values(doc.hooks).some(groups => Array.isArray(groups) && groups.some(g => g.hooks?.some(h => native(h.command))));
  for (const event of HOOK_EVENTS) {
    if (doc.hooks[event] != null && !Array.isArray(doc.hooks[event])) throw Error(`CodeBuddy IDE ${event} Hook 配置无效，未覆盖`);
    const owned = c => python(c) || (!nativeInstalled && native(c));
    doc.hooks[event] = (doc.hooks[event] || []).map(g => Array.isArray(g.hooks) ? {...g, hooks:g.hooks.filter(h => !owned(h.command))} : g).filter(g => !Array.isArray(g.hooks) || g.hooks.length);
    if (!nativeInstalled) doc.hooks[event].push({matcher:'',hooks:[{type:'command',command,timeout:3}]});
  }
  return doc;
}
async function installCodeBuddyIdeHooksAt(dir, { monitorUrl } = {}) {
  try { if (!(await fs.stat(dir)).isDirectory()) return null; } catch { return null; }
  const file = path.join(dir,'settings.json'); let before = {}, existed = false;
  try { before = JSON.parse(await fs.readFile(file,'utf8')); existed = true; } catch(e) { if(e.code !== 'ENOENT') throw e; }
  const script = path.join(dir,'hooks/astra-office-codebuddy-ide.py');
  const command = `/usr/bin/python3 '${script.replaceAll("'", "'\\''")}'`;
  const after = mergeCodeBuddyIdeHooks(before,command);
  const hasNative = Object.values(after.hooks).some(groups => Array.isArray(groups) && groups.some(g => g.hooks?.some(h => native(h.command))));
  if (!hasNative) {
    await fs.mkdir(path.dirname(script),{recursive:true});
    await fs.copyFile(fileURLToPath(new URL('../hooks/codebuddy-ide-status.py',import.meta.url)),script);
    await fs.writeFile(path.join(dir,'hooks/astra-office-codebuddy-ide.url'),monitorUrl.replace(/\/$/,''));
  }
  if (JSON.stringify(before) !== JSON.stringify(after)) {
    if(existed) try { await fs.copyFile(file,path.join(dir,'settings.agent-studio-before-ide-hooks.json'),fs.constants.COPYFILE_EXCL); } catch(e) { if(e.code !== 'EEXIST') throw e; }
    await fs.writeFile(file,JSON.stringify(after,null,2)+'\n');
  }
  return {settingsPath:file,native:hasNative,edition:codeBuddyEdition(dir)};
}
export async function installCodeBuddyIdeHooks(home, {dataDir, monitorUrl} = {}) {
  const dirs = dataDir ? [dataDir] : defaultCodeBuddyPaths(home);
  const installed = [];
  for (const dir of dirs) {
    const result = await installCodeBuddyIdeHooksAt(dir, { monitorUrl });
    if (result) installed.push(result);
  }
  if (!installed.length) return null;
  return { ...installed[0], settingsPaths: installed.map(item => item.settingsPath) };
}
function unanswered(v) {
  if(typeof v === 'string') {try {return unanswered(JSON.parse(v));} catch {return false;}}
  if(!v || typeof v !== 'object') return false;
  if(v.type === 'multi_question_result' || Object.hasOwn(v,'answers')) return !v.answers || Object.values(v.answers).every(x=>x == null || x === '' || Array.isArray(x) && !x.length);
  return unanswered(v.result);
}
const stable = v => JSON.stringify(v && typeof v === 'object' && !Array.isArray(v) ? Object.fromEntries(Object.keys(v).sort().map(k=>[k,JSON.parse(stable(v[k]))])) : Array.isArray(v) ? v.map(x=>JSON.parse(stable(x))) : v);
export class CodeBuddyIdePoller {
  constructor(hub,{home=os.homedir(),dataDir,monitorUrl,hostPresence,vscodePresence}={}) { Object.assign(this,{hub,home,dataDir,monitorUrl}); this.live = new Map(); this.hookCount = 0; this.presence = hostPresence || new HostPresence({source:SOURCE}); this.vscodePresence = vscodePresence || new HostPresence({source:'vscode'}); }
  ingestHook(p) {
    const sid=p.session_id,event=p.hook_event_name;
    const client=String(p.client||'').toLowerCase();
    if(!sid || !HOOK_EVENTS.includes(event) || !['codebuddyide','codebuddy','vscode'].includes(client)) return false;
    // The shared settings file serves both hosts; the payload client picks one.
    const hostKind = client === 'vscode' ? 'vscode' : SOURCE;
    const ts = Number.isInteger(p.timestamp) && p.timestamp > 0 ? p.timestamp : Date.now();
    const state = this.live.get(sid) || {roundId:'',cwd:'',calls:new Map(),seq:0,ended:false,ts:0,agentType:''};
    if(ts < state.ts) return false;
    const generation = p.generation_id || '', begins = event === 'UserPromptSubmit';
    if(!begins && generation && state.roundId && generation !== state.roundId) return false;
    state.cwd = p.cwd || state.cwd; state.ts = ts;
    const agentType = codeBuddyAgentType(p.agent_edition || p.agentEdition || state.agentType);
    state.agentType = agentType;
    const emit = ev => this.hub.ingest({source:SOURCE,sessionId:sid,roundId:state.roundId,cwd:state.cwd,agentType,hostKind,ts,...ev});
    if(begins || !state.roundId && ['PreToolUse','PostToolUse','PreCompact'].includes(event)) {
      state.roundId = generation || `turn:${ts}`; state.calls.clear(); state.ended=false; emit({type:'start'});
    }
    const tool=p.tool_name||'',input=p.tool_input??null;
    const ask=/(?:^|__|\.)(ask_followup_question|request_user_input(?:_async)?|AskUserQuestion|ask_user_question|RequestUserInput)$/.test(tool);
    if(begins && typeof p.prompt==='string' && p.prompt.trim()) emit({type:'meta',title:p.prompt});
    if(event==='PreToolUse' && !state.ended) {
      const id=p.tool_use_id || `ide:${++state.seq}`;
      if(ask) {state.calls.set(id,{tool,input});emit({type:'wait',callId:id,tool,text:question(input),questions:questionDetails(input)});}
      else emit({type:'step',eventId:id,label:tool});
    }
    if(event==='PostToolUse' && !state.ended && ask) {
      const matches=[...state.calls].filter(([id,c])=>p.tool_use_id ? id===p.tool_use_id : c.tool===tool && stable(c.input)===stable(input));
      if(matches.length===1 && !unanswered(p.tool_response)) {const id=matches[0][0];state.calls.delete(id);emit({type:'resolve',callId:id});}
    }
    if(event==='Stop' && !state.ended && state.roundId && !state.calls.size) {emit({type:'end',status:'done'});state.ended=true;}
    if(event==='SessionEnd' && !state.ended && state.roundId) {emit({type:'end',status:'aborted'});state.calls.clear();state.ended=true;}
    if(event==='PreCompact' && !state.ended) emit({type:'activity'});
    this.live.set(sid,state);this.hookCount++;(hostKind==='vscode'?this.vscodePresence:this.presence).noteHook();this.poll();return true;
  }
  async install(monitorUrl=this.monitorUrl) { return installCodeBuddyIdeHooks(this.home,{dataDir:this.dataDir,monitorUrl}); }
  async poll() {
    const ide = await this.presence.observe(), vscode = await this.vscodePresence.observe();
    const gone = [];
    if(ide==='gone'){endHostSessions(this.hub,SOURCE,{hostKind:SOURCE});gone.push(SOURCE);}
    if(vscode==='gone'){endHostSessions(this.hub,SOURCE,{hostKind:'vscode'});gone.push('vscode');}
    // A live host kind keeps the source connected; a kind that never saw a hook
    // stays unknown and never becomes an exit.
    if(gone.length && ide!=='alive' && vscode!=='alive'){
      this.hub.health(SOURCE,'exited',gone.length===2?'CodeBuddy IDE 与 VS Code 已退出，未完成的任务已标记中止':gone[0]==='vscode'?'VS Code 已退出，未完成的任务已标记中止':'CodeBuddy IDE 已退出，未完成的任务已标记中止');
      return;
    }
    this.hub.health(SOURCE,'ok',this.hookCount?'已连接 CodeBuddy Hook（不读取会话文件）':'等待新的 CodeBuddy Hook；不恢复历史会话');
  }
}
