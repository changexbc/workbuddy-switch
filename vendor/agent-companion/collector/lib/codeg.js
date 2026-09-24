// Webhooks discover connections; read-only event streams reconcile confirmations. SQLite is used
// only for API credentials and one keyed metadata lookup after an event.
import path from 'node:path';
import fs from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { question, questionDetails } from './codex.js';
let DatabaseSync;
try { ({ DatabaseSync } = await import('node:sqlite')); } catch {}
export const SOURCE = 'codeg';
export const CODEG_EVENTS = ['user_prompt_sent','question_request','permission_request','turn_complete','error'];
export function defaultCodegDbPaths(home) {
  return [path.join(home,'Library/Application Support/app.codeg/codeg.db'),path.join(home,'Library/Application Support/codeg/codeg.db'),path.join(home,'.local/share/codeg/codeg.db')];
}
export function mergeCodegWebhooks(existing, owned, url) {
  if (!Array.isArray(existing) || existing.some(w=>!w || typeof w.url!=='string' || typeof w.enabled!=='boolean')) throw Error('Codeg Webhook 配置无效，未覆盖');
  const next=existing.filter(w=>!owned.includes(w.url) && w.url!==url);
  if(url)next.push({url,enabled:true});
  return next;
}
export function askFromSnapshot(snap) {
  if(snap?.pending_question)return snap.pending_question;
  if(snap?.pending_plan_approval)return {approval_id:snap.pending_plan_approval.approval_id,questions:[{question:snap.pending_plan_approval.plan_markdown,options:[{label:'批准'},{label:'拒绝'}]}]};
  if(snap?.pending_permission){const p=snap.pending_permission;return {request_id:p.request_id,questions:[{question:p.tool_call?.title||p.tool_call?.name||'需要你的许可',options:(p.options||[]).map(o=>({label:o.name||o.label||'',description:o.kind||''}))}]};}
  return null;
}
export class CodegHooks {
  constructor(hub,{home,dbPaths=defaultCodegDbPaths(home),fetchImpl=globalThis.fetch,now=Date.now,WebSocketImpl=globalThis.WebSocket}={}) {
    Object.assign(this,{hub,home,dbPaths,fetchImpl,now,WebSocketImpl});
    this.url='';this.auth=null;this.registered=false;this.reconciled=false;this.nextAttempt=0;this.enabled=true;this.sequence=0;this.connections=new Map();this.ignoredConnections=new Set();
    this.streams=new Map();
    this.stateFile=path.join(home,'.agent-studio/codeg-webhook-node.json');
  }
  open() {
    if(!DatabaseSync)throw Error('Codeg 配置需要 Node 22.13+');
    for(const p of this.dbPaths){try{return new DatabaseSync(p,{readOnly:true});}catch{}}
    throw Error('未找到 Codeg 数据库，无法读取 Web Service 配置');
  }
  credentials() {
    const db=this.open();try {
      const meta=Object.fromEntries(db.prepare("SELECT key,value FROM app_metadata WHERE key IN ('web_service_port','web_service_token')").all().map(r=>[r.key,r.value]));
      const port=Number(meta.web_service_port||3080),token=meta.web_service_token;
      if(!Number.isInteger(port)||port<1||port>65535||!token)throw Error('请启用 Codeg Web Service');
      return {port,token};
    } finally {db.close();}
  }
  async post(name,body={}) {
    const response=await this.fetchImpl(`http://127.0.0.1:${this.auth.port}/api/${name}`,{method:'POST',headers:{'Content-Type':'application/json',Authorization:`Bearer ${this.auth.token}`},body:JSON.stringify(body),signal:AbortSignal.timeout(1500)});
    if(!response.ok)throw Error(`Codeg API HTTP ${response.status}`);
    return response.json();
  }
  async install(base) {this.url=`${base}/api/codeg-webhook/${randomUUID()}`;this.registered=false;this.reconciled=false;this.nextAttempt=0;await this.poll();}
  async poll() {
    // Only failed configuration is retried. No timer reads session state.
    if(!this.url){this.hub.health(SOURCE,'partial','Webhook 接收入口尚未启动');return;}
    if(this.registered || this.now()<this.nextAttempt)return;
    this.nextAttempt=this.now()+60000;
    try {
      let owned=[];try{owned=JSON.parse(await fs.readFile(this.stateFile,'utf8')).owned;if(!Array.isArray(owned))throw Error('invalid');}catch(e){if(e.code!=='ENOENT')throw Error('Codeg 注册记录不可读，未覆盖');}
      if(!this.enabled&&!owned.length){this.registered=true;return;}
      this.auth=this.credentials();
      const existing=await this.post('get_chat_event_webhooks');
      const next=mergeCodegWebhooks(existing,owned,this.enabled?this.url:'');
      if(this.enabled){
        const filter=await this.post('get_chat_event_filter');
        if(filter!==null && (!Array.isArray(filter)||filter.some(e=>typeof e!=='string')))throw Error('Codeg 事件配置无效，未覆盖');
        const current=filter??CODEG_EVENTS.filter(e=>e!=='user_prompt_sent');
        if(CODEG_EVENTS.some(e=>!current.includes(e))){
          // The filter is global: don't turn on new messages to other sinks.
          const channels=await this.post('list_chat_channels');
          if(!Array.isArray(channels)||channels.some(c=>c.enabled)||next.some(w=>w.url!==this.url&&w.enabled))throw Error('Codeg 全局事件开关影响其他推送目标；请先在 Codeg 启用所需的五类事件');
          await this.post('set_chat_event_filter',{filter:[...new Set([...current,...CODEG_EVENTS])]});
        }
      }
      // Journal both URLs before mutation; retry uncertain writes without duplicates.
      const url=this.enabled?this.url:'';
      if(url&&!owned.includes(url))owned.push(url);
      await fs.mkdir(path.dirname(this.stateFile),{recursive:true});
      const save=async value=>{const tmp=this.stateFile+'.tmp';await fs.writeFile(tmp,JSON.stringify(value),{mode:0o600});await fs.rename(tmp,this.stateFile);};
      await save({owned});
      if(JSON.stringify(existing)!==JSON.stringify(next))await this.post('set_chat_event_webhooks',{webhooks:next});
      await save({owned:url?[url]:[]});
      this.registered=true;
      this.hub.health(SOURCE,this.enabled?'ok':'disabled',this.enabled?'Webhook 已注册，等待 Codeg 事件（不扫描会话）':'已关闭监听');
      // One-shot startup alignment; failures only show on codeg health.
      if(this.enabled&&!this.reconciled){
        this.reconciled=true;
        try {await this.reconcileActiveConnections();}
        catch(e) {this.hub.health(SOURCE,'error',e.message?.startsWith('Codeg')||e.message?.startsWith('请')||e.message?.startsWith('未')?e.message:'请确认 Codeg Web Service 可用');}
      }
    } catch(e) {this.hub.health(SOURCE,this.enabled?'error':'disabled',this.enabled?`Webhook 注册失败：${e.message?.startsWith('Codeg')||e.message?.startsWith('请')||e.message?.startsWith('未')?e.message:'请确认 Codeg Web Service 可用'}`:'已关闭监听；Webhook 注销待重试');}
  }
  async disable(){this.enabled=false;for(const stream of this.streams.values())stream.close();this.streams.clear();this.registered=false;this.reconciled=false;this.nextAttempt=0;await this.poll();}
  metadata(sid) {
    const db=this.open();try{
      const tables=db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().map(r=>r.name);
      const table=tables.includes('conversation')?'conversation':'conversations';
      const row=db.prepare(`SELECT * FROM ${table} WHERE id=?`).get(sid)||{};
      let cwd=row.origin_cwd||row.cwd||row.workspace||'';
      if(!cwd&&row.folder_id&&tables.includes('folder'))cwd=db.prepare('SELECT path FROM folder WHERE id=?').get(row.folder_id)?.path||'';
      return {title:row.title||'',cwd,agentType:row.agent_type||row.agent||'',externalId:row.external_id||'',folderId:row.folder_id,isSubagent:row.parent_id!=null||row.kind==='delegate'};
    } finally{db.close();}
  }
  async ingestHook(p) {
    if(!this.enabled||p?.source!=='codeg'||!CODEG_EVENTS.includes(p.event)||typeof p.connection_id!=='string'||!p.connection_id.trim()||p.connection_id.length>256)return false;
    if(this.ignoredConnections.has(p.connection_id))return true;
    let snap=null;
    try {this.auth??=this.credentials();snap=await this.post('acp_get_session_snapshot',{connectionId:p.connection_id});}catch{}
    if(!this.enabled)return false;
    const sid=String(snap?.conversation_id??this.connections.get(p.connection_id)??`connection:${p.connection_id}`);
    const provisional=`codeg:connection:${p.connection_id}`;
    if(!sid.startsWith('connection:')){this.connections.set(p.connection_id,sid);this.hub.sessions.delete(provisional);}
    if(this.connections.size>512)this.connections.delete(this.connections.keys().next().value);
    let meta={};if(!sid.startsWith('connection:'))try{meta=this.metadata(sid);}catch{}
    if(meta.isSubagent) {
      // A known child stays ignored even if later snapshot/metadata reads fail.
      this.ignoredConnections.add(p.connection_id);
      this.streams.get(p.connection_id)?.close();this.streams.delete(p.connection_id);
      this.hub.sessions.delete(provisional);
      this.hub.sessions.delete(`codeg:${sid}`);
      return true;
    }
    const existingStream=this.streams.get(p.connection_id);
    if(existingStream?.sid!==sid){existingStream?.close();this.streams.delete(p.connection_id);}
    const active=this.streams.get(p.connection_id);
    // A keyed HTTP response may arrive after newer stream frames. Never let it
    // rewind pending requests or reopen a round already completed by the stream.
    if(active?.seq!=null && Number.isSafeInteger(snap?.event_seq) && snap.event_seq<active.seq)return true;
    if(active?.seq!=null && p.event==='user_prompt_sent' && snap?.status && snap.status!=='prompting' && ['done','error','aborted'].includes(this.hub.sessions.get(`codeg:${sid}`)?.status))return true;
    if(active && active.seq!==null && ['question_request','permission_request'].includes(p.event))return true;
    if(p.event==='turn_complete'&&snap?.status==='prompting')return true;
    const ts=this.now(),seq=++this.sequence;
    const current=this.hub.sessions.get(`codeg:${sid}`);
    const base=this.seedSession(sid,snap,meta,ts,seq,p.event==='user_prompt_sent'||!current,String(p.body||'Codeg').slice(0,240));
    const send=e=>this.hub.ingest({...base,...e});
    if((p.event==='question_request'||p.event==='permission_request') && !['done','error','aborted'].includes(this.hub.sessions.get(`codeg:${sid}`)?.status)) {
      const ask=askFromSnapshot(snap);
      const fields=Array.isArray(p.fields)?p.fields:[];
      // Native webhook fields retain question/option text when snapshot unavailable.
      const text=ask?question(ask):fields.map(f=>String(f.value||'')).filter(Boolean).join('\n')||String(p.body||p.title||'等待确认');
      for(const pending of [...(this.hub.sessions.get(`codeg:${sid}`)?.pending||[])])send({type:'resolve',callId:pending.id});
      send({type:'wait',callId:ask?.question_id||ask?.request_id||ask?.approval_id||`codeg:${p.connection_id}:${seq}`,tool:p.event==='question_request'?'ask':'permission',text,questions:ask?questionDetails(ask):[]});
    } else if(p.event==='turn_complete'||p.event==='error')send({type:'end',status:p.event==='error'?'error':'done'});
    // The keyed snapshot can already be ahead of a delayed webhook.
    if(snap?.status==='prompting')this.reconcileSnapshot(sid,snap);
    this.attachStream(p.connection_id,sid);
    if(Number.isSafeInteger(snap?.event_seq)){const stream=this.streams.get(p.connection_id);if(stream)stream.seq=Math.max(stream.seq??-1,snap.event_seq);}
    this.hub.health(SOURCE,'ok','已收到 Codeg Webhook；确认状态通过实时事件同步');
    return true;
  }
  // Shared by the webhook path and the startup alignment so both emit the same
  // `start` shape and round id. `startBody` only fills in when the session
  // metadata has no title.
  seedSession(sid,snap,meta,ts,seq,start,startBody) {
    const base={source:SOURCE,sessionId:sid,ts,...meta,externalId:snap?.external_id||meta.externalId,folderId:snap?.folder_id??meta.folderId};
    if(start)this.hub.ingest({...base,type:'start',roundId:`hook:${ts}:${seq}`,title:meta.title||startBody});
    return base;
  }
  // One-shot alignment at startup or enablement: enumerate live connections,
  // then re-seed only the in-flight ones, because a session waiting for a
  // delegated child emits no events at all. Never runs again while registered.
  async reconcileActiveConnections() {
    const listed=await this.post('acp_list_connections');
    if(!Array.isArray(listed))throw Error('Codeg 连接列表无效');
    const limit=64;let attempted=0,failed=0;
    for(const row of listed.slice(0,limit)) {
      const conn=typeof row?.id==='string'?row.id:String(row?.id??'');
      if(!conn.trim()||conn.length>256)continue;
      attempted++;
      let snap;try {snap=await this.post('acp_get_session_snapshot',{connectionId:conn});}catch{failed++;continue;}
      if(!this.enabled)return;
      const sid=String(snap?.conversation_id??'');
      if(!sid)continue;
      let meta={};try {meta=this.metadata(sid);}catch{}
      if(meta.isSubagent)continue;
      // A webhook may have won the race; never start a round twice or revive
      // the one the hub already finished.
      if(this.hub.sessions.has(`codeg:${sid}`))continue;
      if(snap?.status!=='prompting'&&!['pending_question','pending_permission','pending_plan_approval'].some(key=>snap[key]!=null))continue;
      this.connections.set(conn,sid);
      const ts=this.now(),seq=++this.sequence;
      this.seedSession(sid,snap,meta,ts,seq,true,'');
      this.reconcileSnapshot(sid,snap);
      this.attachStream(conn,sid);
      if(Number.isSafeInteger(snap?.event_seq)){const stream=this.streams.get(conn);if(stream)stream.seq=Math.max(stream.seq??-1,snap.event_seq);}
    }
    // One health write: a second note must not erase the first.
    const notes=[];
    if(attempted&&failed===attempted)notes.push('启动对齐未能读取会话快照');
    if(listed.length>limit)notes.push('Codeg 连接数超出上限，仅对齐前 64 条');
    if(notes.length)this.hub.health(SOURCE,'partial',notes.join('；'));
  }
  reconcileSnapshot(sid,snap,authoritative=false) {
    const session=this.hub.sessions.get(`codeg:${sid}`);
    if(!session||['done','error','aborted'].includes(session.status))return;
    const asks=['pending_question','pending_permission','pending_plan_approval'].flatMap(key=>{
      if(!snap[key])return [];
      const ask=askFromSnapshot({[key]:snap[key]});
      const id=ask?.question_id||ask?.request_id||ask?.approval_id;
      return typeof id==='string'&&id?[{id,ask,tool:key==='pending_question'?'ask':'permission'}]:[];
    });
    const send=e=>this.hub.ingest({source:SOURCE,sessionId:sid,ts:this.now(),...e});
    if(authoritative && snap.status==='connected' && ['pending_question','pending_permission','pending_plan_approval'].every(key=>snap[key]==null)){send({type:'end',status:'done'});return;}
    // Only a prompting snapshot proves that an absent request has resumed.
    if(snap.status==='prompting')for(const pending of [...session.pending])if(!asks.some(a=>a.id===pending.id))send({type:'resolve',callId:pending.id});
    for(const {id,ask,tool} of asks)send({type:'wait',callId:id,tool,text:question(ask),questions:questionDetails(ask)});
  }
  applyEnvelope(sid,envelope) {
    const session=this.hub.sessions.get(`codeg:${sid}`);
    if(!session||['done','error','aborted'].includes(session.status))return;
    const send=e=>this.hub.ingest({source:SOURCE,sessionId:sid,ts:this.now(),...e});
    const kind=envelope.type;
    const id=envelope.question_id||envelope.request_id||envelope.approval_id;
    if(['question_resolved','permission_resolved','plan_approval_resolved'].includes(kind)) {
      if(typeof id==='string'&&id)send({type:'resolve',callId:id});
    } else if(['question_request','permission_request','plan_approval_request'].includes(kind)) {
      const key=kind==='question_request'?'pending_question':kind==='permission_request'?'pending_permission':'pending_plan_approval';
      const ask=askFromSnapshot({[key]:envelope});
      if(typeof id==='string'&&id)send({type:'wait',callId:id,tool:kind==='question_request'?'ask':'permission',text:question(ask),questions:questionDetails(ask)});
    } else if(kind==='turn_complete')send({type:'end',status:'done'});
  }
  attachStream(connectionId,sid) {
    if(!this.auth||!this.WebSocketImpl||this.streams.has(connectionId)||!this.enabled)return;
    if(this.streams.size>=512){const key=this.streams.keys().next().value;this.streams.get(key).close();this.streams.delete(key);}
    const subscription=randomUUID();let socket,timer,closed=false,attempt=0;
    const state={sid,seq:null,close:()=>{closed=true;clearTimeout(timer);socket?.close();}};
    this.streams.set(connectionId,state);
    const connect=()=>{
      if(closed||!this.enabled)return;
      const protocols=['codeg-events',`codeg-token.${Buffer.from(this.auth.token.trim()).toString('base64url')}`];
      socket=new this.WebSocketImpl(`ws://127.0.0.1:${this.auth.port}/ws/events`,protocols);
      const current=socket;
      const attach=()=>current.send(JSON.stringify({action:'attach',subscription_id:subscription,connection_id:connectionId,since_seq:state.seq}));
      current.addEventListener('open',attach);
      current.addEventListener('error',()=>{});
      current.addEventListener('close',()=>{
        if(!closed){timer=setTimeout(connect,Math.min(30000,500*2**Math.min(attempt++,6)));timer.unref?.();}
      });
      current.addEventListener('message',message=>{
        if(closed||!this.enabled||socket!==current)return;
        let frame;try{frame=JSON.parse(message.data);}catch{return;}
        if(frame.subscription_id!==subscription||(frame.connection_id!=null&&frame.connection_id!==connectionId))return;
        attempt=0;
        if(frame.type==='snapshot'&&Number.isSafeInteger(frame.event_seq)) {
          if(state.seq!==null&&frame.event_seq<state.seq)return;
          state.seq=frame.event_seq;this.reconcileSnapshot(sid,frame.snapshot||{},true);
        } else if(frame.type==='event'||frame.type==='replay') {
          for(const envelope of frame.type==='event'?[frame.envelope]:frame.events||[]) {
            if(!Number.isSafeInteger(envelope?.seq)||envelope.seq<=(state.seq??-1)||envelope.connection_id!==connectionId)continue;
            state.seq=envelope.seq;this.applyEnvelope(sid,envelope);
          }
          if(frame.type==='replay'&&Number.isSafeInteger(frame.high_water_seq))state.seq=Math.max(state.seq??0,frame.high_water_seq);
        } else if(frame.type==='detached') {
          if(['lagged','server_shutdown'].includes(frame.reason))current.close();
          else {state.close();this.streams.delete(connectionId);}
        }
      });
    };
    connect();
  }

}
