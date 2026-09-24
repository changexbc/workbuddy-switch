// Read only Codex desktop's unread metadata, never transcripts or the database.
import fs from 'node:fs/promises';
const INTERVAL_MS=60_000, HOLD_MS=3_600_000, MAX_BYTES=8*1024*1024;
export class CodexReadStateObserver {
  constructor(file,{io=fs,now=Date.now}={}) {this.file=file;this.io=io;this.now=now;this.nextCheck=0;this.stamp=null;this.bucket=null;this.observed=new Map();}
  async poll(hub) {
    const now=this.now();
    const candidates=[...hub.sessions.values()].filter(s=>s.source==='codex'&&['done','error','aborted'].includes(s.status)&&s.viewedRoundId!==s.roundId&&now-(s.endedAt??0)<HOLD_MS);
    const key=s=>JSON.stringify([s.sessionId,s.roundId]);const keys=new Set(candidates.map(key));
    for(const k of this.observed.keys())if(!keys.has(k))this.observed.delete(k);
    if(!candidates.length||now<this.nextCheck)return;
    this.nextCheck=now+INTERVAL_MS;
    try {await this.refresh();}catch {this.stamp=null;this.bucket=null;this.observed.clear();return;}
    if(!this.bucket){this.observed.clear();return;}
    for(const s of candidates){
      if(this.bucket.ids.has(s.sessionId))this.observed.set(key(s),this.bucket.key);
      else if(this.observed.get(key(s))===this.bucket.key){s.viewedRoundId=s.roundId;this.observed.delete(key(s));}
    }
  }
  async refresh(){
    const stat=await this.io.stat(this.file);if(!stat.isFile()||stat.size>MAX_BYTES)throw Error('Unavailable read state');
    const stamp=`${stat.mtimeMs}:${stat.size}`;if(stamp===this.stamp)return;
    const handle=await this.io.open(this.file,'r');let raw;
    try {const chunks=[];let size=0;while(size<=MAX_BYTES){const buffer=Buffer.alloc(Math.min(64*1024,MAX_BYTES+1-size));const {bytesRead}=await handle.read(buffer,0,buffer.length,null);if(!bytesRead)break;chunks.push(buffer.subarray(0,bytesRead));size+=bytesRead;}if(size>MAX_BYTES)throw Error('Oversized read state');raw=Buffer.concat(chunks).toString('utf8');}finally{await handle.close();}
    const state=JSON.parse(raw)['electron-thread-read-state-v1'];
    if(state?.version!==1||!state.unreadByIdentity||typeof state.unreadByIdentity!=='object')throw Error('Unknown read state schema');
    const buckets=[];
    for(const [identity,hosts] of Object.entries(state.unreadByIdentity))for(const [host,ids] of Object.entries(hosts)){
      if(host!=='local'&&!host.startsWith('local:'))continue;
      if(!Array.isArray(ids)||!ids.every(id=>typeof id==='string'))throw Error('Invalid unread ids');
      buckets.push({key:`${identity}/${host}`,ids:new Set(ids)});
    }
    const next=buckets.length===1?buckets[0]:null;
    if(this.bucket?.key!==next?.key)this.observed.clear();
    this.bucket=next;this.stamp=stamp;
  }
}
