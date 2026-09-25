import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {preview} from 'vite';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const server=await preview({preview:{host:'127.0.0.1',port:4191,strictPort:true}});
const browser=await chromium.launch({headless:true});
try {
  const context=await browser.newContext({viewport:{width:420,height:640}});
  await context.route('**/qa-host.html',route=>route.fulfill({contentType:'text/html',body:'<!doctype html><html><body style="margin:0"></body></html>'}));
  const page=await context.newPage();
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  await page.goto('http://127.0.0.1:4191/qa-host.html');
  // The embedded host supplies a real desktop bridge to the rail iframe.
  await page.evaluate(()=>{
    window.__listeners=new Map();window.__opened=[];window.__failOpen=false;
    window.__AGENT_STUDIO_EMBED_HOST__={desktop:true,
      listen:async(event,handler)=>{const list=window.__listeners.get(event)||[];list.push(handler);window.__listeners.set(event,list);return()=>window.__listeners.set(event,(window.__listeners.get(event)||[]).filter(item=>item!==handler));},
      invoke:async(command,args)=>{
        if(command==='plugin:agent-studio|monitor_state')return{snapshot:null,connected:true};
        if(command==='plugin:agent-studio|rail_settings_get')return{avatarStyle:'animal',visibleCount:8,animation:true,autostart:false,autostartSupported:false};
        if(command==='plugin:agent-studio|open_session_url'){window.__opened.push(args.url);if(window.__failOpen)throw Error('open failed');}
        return null;
      },enableNotifications:async()=>'granted'};
    window.__emit=(event,payload)=>{for(const handler of window.__listeners.get(event)||[])handler({payload});};
    const frame=document.createElement('iframe');frame.id='rail';frame.src='/desktop.html';frame.style.cssText='width:368px;height:600px;border:0';document.body.append(frame);
  });
  const rail=page.frameLocator('#rail');
  const avatar=rail.locator('.desktop-avatar[data-session-id="codex:fixture"]');
  const send=async(roundId='r1',status='aborted')=>page.evaluate(({roundId,status})=>{
    const now=Date.now();window.__emit('monitor-state',{version:1,ts:now,ready:true,sources:{codex:{state:'ok'}},sessions:[{id:'codex:fixture',source:'codex',sessionId:'fixture',project:'agent-companion',title:'test',status,roundId,updatedAt:now,endedAt:status==='running'?null:now,steps:[],pending:[]}],events:[]});
  },{roundId,status});
  await page.waitForFunction(()=>window.__listeners.get('monitor-state')?.length);
  await send('r1','running');await avatar.waitFor();
  await send();await avatar.waitFor();
  assert.equal(await rail.locator('.desktop-countdown').count(),0);
  await avatar.click();
  await rail.locator('.desktop-dot-countdown .desktop-countdown-progress').waitFor();
  const placement=await rail.locator('.desktop-dot-countdown').evaluate(dot=>{
    const badge=dot.getBoundingClientRect(), ring=dot.querySelector('.desktop-countdown').getBoundingClientRect();
    const animation=dot.querySelector('.desktop-countdown-progress').getAnimations()[0];
    return {badge:{x:badge.x,y:badge.y,width:badge.width,height:badge.height},ring:{x:ring.x,y:ring.y,width:ring.width,height:ring.height},duration:animation?.effect?.getTiming().duration};
  });
  assert(placement.ring.width<=placement.badge.width+7 && placement.ring.height<=placement.badge.height+7,'countdown stays around the status badge');
  assert(Math.abs(placement.ring.x+placement.ring.width/2-placement.badge.x-placement.badge.width/2)<1,'countdown is centred on the status badge');
  assert.equal(placement.duration,10000,'one entry uses a fixed ten-second animation');
  await fs.mkdir('artifacts/ui',{recursive:true});
  await page.screenshot({path:'artifacts/ui/rail-countdown.png'});
  assert.equal(await rail.locator('.desktop-countdown text').count(),0);
  assert.deepEqual(await page.evaluate(()=>window.__opened),['codex://threads/fixture']);
  const first=await rail.locator('.desktop-countdown-progress').evaluate(node=>node.getAnimations()[0]?.startTime);
  const box=await avatar.boundingBox();
  await page.evaluate(point=>{window.__emit('agent-studio-pointer',point);window.__emit('agent-studio-pointer',point);},{x:box.x+box.width/2,y:box.y+box.height/2});
  const nativeRepeat=await rail.locator('.desktop-countdown-progress').evaluate(node=>node.getAnimations()[0]?.startTime);
  assert.equal(nativeRepeat,first,'native pointer updates do not restart the same entry');
  await page.mouse.move(10,10);await page.waitForTimeout(100);
  await avatar.hover();
  const second=await rail.locator('.desktop-countdown-progress').evaluate(node=>node.getAnimations()[0]?.startTime);
  assert(second!==first,'entry remounts the ring and restarts progress');
  await page.mouse.move(10,10);
  await page.evaluate(()=>window.__emit('monitor-connection','offline'));
  await avatar.waitFor({state:'detached',timeout:12000});
  await rail.locator('.desktop-card').waitFor({state:'detached'});
  await page.evaluate(()=>window.__emit('monitor-connection','connected'));
  await send('r2','running');await avatar.waitFor();
  assert.equal(await rail.locator('.desktop-countdown').count(),0,'new round has no old countdown');
  await send('r2','aborted');await page.evaluate(()=>window.__failOpen=true);
  await avatar.click();await rail.locator('.desktop-countdown').waitFor({state:'detached'});
  assert.equal(await avatar.count(),1,'open failure retains the row');
  await page.evaluate(()=>window.__failOpen=false);
  await send('r3','running');await send('r3','done');
  await avatar.click();await rail.locator('.desktop-dot-countdown .desktop-countdown-progress').waitFor();
  const completedBadge=await rail.locator('.desktop-dot-countdown').evaluate(dot=>({
    color:getComputedStyle(dot).backgroundColor,
    check:getComputedStyle(dot,'::after').backgroundImage,
  }));
  assert.equal(completedBadge.color,'rgb(57, 135, 108)','the completed badge stays green during countdown');
  assert.match(completedBadge.check,/data:image\/svg\+xml/,'the white check remains inside the completed badge');
  await page.screenshot({path:'artifacts/ui/rail-countdown-done.png'});
  assert.deepEqual(errors,[]);
  console.log('PASS: countdown around badge, completed check retained, ring reset, offline expiry, new round, open failure');
  await context.close();
} finally {await browser.close();await new Promise(resolve=>server.httpServer.close(resolve));}
