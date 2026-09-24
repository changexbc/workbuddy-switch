import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { preview } from 'vite';
import { defaultSettings } from '../src/settings-config.js';
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const server = await preview({preview:{host:'127.0.0.1',port:4191,strictPort:true}});
const browser = await chromium.launch({headless:true});
const context = await browser.newContext({viewport:{width:368,height:600},reducedMotion:'reduce'});
const errors=[], failed=[], resources=[], checks=[];
// One settings route drives every scenario through flags, so the tests never
// depend on route-registration order.
// - calls records the traffic, so a save can be asserted as one re-read + one write
// - failWrites makes the source write fail
// - the first GET can be held open to observe the loading state
const calls=[];
let settings=defaultSettings();
let failWrites=false;
let holdRead=false;
let signalReadStarted=()=>{}, releaseRead=()=>{};
const readStarted=new Promise(resolve=>{signalReadStarted=resolve;});
const readReleased=new Promise(resolve=>{releaseRead=resolve;});
let held=false;
// Integration actions have their own dedicated harness; keep this fixture stable.
await context.route('**/api/integrations', route => route.fulfill({json:{sources:[]}}));
await context.route('**/api/custom-integrations**', route => route.fulfill({json:{version:1,storage:{ok:true,error:null},binaryInstalled:true,templates:[],diagnostics:[]}}));
await context.route('**/api/settings', async route => {
  const method=route.request().method();
  calls.push(method);
  if(method==='GET'&&holdRead&&!held){held=true;signalReadStarted();await readReleased;}
  if(method==='PUT'){
    if(failWrites)return route.fulfill({status:500,json:{error:'磁盘写入失败'}});
    settings=route.request().postDataJSON();
  }
  await route.fulfill({json:settings});
});
await context.addInitScript(() => {
  localStorage.setItem('agent-studio.welcome.v1','1');
  window.EventSource=class {
    constructor(){ window.__stream=this; }
    close(){}
  };
  window.__snapshot=(sessions)=>window.__stream.onmessage({data:JSON.stringify({version:1,ts:Date.now(),ready:true,sources:{codex:{state:'ok'}},sessions,events:[]})});
});
const page=await context.newPage();
// Playwright's locator.isDisabled() reports false for a <fieldset> even when it is
// disabled, so read the IDL property the way src-tauri/src/native_qa.rs does.
const fieldsetDisabled=()=>page.locator('fieldset').evaluate(fieldset=>fieldset.disabled);
page.on('pageerror',e=>errors.push(e.message));
page.on('response',r=>{resources.push(r.url());if(r.status()>=400&&!r.url().endsWith('favicon.ico'))failed.push(r.url());});
await fs.mkdir('artifacts/ui',{recursive:true});
try {
  await page.goto('http://127.0.0.1:4191/desktop.html');
  await page.waitForFunction(()=>window.__stream?.onmessage);
  await page.evaluate(()=>__snapshot([]));
  await page.locator('.desktop-empty').waitFor({state:'visible'});
  assert.equal(await page.locator('.desktop-empty [data-character=cat][data-state=sleep]').count(),1,'empty launch defaults to a sleeping cat');
  assert.equal(await page.locator('.desktop-empty [data-session-id]').count(),0);
  assert.equal(await page.locator('.desktop-empty').getAttribute('title'),'暂无任务，小猫正在休息');
  await page.screenshot({path:'artifacts/ui/empty.png'});
  const session={id:'codex:fixture',source:'codex',sessionId:'fixture',title:'独立悬浮框迁移验证',status:'running',roundId:'r1',updatedAt:Date.now(),steps:[],pending:[]};
  await page.evaluate(s=>__snapshot([s]),session);
  await page.locator('.desktop-avatar[data-status=running]').waitFor();
  session.status='wait';session.pending=[{id:'q1',text:'请选择下一步',questions:[{question:'请选择下一步',options:[{label:'继续',description:'保留当前设置'},{label:'暂停'}]}]}];
  await page.evaluate(s=>__snapshot([s]),session);
  await page.locator('.desktop-automatic-card').waitFor({state:'visible'});
  assert.match(await page.locator('.desktop-automatic-card').innerText(),/请选择下一步/);
  await page.screenshot({path:'artifacts/ui/wait.png'});
  session.status='done';session.pending=[];session.endedAt=Date.now();session.updatedAt=Date.now();
  await page.evaluate(s=>__snapshot([s]),session);
  await page.locator('.desktop-avatar[data-status=done]').waitFor();
  await page.locator('.desktop-automatic-card').waitFor({state:'detached'});
  await page.screenshot({path:'artifacts/ui/done.png'});
  await page.locator('.desktop-grip').click({button:'right'});
  const menu=await page.locator('[role=menuitem]').allTextContents();
  assert(menu.includes('悬浮窗设置'));assert(!menu.some(t=>/3D|办公室/.test(t)));
  // Escape is the rail's whole keyboard story and had no coverage: it has to
  // close the menu, close the card, and hand focus back to the row that opened
  // it, or the rail becomes a keyboard trap. Focus rather than click opens the
  // card, so nothing is launched.
  await page.keyboard.press('Escape');
  await page.locator('.desktop-context-menu').waitFor({state:'hidden'});
  await page.locator('.desktop-avatar').first().focus();
  await page.locator('.desktop-card').waitFor({state:'visible'});
  // Focus has to start inside the card, or it never leaves the avatar and the
  // return path is not exercised at all: the card's controls are removed when it
  // closes, so without an explicit hand-back focus lands on the document body.
  await page.locator('.desktop-preview').focus();
  assert(await page.locator('.desktop-card').evaluate(card=>card.contains(document.activeElement)),'focus is inside the card before Escape');
  await page.keyboard.press('Escape');
  await page.locator('.desktop-card').waitFor({state:'detached'});
  assert.equal(await page.evaluate(()=>document.activeElement?.getAttribute('data-session-id')),session.id,'Escape returns focus to the row that opened the card');
  assert(await page.locator('.desktop-context-menu').isHidden(),'the menu stays closed');
  // Hold the first read open. The retry button must not be reachable while a read
  // is in flight, or a stale success can overwrite a failure the user already saw.
  holdRead=true;
  await page.goto('http://127.0.0.1:4191/desktop-settings.html');
  await readStarted;
  assert.equal(await page.locator('[data-action=retry]').count(),0,'no retry button while the first read is in flight');
  assert.equal(await fieldsetDisabled(),true,'the form is disabled while the first read is in flight');
  releaseRead();
  await page.waitForFunction(()=>document.querySelector('fieldset')?.disabled===false);
  const codexSwitch=page.locator('[data-field=source-codex]');
  assert.equal(await codexSwitch.getAttribute('aria-checked'),'true');
  await page.locator('button[data-style=bot]').click();
  await page.getByRole('combobox', {name:'默认显示数量'}).click();
  await page.getByRole('option', {name:'5 个',exact:true}).click();
  settings.scene.speed=9;
  settings.sources.codex.path='/fresh/path';
  settings.notifications.sound=true;
  calls.length=0;
  await codexSwitch.click();
  assert.equal(await codexSwitch.getAttribute('aria-checked'),'false');
  // The row itself is the label, so the gap between the text and the switch toggles too.
  const animationSwitch=page.locator('[data-field=animation]');
  await page.locator('label.row',{has:animationSwitch}).click({position:{x:200,y:10}});
  assert.equal(await animationSwitch.getAttribute('aria-checked'),'false','clicking the row outside the switch toggles it');
  await animationSwitch.click();
  await page.waitForFunction(()=>document.querySelector('#save-status').textContent.includes('实时生效'));
  assert.deepEqual(calls,['GET','PUT'],'listener auto-save reads current source settings and writes once');
  assert.equal(settings.sources.codex.enabled,false);
  assert.equal(settings.scene.speed,9,'the write carries what the save-time read returned, not the page-load copy');
  assert.equal(settings.sources.codex.path,'/fresh/path');
  assert.equal(settings.notifications.sound,true);
  assert.equal(settings.scene.light,defaultSettings().scene.light,'unrelated settings survive the write');
  await page.reload();
  await page.waitForFunction(()=>document.querySelector('fieldset')?.disabled===false);
  // Focus rings are pseudo-class styles, so a default-state computed-style diff
  // cannot see them. `outline-none` next to `focus-visible:outline-2` silently
  // cancels the ring — `outline-none` sets `--tw-outline-style: none` on the
  // element and `outline-2` reads that variable back as `outline-style` — which
  // left every control with no ring, and the style cards with the browser's own
  // blue one. Assert the rule the migration replaced: `button:focus-visible,
  // input:focus-visible,select:focus-visible{outline:2px solid #477d66;
  // outline-offset:4px}`. rgb(71,125,102) is #477d66.
  // Walk the tab order here, right after the reload: a mouse click leaves
  // Chromium's sequential-focus starting point somewhere Tab no longer advances
  // from, so this has to run before the click-driven scenarios below.
  const rings=[];
  for(let i=0;i<20;i++){
    await page.keyboard.press('Tab');
    const stop=await page.evaluate(()=>{
      const el=document.activeElement;
      if(!el||el===document.body)return null;
      const s=getComputedStyle(el);
      return {tag:el.tagName.toLowerCase(),field:el.getAttribute('data-field')||el.getAttribute('data-action')||el.getAttribute('data-style')||'',ring:`${s.outlineWidth} ${s.outlineStyle} ${s.outlineColor} @${s.outlineOffset}`};
    });
    if(!stop)break;
    rings.push(stop);
  }
  assert.equal(rings.length,13,'settings controls, integration refresh and four independent disclosure buttons are reachable by Tab');
  for(const stop of rings)assert.equal(stop.ring,'2px solid rgb(71, 125, 102) @4px',`${stop.tag}[${stop.field}] keeps the pre-migration focus ring`);
  assert.equal(await page.locator('button[data-style=bot]').getAttribute('aria-pressed'),'true');
  assert.equal(await page.locator('[data-field=visibleCount]').innerText(),'5 个');
  assert.equal(await page.locator('[data-field=source-codex]').getAttribute('aria-checked'),'false');
  await page.setViewportSize({width:480,height:700});
  await page.evaluate(()=>window.scrollTo(0,0));
  for (const viewport of [{width:480,height:700},{width:420,height:520}]) {
    await page.setViewportSize(viewport);
    assert.equal(await page.locator('footer,[data-action=save],.integration-columns').count(),0,'manual save and redundant captions are gone');
    assert.equal(await page.locator('.settings-group').count(),3,'display, Agent and startup share bordered groups');
    await page.evaluate(()=>scrollTo(0,document.body.scrollHeight));
    const lastBottom=await page.locator('[data-field=autostart]').evaluate(el=>el.getBoundingClientRect().bottom);
    assert(lastBottom<=viewport.height,'last setting remains reachable');
    await page.screenshot({path:`artifacts/ui/settings-bottom-${viewport.width}.png`});
  }
  await page.setViewportSize({width:480,height:700});
  await page.evaluate(()=>window.scrollTo(0,0));
  const countSelect=page.getByRole('combobox',{name:'默认显示数量'});
  // Radix dismisses an open popup on window resize. Let the viewport resize
  // event finish before testing keyboard opening, otherwise it closes the popup.
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  await countSelect.press('ArrowDown');
  await page.getByRole('listbox').waitFor();
  await page.waitForFunction(()=>document.activeElement?.getAttribute('role')==='option');
  assert.equal(await page.getByRole('option').count(),14);
  await page.keyboard.press('End');
  await page.waitForFunction(()=>document.activeElement?.textContent==='16 个');
  await page.keyboard.press('Enter');
  await page.getByRole('listbox').waitFor({state:'hidden'});
  assert.equal(await countSelect.innerText(),'16 个','keyboard can reach the last option');
  await countSelect.click();
  await page.keyboard.press('Escape');
  await page.getByRole('listbox').waitFor({state:'hidden'});
  assert(await countSelect.evaluate(el=>el===document.activeElement),'Escape returns focus to the select');
  await countSelect.click();
  await page.getByRole('option',{name:'5 个',exact:true}).click();
  await countSelect.click();
  await page.screenshot({path:'artifacts/ui/settings-select.png'});
  await page.keyboard.press('Escape');
  // The switch thumb animates on load; wait it out so the screenshot is stable.
  await page.waitForTimeout(300);
  await page.screenshot({path:'artifacts/ui/settings.png',fullPage:true});
  assert.deepEqual(errors,[]);assert.deepEqual(failed,[]);
  // A failed source write must never be reported as a full success.
  failWrites=true;
  await page.locator('[data-field=source-workbuddy]').click();

  await page.waitForFunction(()=>/^部分更改可能已生效，请重试：/.test(document.querySelector('#save-status').textContent));
  assert.equal(await fieldsetDisabled(),false,'the form is usable again after a failed save');
  assert.equal(settings.sources.workbuddy.enabled,true,'the rejected write did not reach the server');
  // The real partial case: the source write lands, the preference write does not.
  failWrites=false;
  await page.locator('[data-action=retry-save]').click();
  await page.waitForFunction(()=>document.querySelector('#save-status').textContent.includes('实时生效'));
  const storedBefore=await page.evaluate(()=>localStorage.getItem('astra.desktop.preferences.v1'));
  await page.evaluate(()=>{
    const original=Storage.prototype.setItem;
    window.__restoreStorage=()=>Storage.prototype.setItem=original;
    Storage.prototype.setItem=function(key,value){
      if(key==='astra.desktop.preferences.v1')throw new Error('prefs disk');
      return original.call(this,key,value);
    };
  });
  calls.length=0;
  await page.evaluate(()=>{document.querySelector('[data-field=source-codeg]').click();document.querySelector('[data-field=animation]').click();});

  await page.waitForFunction(()=>/^部分更改可能已生效，请重试：/.test(document.querySelector('#save-status').textContent));
  assert.equal(settings.sources.codeg.enabled,false,'the source write really landed, so this is a partial save');
  assert.equal(await page.evaluate(()=>localStorage.getItem('astra.desktop.preferences.v1')),storedBefore,'the preference write really failed');
  assert.deepEqual(calls,['GET','PUT'],'a partial save still re-reads once and writes once');
  assert.equal(await page.locator('#save-status').textContent(),'部分更改可能已生效，请重试：prefs disk');
  await page.evaluate(()=>window.__restoreStorage());
  await page.locator('[data-action=retry-save]').click();
  await page.waitForFunction(()=>document.querySelector('#save-status').textContent.includes('实时生效'));
  assert.deepEqual(calls,['GET','PUT'],'retry does not replay successful source write');
  assert.deepEqual(errors,[]);
  assert(!resources.some(url=>/three|\.glb|\.exr|\/models\//i.test(url)));
  checks.push(
    'empty','running','wait reminder','quiet done','settings persistence',
    'no retry while a read is in flight','row label toggles the switch','automatic changes persist without submit',
    'save re-reads then writes once','source write failure is not reported as success',
    'partial save is not reported as success','shared schema preserved',
    'focus rings match the pre-migration rule','no 3D resources',
  );
  await railMigrationRegressions();
  await railMotion();
  await railDesktopPath();
  await fs.writeFile('artifacts/ui/report.json',JSON.stringify({passed:true,errors,failed,menu,checks},null,2));
  console.log('PASS: rail lifecycle, question reminder, quiet completion, settings persistence, no office resources');
} finally {await browser.close();await new Promise(resolve=>server.httpServer.close(resolve));}

async function railMigrationRegressions(){
  const context=await browser.newContext({viewport:{width:368,height:600},reducedMotion:'no-preference'});
  try {
    await context.addInitScript(()=>{
      localStorage.setItem('agent-studio.welcome.v1','1');
      window.EventSource=class{constructor(){window.__stream=this;}close(){}};
      window.__snapshot=sessions=>window.__stream.onmessage({data:JSON.stringify({version:1,ts:Date.now(),ready:true,sources:{codex:{state:'ok'}},sessions,events:[]})});
      window.__cardEntrances=0;
      const animate=Element.prototype.animate;
      Element.prototype.animate=function(...args){
        if(this.classList.contains('desktop-automatic-card'))window.__cardEntrances++;
        return animate.apply(this,args);
      };
    });
    const page=await context.newPage();
    page.on('pageerror',error=>errors.push(`migration: ${error.message}`));
    await page.goto('http://127.0.0.1:4191/desktop.html');
    await page.waitForFunction(()=>window.__stream?.onmessage);
    const session={id:'codex:regression',source:'codex',sessionId:'regression',title:'迁移回归验证',status:'running',roundId:'r1',updatedAt:4102444800000,steps:[],pending:[]};
    await page.evaluate(s=>__snapshot([s]),session);
    await page.locator('.desktop-avatar').waitFor();
    await page.waitForTimeout(500);
    session.status='wait';session.pending=[{id:'q1',text:'请选择下一步'}];
    await page.evaluate(s=>__snapshot([s]),session);
    await page.locator('.desktop-automatic-card').waitFor();
    await page.waitForTimeout(350);
    const entrances=await page.evaluate(()=>window.__cardEntrances);
    assert(entrances>0,'the initial automatic card really animates');
    await page.evaluate(s=>__snapshot([s]),session);
    await page.waitForTimeout(100);
    const repeated=await page.evaluate(()=>window.__cardEntrances);
    await page.locator('.desktop-grip').click({button:'right'});
    await page.locator('.desktop-context-menu').waitFor({state:'visible'});
    await page.locator('.desktop-grip').click();
    const menuClosed=await page.locator('.desktop-context-menu').isHidden();
    await page.keyboard.press('Escape');
    const eyes=()=>page.locator('.desktop-list .companion-lids path').evaluateAll(paths=>paths.map(path=>path.getAttribute('d')));
    const before=await eyes();
    assert(before.length===2&&before.every(Boolean),'the waiting avatar starts with two drawn eyes');
    const styles=[];
    for(const avatarStyle of ['bot','animal']){
      await page.evaluate(avatarStyle=>{
        localStorage.setItem('astra.desktop.preferences.v1',JSON.stringify({avatarStyle,visibleCount:8,animation:true}));
        window.dispatchEvent(new StorageEvent('storage',{key:'astra.desktop.preferences.v1'}));
      },avatarStyle);
      await page.locator(`.desktop-list svg[data-style=${avatarStyle}]`).waitFor();
      styles.push(await eyes());
    }
    assert.deepEqual({replays:repeated-entrances,menuClosed,styles},{replays:0,menuClosed:true,styles:[before,before]},'unchanged snapshots preserve animation, outside clicks close menus, and both style switches preserve eyes');
    checks.push('unchanged snapshots do not replay reminder entrance','outside click closes the rail menu','both avatar style switches preserve status eyes');
  } finally {await context.close();}
}

/**
 * The rail with motion enabled. Every automated screenshot diff runs under
 * `prefers-reduced-motion: reduce`, which is exactly the path where `retire()`
 * removes a row immediately — so the departing ghost is only ever exercised
 * here. The observer records the ghost's box at the moment it appears, because
 * a 200ms fade is too short to poll for reliably.
 */
async function railMotion(){
  const context=await browser.newContext({viewport:{width:368,height:600},reducedMotion:'no-preference'});
  await context.route('**/api/settings',route=>route.fulfill({json:defaultSettings()}));
  await context.addInitScript(()=>{
    localStorage.setItem('agent-studio.welcome.v1','1');
    window.EventSource=class{constructor(){window.__stream=this;}close(){}};
    window.__snapshot=sessions=>window.__stream.onmessage({data:JSON.stringify({version:1,ts:Date.now(),ready:true,sources:{codex:{state:'ok'}},sessions,events:[]})});
  });
  const page=await context.newPage();
  page.on('pageerror',e=>errors.push(`motion: ${e.message}`));
  await page.goto('http://127.0.0.1:4191/desktop.html');
  await page.waitForFunction(()=>window.__stream?.onmessage);
  await page.evaluate(()=>__snapshot([]));
  await page.evaluate(()=>{
    window.__ghosts=[];
    const seen=new WeakSet();
    new MutationObserver(()=>{
      for(const node of document.querySelectorAll('.desktop-departing')){
        if(seen.has(node))continue;
        seen.add(node);
        const r=node.getBoundingClientRect();
        window.__ghosts.push({tag:node.tagName.toLowerCase(),x:r.x,y:r.y,width:r.width,height:r.height,inert:node.inert,animations:node.getAnimations().length});
      }
    }).observe(document.querySelector('#desktop-rail'),{childList:true,subtree:true});
  });
  const running={id:'codex:fixture',source:'codex',sessionId:'fixture',title:'独立悬浮框迁移验证',status:'running',roundId:'r1',updatedAt:4102444800000,steps:[],pending:[]};
  await page.evaluate(s=>__snapshot([s]),running);
  await page.locator('.desktop-avatar').waitFor();
  // Let the entrance animation finish so the recorded box is the resting one.
  await page.waitForTimeout(500);
  const row=await page.locator('.desktop-avatar').boundingBox();
  await page.evaluate(()=>__snapshot([]));
  await page.locator('.desktop-sleeping').waitFor();
  const sleeping=await page.locator('.desktop-sleeping').boundingBox();
  assert(Math.abs(sleeping.x-row.x)<1.5&&Math.abs(sleeping.y-row.y)<1.5,'the last portrait falls asleep at the same position');
  assert.equal(await page.locator('.desktop-departing').count(),0,'the last avatar is not also retired as a ghost');
  assert.equal(await page.locator('.desktop-sleeping [data-state="sleep"]').count(),1);
  assert.equal(await page.locator('.desktop-sleeping [data-character="cat"]').count(),1);
  assert.equal(await page.locator('.desktop-sleeping .sleep-symbols').textContent(),'zZZ');
  assert.equal(await page.locator('.desktop-avatar').count(),0,'the sleeping portrait is not a session');
  await page.waitForTimeout(900);
  await page.screenshot({path:'artifacts/ui/sleeping-cat.png'});
  // Expanding the list grows the rail, and the growth is animated rather than
  // snapped; reduced motion is asserted to skip it by the screenshot diff.
  const ghostsBefore=await page.evaluate(()=>window.__ghosts.length);
  const ten=Array.from({length:10},(_,i)=>({...running,id:`codex:fixture-${i}`,sessionId:`fixture-${i}`}));
  await page.evaluate(list=>__snapshot(list),ten);
  await page.locator('.desktop-overflow').waitFor({state:'visible'});
  await page.locator('.desktop-overflow').click();
  await page.waitForFunction(()=>document.querySelector('.desktop-strip').getAnimations().some(a=>a.effect?.getKeyframes().some(keyframe=>'height' in keyframe)),null,{timeout:2000});
  await page.waitForTimeout(400);
  assert.equal(await page.locator('.desktop-avatar:visible').count(),10,'expanding reveals every row');
  // Collapsing is the other half of the same behaviour and used to go untested.
  await page.locator('.desktop-overflow').click();
  await page.waitForTimeout(400);
  assert.equal(await page.locator('.desktop-avatar:visible').count(),8,'collapsing hides the overflow again');
  assert.equal(await page.locator('.desktop-overflow').getAttribute('aria-expanded'),'false');
  assert.equal(await page.evaluate(()=>window.__ghosts.length),ghostsBefore,'expanding and collapsing retires nothing');
  // Hover raises the portrait by 1px; refreshed snapshots must not move its card.
  const hoverRow=page.locator('.desktop-list .desktop-avatar').nth(1);
  await hoverRow.hover();
  await page.waitForTimeout(350);
  const cardAnchor=()=>page.locator('#desktop-session-card').evaluate(el=>({top:el.style.top,pointer:el.style.getPropertyValue('--pointer-top')}));
  const anchorBefore=await cardAnchor();
  await page.evaluate(list=>__snapshot(list),ten);
  await page.waitForTimeout(100);
  assert.deepEqual(await cardAnchor(),anchorBefore,'hover transform does not shift the card on a snapshot');
  await page.mouse.move(0,550);
  await page.waitForTimeout(400);
  // A FLIP baseline taken from a rect would include the transform of a reorder
  // that is still running, so a commit landing mid-animation would stack a
  // second, spurious animation on a row that never moved. Only rows inside the
  // list count: a retiring ghost is also a `.desktop-avatar` and legitimately
  // owns an animation of its own. The row that leaves has to come from the
  // middle, or nothing below it moves and there is no reorder to interrupt.
  const animationCounts=()=>page.evaluate(()=>[...document.querySelectorAll('.desktop-list .desktop-avatar')].map(avatar=>avatar.getAnimations().length));
  const survivors=ten.filter((_,index)=>index!==2);
  await page.locator('.desktop-overflow').click();
  await page.waitForTimeout(400);
  await page.evaluate(list=>__snapshot(list),survivors);
  await page.waitForFunction(()=>[...document.querySelectorAll('.desktop-list .desktop-avatar')].some(a=>a.getAnimations().length>0),null,{timeout:2000});
  await page.waitForTimeout(40);
  await page.evaluate(list=>__snapshot(list),survivors);
  const stacked=await animationCounts();
  assert(stacked.some(count=>count===1),`the reorder really was still running when the second commit landed (saw ${JSON.stringify(stacked)})`);
  assert(stacked.every(count=>count<=1),`a commit during a reorder does not stack a second animation on one row (saw ${JSON.stringify(stacked)})`);
  await page.waitForTimeout(500);
  assert.deepEqual(errors,[]);
  // The last visible identity survives as appearance only, including non-cat variants.
  const finalPortrait=await page.locator('.desktop-list .desktop-avatar:visible .companion-avatar').last().evaluate(el=>({character:el.dataset.character,style:el.dataset.style,shape:el.querySelector('.companion-attention > g').innerHTML}));
  await page.evaluate(()=>__snapshot([]));
  await page.locator('.desktop-sleeping').waitFor();
  const restingPortrait=await page.locator('.desktop-sleeping .companion-avatar').evaluate(el=>({character:el.dataset.character,style:el.dataset.style,shape:el.querySelector('.companion-attention > g').innerHTML}));
  assert.deepEqual(restingPortrait,finalPortrait,'the final animal keeps its shape and color');
  await page.evaluate(s=>__snapshot([s]),running);
  await page.locator('.desktop-list .desktop-avatar').waitFor();
  assert.equal(await page.locator('.desktop-sleeping').count(),0,'new work removes the sleeping placeholder');
  checks.push('last avatar falls asleep in place and wakes for new work','rail height animation runs with motion enabled','expanding and collapsing both animate','a mid-reorder commit does not stack animations');
  await context.close();
}

/**
 * The rail as the desktop shell drives it.
 *
 * `host.ts` has an embedding path for exactly this: when the page runs inside
 * another window that provides `__AGENT_STUDIO_EMBED_HOST__`, `isDesktop()` is
 * true and every command goes through that host instead of Tauri. Running the
 * rail in an iframe therefore exercises the native code path — `set_hit_regions`
 * included — and makes the payload observable, which a browser build cannot do
 * because it never reports hit regions at all.
 */
async function railDesktopPath(){
  const context=await browser.newContext({viewport:{width:420,height:640},reducedMotion:'reduce'});
  await context.route('**/api/settings',route=>route.fulfill({json:defaultSettings()}));
  await context.route('**/qa-host.html',route=>route.fulfill({contentType:'text/html',body:'<!doctype html><html><body style="margin:0"></body></html>'}));
  const page=await context.newPage();
  page.on('pageerror',e=>errors.push(`host: ${e.message}`));
  await page.goto('http://127.0.0.1:4191/qa-host.html');
  await page.evaluate(()=>{
    window.__hostCalls=[];
    window.__listeners=new Map();
    window.__state={snapshot:null,connected:true};
    window.__AGENT_STUDIO_EMBED_HOST__={
      desktop:true,
      listen:async(event,handler)=>{
        const handlers=window.__listeners.get(event)??[];
        handlers.push(handler);
        window.__listeners.set(event,handlers);
        return()=>window.__listeners.set(event,(window.__listeners.get(event)??[]).filter(entry=>entry!==handler));
      },
      invoke:async(command,args)=>{
        window.__hostCalls.push({command,args});
        if(command==='plugin:agent-studio|monitor_state')return{snapshot:window.__state.snapshot,connected:window.__state.connected};
        if(command==='plugin:agent-studio|rail_settings_get')return{avatarStyle:'animal',visibleCount:8,animation:true,autostart:false,autostartSupported:false};
        return null;
      },
      enableNotifications:async()=>'granted',
    };
    window.__emit=(event,payload)=>{for(const handler of window.__listeners.get(event)??[])handler({payload});};
    window.__regions=()=>{
      const call=[...window.__hostCalls].reverse().find(entry=>entry.command==='plugin:agent-studio|set_hit_regions');
      return call?call.args.regions:null;
    };
    const frame=document.createElement('iframe');
    frame.id='rail';
    frame.src='/desktop.html';
    frame.style.cssText='width:368px;height:600px;border:0;display:block';
    document.body.append(frame);
  });
  const rail=page.frameLocator('#rail');
  await rail.locator('.desktop-empty').waitFor();
  await page.waitForFunction(()=>window.__regions()!==null);
  const empty=await page.evaluate(()=>window.__regions());
  // The empty rail is a surface plus the grip and the menu's two items; nothing
  // else exists yet, and the disclosure button is still hidden.
  assert(empty.length>0,'the empty rail reports something to click');
  assert(!empty.some(region=>region.width<=0||region.height<=0),'no zero-sized region is reported');
  const question=[{id:'q1',text:'请选择下一步',questions:[{question:'请选择下一步',options:[{label:'继续'}]}]}];
  await page.evaluate(session=>{
    window.__state.snapshot={version:1,ts:1,ready:true,sources:{codex:{state:'ok'}},sessions:[session],events:[]};
    window.__emit('monitor-state',window.__state.snapshot);
  },{id:'codex:fixture',source:'codex',sessionId:'fixture',title:'独立悬浮框迁移验证',status:'wait',roundId:'r1',updatedAt:4102444800000,steps:[],pending:question});
  await rail.locator('.desktop-automatic-card').waitFor({state:'visible'});
  const cardBox=await rail.locator('.desktop-automatic-card').boundingBox();
  // Wait for a region that is the card's own box grown by the 10px padding, not
  // for "something taller than a row": the empty rail's own surface is 92px
  // padded, so a height threshold alone would already be satisfied before the
  // card existed and would prove nothing.
  const padded=(region,box)=>Math.abs(region.x-(box.x-10))<2&&Math.abs(region.y-(box.y-10))<2
    &&Math.abs(region.width-(box.width+20))<2&&Math.abs(region.height-(box.height+20))<2;
  await page.waitForFunction(box=>window.__regions().some(region=>Math.abs(region.x-(box.x-10))<2&&Math.abs(region.y-(box.y-10))<2&&Math.abs(region.width-(box.width+20))<2&&Math.abs(region.height-(box.height+20))<2),cardBox);
  const withCard=await page.evaluate(()=>window.__regions());
  const avatarBox=await rail.locator('.desktop-avatar').boundingBox();
  assert(withCard.some(region=>Math.abs(region.x+region.width/2-(avatarBox.x+avatarBox.width/2))<2&&Math.abs(region.height-avatarBox.height)<2),'the avatar is reported as a clickable control');
  assert(withCard.some(region=>region.cursor==='grab'),'the drag grip is reported with the grab cursor');
  assert(withCard.some(region=>padded(region,cardBox)),'the automatic card is reported as a padded surface');
  assert(withCard.length>empty.length,'adding a session adds regions rather than replacing them');
  // Every control is reported at its own size, unpadded. Checking the preview
  // matters because dropping its registration would still leave the region count
  // growing thanks to the avatar, the card surface and the dismiss button.
  const previewBox=await rail.locator('.desktop-preview').boundingBox();
  assert(withCard.some(region=>Math.abs(region.x-previewBox.x)<1&&Math.abs(region.y-previewBox.y)<1&&Math.abs(region.width-previewBox.width)<1&&Math.abs(region.height-previewBox.height)<1),'the open-preview button is reported as a control at its own size');
  const dismissBox=await rail.locator('.desktop-dismiss').boundingBox();
  assert(withCard.some(region=>Math.abs(region.x-dismissBox.x)<1&&Math.abs(region.y-dismissBox.y)<1&&Math.abs(region.width-dismissBox.width)<1&&Math.abs(region.height-dismissBox.height)<1),'the dismiss button is reported as a control at its own size, not as part of the card surface');
  // A hidden surface must contribute nothing. Padding is what makes this worth
  // asserting: a 0x0 box would otherwise become a valid 20x20 region at a
  // negative offset, which the host accepts and which would put a clickable hole
  // in the corner of the window. The menu is hidden at this point.
  assert(withCard.every(region=>region.x>=0&&region.y>=0),'no region sits outside the window, which is what a hidden surface would produce');
  assert(await rail.locator('.desktop-context-menu').isHidden(),'the menu really is hidden while this is asserted');
  // Disposing must release every subscription: a snapshot after teardown must
  // not repaint the rail. The event has to be dispatched inside the frame —
  // dispatching it on the parent window never reaches the rail's own listener.
  const frameElement=await page.locator('#rail').elementHandle();
  const railFrame=await frameElement.contentFrame();
  await railFrame.evaluate(()=>window.dispatchEvent(new PageTransitionEvent('pagehide')));
  const afterDispose=await page.evaluate(()=>window.__hostCalls.length);
  await page.evaluate(()=>{
    window.__emit('monitor-connection','offline');
    window.__emit('monitor-state',{version:1,ts:9,ready:true,sources:{codex:{state:'ok'}},sessions:[],events:[]});
  });
  await page.waitForTimeout(150);
  assert.equal(await rail.locator('.desktop-strip').getAttribute('data-connection'),'connected','a disposed rail ignores later events');
  assert.equal(await rail.locator('.desktop-avatar').count(),1,'a disposed rail keeps the rows it last rendered');
  assert.equal(await page.evaluate(()=>window.__hostCalls.length),afterDispose,'a disposed rail stops talking to the host');
  assert.deepEqual(errors,[]);
  checks.push('hit regions cover the visible surfaces and controls','hit regions ignore hidden and departing elements','dispose releases every subscription');
  await context.close();
}
