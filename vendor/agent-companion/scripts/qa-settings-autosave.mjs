import assert from 'node:assert/strict';
import {preview} from 'vite';
import {chromium} from 'playwright';
import {defaultSettings} from '../src/settings-config.js';
const server=await preview({preview:{host:'127.0.0.1',port:0,strictPort:true}});
const browser=await chromium.launch({headless:true});
try {
  const page=await browser.newPage();
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  await page.route('**/qa-host.html',route=>route.fulfill({contentType:'text/html',body:'<html><body></body></html>'}));
  await page.goto(`${server.resolvedUrls.local[0]}qa-host.html`);
  await page.evaluate(settings=>{
    window.qa={settings,prefs:{avatarStyle:'animal',visibleCount:8,animation:true,autostart:false,autostartSupported:true,autostartManaged:true},calls:[],hold:false,fail:false};
    const sources=['codex','workbuddy','codebuddy-ide','codeg'].map(source=>({source,kind:source==='codeg'?'webhook':'hooks',status:'installed',message:'已配置',locations:[],automatic:true,lastEventAt:null}));
    window.__AGENT_STUDIO_EMBED_HOST__={desktop:true,listen:async()=>()=>{},enableNotifications:async()=>'granted',invoke:async(command,args)=>{
      const q=window.qa;q.calls.push({command,args});
      if(command.endsWith('|rail_settings_get'))return structuredClone(q.prefs);
      if(command.endsWith('|rail_settings_set')){
        if(q.hold)await new Promise(resolve=>q.release=resolve);
        if(q.fail)throw Error('preferences write failed');
        q.prefs={...args.preferences,autostart:args.autostart,autostartSupported:true,autostartManaged:true};return structuredClone(q.prefs);
      }
      if(command.endsWith('|collector_request')){
        if(args.command==='settings_get')return structuredClone(q.settings);
        if(args.command==='settings_set'){q.settings=args.payload;sources.find(s=>s.source==='codeg').status=q.settings.sources.codeg.enabled?'installed':'not_installed';return structuredClone(q.settings);}
        if(args.command==='integrations_get')return {sources:structuredClone(sources)};
        if(args.command==='integrations_set'){sources.find(s=>s.source===args.payload.source).status=args.payload.action==='install'?'installed':'not_installed';return {sources:structuredClone(sources)};}
      }
      return null;
    }};
    const frame=document.createElement('iframe');frame.id='settings';frame.src='/desktop-settings.html';frame.style='width:480px;height:900px';document.body.append(frame);
  },defaultSettings());
  const frame=page.frameLocator('#settings');
  await frame.locator('[data-field=autostart]').waitFor();
  await page.waitForFunction(()=>document.querySelector('iframe').contentDocument.querySelector('fieldset')?.disabled===false);
  assert.equal(await page.evaluate(()=>qa.calls.filter(c=>c.command.endsWith('|rail_settings_set')||c.args?.command==='settings_set').length),0,'startup never writes');
  await page.evaluate(()=>{qa.hold=true;qa.prefs.visibleCount=11;});
  await frame.locator('button[data-style=bot]').click();
  await page.waitForFunction(()=>!!qa.release);
  await frame.locator('button[data-style=animal]').click();
  await frame.locator('[data-field=animation]').click();
  assert.equal(await frame.locator('button[data-style=animal]').getAttribute('aria-pressed'),'true','latest choice visible while older response pending');
  await page.evaluate(()=>{qa.hold=false;qa.release();});
  await page.waitForFunction(()=>qa.prefs.avatarStyle==='animal'&&qa.prefs.animation===false);
  assert.equal(await page.evaluate(()=>qa.prefs.visibleCount),11,'fresh read preserves unrelated preference from another host');
  assert.equal(await frame.locator('button[data-style=animal]').getAttribute('aria-pressed'),'true','old response never replaces new choice');
  await frame.locator('[data-field=autostart]').click();
  await page.waitForFunction(()=>qa.prefs.autostart===true);
  await frame.locator('[data-field=source-codeg]').click();
  await frame.locator('[data-integration=codeg] .integration-badge').getByText('未接入',{exact:true}).waitFor();
  await frame.locator('[data-integration=codex] .integration-disclosure').click();
  await frame.getByRole('button',{name:'卸载',exact:true}).click();
  const dialog=frame.getByRole('alertdialog');
  await dialog.getByRole('button',{name:'取消'}).press('Shift+Tab');
  assert(await dialog.evaluate(el=>el.contains(el.ownerDocument.activeElement)),'focus stays trapped');
  await page.keyboard.press('Escape');await dialog.waitFor({state:'hidden'});
  assert.equal(await page.evaluate(()=>qa.calls.filter(c=>c.command.endsWith('|close_settings')).length),0,'dialog Escape never closes native settings');
  await page.evaluate(()=>qa.fail=true);
  await frame.locator('button[data-style=bot]').click();
  await frame.locator('[data-action=retry-save]').waitFor();
  assert.equal(await frame.locator('button[data-style=bot]').getAttribute('aria-pressed'),'true','failed selection is retained');
  assert.equal(await page.evaluate(()=>qa.prefs.avatarStyle),'animal');
  await page.evaluate(()=>qa.fail=false);
  await frame.locator('[data-action=retry-save]').click();
  await page.waitForFunction(()=>qa.prefs.avatarStyle==='bot');
  await page.evaluate(()=>document.querySelector('iframe').contentWindow.location.reload());
  await frame.locator('button[data-style=bot][aria-pressed=true]').waitFor();
  assert.equal(await frame.locator('[data-field=autostart]').getAttribute('aria-checked'),'true');
  assert.deepEqual(errors,[]);
  console.log('PASS: native bridge startup writes zero; rapid async edits; fresh merge; autostart persistence; Codeg refresh; dialog focus/Escape; save failure and retry; reload');
} finally {await pageRelease();await browser.close();await server.httpServer.close();}
async function pageRelease(){for(const context of browser.contexts())for(const page of context.pages())await page.evaluate(()=>window.qa?.release?.()).catch(()=>{});}
