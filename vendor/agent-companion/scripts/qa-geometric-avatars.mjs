import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright';

const server = await createServer({server:{host:'127.0.0.1',port:4193,strictPort:true}});
await server.listen();
const browser = await chromium.launch({headless:true});
const page = await browser.newPage({viewport:{width:900,height:520}});
try {
  // Load Vite's real module without mounting the rail controller in this gallery.
  await page.route('**/gallery', route => route.fulfill({contentType:'text/html',body:'<!doctype html><html><head></head><body></body></html>'}));
  await page.goto('http://127.0.0.1:4193/gallery');
  const identities = await page.evaluate(async () => {
    const {createAvatar, avatarIdentity, updateAvatar} = await import('/src/desktop/avatar.ts');
    window.updateAvatar = updateAvatar;
    const style = document.createElement('style');
    style.textContent = 'body{margin:0;padding:32px;background:#f4f6ed;color:#263c31;font:14px system-ui}main{display:grid;grid-template-columns:repeat(5,1fr);gap:24px}figure{margin:0;text-align:center}svg{width:92px;height:92px;margin:0 auto 8px}.actual svg{width:32px;height:32px}.actual{margin-top:12px}';
    document.head.append(style);
    const main = document.createElement('main');
    document.body.append(main);
    for(let i=0;i<10;i++) {
      const figure = document.createElement('figure');
      const avatar = createAvatar('bot',i);
      avatar.classList.add('companion-visible');
      updateAvatar(avatar,'running');
      figure.append(avatar);
      const label = document.createElement('figcaption');
      label.textContent = avatarIdentity('bot',i).name;
      figure.append(label);
      const actual = document.createElement('div');
      actual.className = 'actual';
      actual.append(avatar.cloneNode(true));
      figure.append(actual);
      main.append(figure);
    }
    return Array.from({length:11},(_,i)=>avatarIdentity('bot',i));
  });
  assert.equal(new Set(identities.slice(0,10).map(x=>x.name)).size,10);
  assert.equal(identities[3].name,'爱心伙伴');
  assert.deepEqual(identities[10],identities[0]);
  const motion = () => page.evaluate(() => Array.from(document.querySelectorAll('figure > svg'), svg => {
    const group = svg.querySelector('.companion-work');
    const css = getComputedStyle(group);
    return {name:css.animationName,state:css.animationPlayState,transform:css.transform};
  }));
  const before = await motion();
  assert(before.every(x=>x.name !== 'none' && x.state === 'running'));
  assert.equal(before[3].name,'companion-heart-work');
  await page.waitForTimeout(250);
  const after = await motion();
  assert(after.some((x,i)=>x.transform !== before[i].transform),'working poses change over time');
  for(const state of ['idle','sleep','wait','done','error','offline']) {
    await page.evaluate(state=>document.querySelectorAll('svg').forEach(svg=>window.updateAvatar(svg,state)),state);
    assert((await motion()).every(x=>x.name==='none'),`working motion stops for ${state}`);
  }
  await page.evaluate(()=>document.querySelectorAll('svg').forEach(svg=>window.updateAvatar(svg,'running')));
  for(const pause of ['companion-system-paused','companion-motion-paused']) {
    await page.evaluate(pause=>document.body.classList.add(pause),pause);
    assert((await motion()).every(x=>x.name==='none'||x.state==='paused'),pause);
    await page.evaluate(pause=>document.body.classList.remove(pause),pause);
  }
  await page.evaluate(()=>document.querySelectorAll('svg').forEach(svg=>svg.classList.remove('companion-visible')));
  assert((await motion()).every(x=>x.state==='paused'),'offscreen pauses');
  await page.evaluate(()=>document.querySelectorAll('svg').forEach(svg=>svg.classList.add('companion-visible')));
  await page.emulateMedia({reducedMotion:'reduce'});
  assert((await motion()).every(x=>x.name==='none'),'reduced motion disables working animation');
  await page.emulateMedia({reducedMotion:'no-preference'});
  assert((await motion()).every(x=>x.state==='running'&&x.name!=='none'),'motion resumes');
  await fs.mkdir('artifacts/geometric-avatars',{recursive:true});
  await page.screenshot({path:'artifacts/geometric-avatars/gallery.png'});
  console.log('PASS: 10 identities, wraparound, live working motion, six other states, pause, visibility and reduced motion');
} finally {
  await browser.close();
  await server.close();
}
