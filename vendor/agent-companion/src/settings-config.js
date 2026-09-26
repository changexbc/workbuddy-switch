// @ts-check
/** @typedef {import('./types/settings.js').Settings} Settings */
/** @typedef {import('./types/settings.js').SourceConfig} SourceConfig */
/** @typedef {import('./types/settings.js').SourceId} SourceId */

/** @type {readonly SourceId[]} */
export const SOURCE_IDS=['codex','workbuddy','codebuddy-ide','codeg'];
/** @returns {Settings} */
export const defaultSettings=()=>({version:1,sources:/** @type {Record<SourceId, SourceConfig>} */(Object.fromEntries(SOURCE_IDS.map(id=>[id,id==='workbuddy'?{enabled:true,path:'',logWatch:true}:{enabled:true,path:''}]))),monitor:{avatarStyle:'animal',railVisibleCount:8,autoDiscover:true,retentionHours:.5,assignment:'auto',seats:Array(8).fill('auto')},scene:{light:'day',weather:'clear',lightning:true,door:false,ceiling:false,playing:true,speed:1,maxFps:60,renderResolution:'native',showPerformance:false,reducedMotion:false,defaultView:'program'},notifications:{desktop:false,wait:true,error:true,done:true,sound:false},general:{mode:'live',rememberView:true},schedule:{enabled:false,start:'09:00',end:'18:00',deferBusy:true}});
/**
 * Validates externally supplied settings JSON (settings file, native command
 * payload, HTTP body). This function is the single place where untyped input
 * becomes `Settings`, so it reads the payload through a loose alias; the checks
 * themselves and their order are unchanged from the pre-migration version.
 * @param {unknown} input
 * @returns {Settings}
 */
export function validateSettings(input){
 /** @type {any} */ const raw=input;
 const d=defaultSettings();
 /** @type {any} */ const merged=d;
 if(!raw||raw.version!==1)throw Error('配置版本无效');
 for(const id of SOURCE_IDS){const s=raw.sources?.[id];if(!s||typeof s.enabled!=='boolean'||typeof s.path!=='string'||s.path.length>2048||s.path.includes('\0'))throw Error('Agent 配置无效');if(id==='workbuddy'&&s.logWatch!==undefined&&typeof s.logWatch!=='boolean')throw Error('Agent 配置无效');d.sources[id]={enabled:s.enabled,path:s.path.trim()};if(id==='workbuddy')d.sources[id].logWatch=s.logWatch!==false;}
 for(const group of ['monitor','scene','notifications','general','schedule']){if(!raw[group])throw Error('缺少配置分组');for(const k of Object.keys(merged[group])){const value=group==='monitor'&&['railVisibleCount','avatarStyle'].includes(k)&&raw[group][k]===undefined?merged[group][k]:group==='scene'&&['maxFps','renderResolution','showPerformance','weather','lightning'].includes(k)&&raw[group][k]===undefined?merged[group][k]:raw[group][k];if(typeof merged[group][k]==='boolean'&&typeof value!=='boolean')throw Error('开关配置无效');merged[group][k]=value;}}
 /** @type {(v: unknown, values: unknown[]) => void} */
 const choices=(v,values)=>{if(!values.includes(v))throw Error('配置选项无效');};
 choices(d.monitor.avatarStyle,['animal','bot']);
 choices(d.monitor.railVisibleCount,[3,4,5,6,7,8,9,10,11,12,13,14,15,16]);
 choices(d.monitor.retentionHours,[0,.5,24,168]);choices(d.monitor.assignment,['auto','fixed']);if(!Array.isArray(d.monitor.seats)||d.monitor.seats.length!==8||d.monitor.seats.some(s=>!['auto',...SOURCE_IDS].includes(s)))throw Error('工位配置无效');
 choices(d.scene.light,['day','night','auto']);choices(d.scene.weather,['clear','overcast','rain','downpour','thunderstorm','wind','auto']);choices(d.scene.speed,[1,2,4]);choices(d.scene.maxFps,[30,60]);choices(d.scene.renderResolution,['native','balanced','low']);choices(d.scene.defaultView,['all','program','device']);choices(d.general.mode,['live','demo']);
 if(!/^([01]\d|2[0-3]):[0-5]\d$/.test(d.schedule.start)||!/^([01]\d|2[0-3]):[0-5]\d$/.test(d.schedule.end)||d.schedule.start>=d.schedule.end)throw Error('上班时间必须早于下班时间');
 return d;
}
