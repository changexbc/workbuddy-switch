import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import { createRailModel } from '../src/desktop/rail-model.js';
import { sessionPresentation } from '../src/monitor/presentation.js';
import { providerLabel } from '../src/desktop/components/provider.tsx';
import { railProjectLabel } from '../src/desktop/project-label.ts';
import { createNotificationTracker } from '../src/monitor/model.js';
import { snapshotKey } from '../collector/desktop.js';
import { defaultSettings } from '../src/settings-config.js';
import { CodexLivePoller } from '../collector/lib/codex-live.js';
import { Hub, STALE_MS } from '../collector/lib/hub.js';

const session = (n, status = 'running', extra = {}) => ({ id: `codex:${n}`, source: 'codex', sessionId: String(n), status, title: `任务 ${n}`, pending: [], steps: [], roundId: 'r1', updatedAt: 1000, ...extra });
const snapshot = sessions => ({ version: 1, ts: 1000, ready: true, sources: { codex: { state: 'ok', checkedAt: 1000 } }, sessions, events: [] });
test('generated Codeg and WorkBuddy directory names get readable rail labels', () => {
  assert.equal(railProjectLabel({source:'codeg',project:'79f660f8af4149aa97b1f60a22be581e'},'Codeg'),'聊天');
  assert.equal(railProjectLabel({source:'workbuddy',project:'2026-09-24-17-24-22'},'WorkBuddy'),'任务 · 09/24 17:24');
  assert.equal(railProjectLabel({source:'codeg',project:'my-project'},'Codeg'),'my-project');
  assert.equal(railProjectLabel({source:'workbuddy',project:'my-project'},'WorkBuddy'),'my-project');
  assert.equal(railProjectLabel({source:'codex',project:'2026-09-24-17-24-22'},'Codex'),'2026-09-24-17-24-22');
  assert.equal(railProjectLabel({source:'workbuddy',project:'2026-13-24-17-24-22'},'WorkBuddy'),'2026-13-24-17-24-22');
  assert.equal(railProjectLabel({source:'codeg',project:'codeg'},'Codeg'),'Codeg');
});
test('desktop restart ignores existing Codex checkpoint and transcript', async t => {
 const home=await fs.mkdtemp(path.join(os.tmpdir(),'agent-studio-hook-only-'));
 t.after(()=>fs.rm(home,{recursive:true,force:true}));
 await fs.mkdir(path.join(home,'.agent-studio'),{recursive:true});
 const file=path.join(home,'.agent-studio/codex-resume.json');
 const old=JSON.stringify({sessions:[session(7)]});await fs.writeFile(file,old);
 const hub=new Hub(),poller=new CodexLivePoller(hub,{home});await poller.poll();
 assert.equal(hub.sessions.size,0);assert.equal(poller.live.size,0);
 assert.equal(await fs.readFile(file,'utf8'),old);
});
function connected(options) { const model = createRailModel(options); model.connect('connected'); return model; }

test('overflow retires oldest done sessions every ten seconds until eight remain', () => {
  let time = 5000; const model = connected({ now: () => time });
  const active = Array.from({ length: 10 }, (_, n) => session(n + 1));
  model.accept(snapshot(active));
  const completed = active.map((s, n) => n < 3 ? { ...s, status: 'done', endedAt: 3000 - n * 1000 } : s);
  model.accept(snapshot(completed));
  assert.equal(model.nextExpiry, 15000);
  time = 14999; model.accept(snapshot(completed)); assert.equal(model.items.length, 10);
  assert.equal(model.nextExpiry, 15000);
  time = 15000; model.refresh();
  assert.equal(model.items.length, 9); assert.ok(!model.items.some(s => s.id === 'codex:3'));
  assert.equal(model.nextExpiry, 25000);
  time = 25000; model.refresh();
  assert.equal(model.items.length, 8); assert.ok(!model.items.some(s => s.id === 'codex:2'));
  assert.equal(model.nextExpiry, 3000 + 60 * 60_000);
  model.accept(snapshot(completed)); assert.equal(model.items.length, 8);
  model.accept(snapshot(completed.map(s => s.id === 'codex:3' ? { ...s, status: 'running', roundId: 'r2' } : s)));
  assert.ok(model.items.some(s => s.id === 'codex:3'));
});

test('overflow cancels on disconnect, new turns and a drop to eight; other states survive', () => {
  let time = 5000; const model = connected({ now: () => time });
  const active = Array.from({ length: 9 }, (_, n) => session(n + 1));
  model.accept(snapshot(active));
  const completed = active.map((s, n) => n === 0 ? { ...s, status: 'done', endedAt: 5000 } : s);
  model.accept(snapshot(completed));
  model.connect('offline'); assert.equal(model.nextExpiry, null);
  time = 20000; model.connect('connected'); assert.equal(model.nextExpiry, 30000);
  model.accept(snapshot(completed.slice(0, 8)));
  assert.equal(model.nextExpiry, 5000 + 60 * 60_000);
  model.accept(snapshot(completed)); assert.equal(model.nextExpiry, 30000);
  model.accept(snapshot(active.map((s, n) => n === 0 ? { ...s, roundId: 'r2' } : s)));
  time = 40000; model.refresh(); assert.equal(model.items.length, 9); assert.equal(model.nextExpiry, null);
  model.accept(snapshot(active.map((s, n) => ({ ...s, status: ['running', 'wait', 'error', 'aborted'][n % 4], endedAt: time }))));
  time += 10000; model.refresh(); assert.equal(model.items.length, 9);
});

test('desktop rail follows actual sessions, preserves identity/order, and has no eight-seat cap', () => {
  const model = connected({ now: () => 2000 });
  model.accept(snapshot([session(1), session(2), session(3), session(99, 'done')]));
  assert.equal(model.items.length, 3);
  const old = model.items.map(row => [row.id, row.identity.id]);
  model.accept(snapshot([session(5), session(3), session(2), session(4), session(1)]));
  assert.deepEqual(model.items.slice(0, 3).map(row => [row.id, row.identity.id]), old);
  assert.equal(model.items.length, 5);
  model.accept(snapshot(Array.from({ length: 15 }, (_, n) => session(n + 1))));
  assert.equal(model.items.length, 15);
});
test('confirmed monitor close forgets only the matching row and a later Hook can show it again', () => {
  const model = connected({now: () => 2000});
  model.accept(snapshot([session(1), session(2)]));
  model.forgetMonitoring('codex:1', 'older-round');
  assert.equal(model.items.length, 2);
  model.forgetMonitoring('codex:1', 'r1');
  assert.deepEqual(model.items.map(item => item.id), ['codex:2']);
  model.accept(snapshot([session(1), session(2)]));
  assert.deepEqual(model.items.map(item => item.id), ['codex:2'], 'a delayed pre-close snapshot stays suppressed');
  model.accept({...snapshot([session(1), session(2)]), ts: 2001});
  assert.deepEqual(model.items.map(item => item.id), ['codex:2', 'codex:1'], 'a newer same-round Hook can return');
  model.forgetMonitoring('codex:1', 'r1');
  model.accept({...snapshot([session(2)]), ts: 2002});
  model.accept({...snapshot([session(1, 'running', {roundId: 'r2'}), session(2)]), ts: 2003});
  assert.deepEqual(model.items.map(item => item.id), ['codex:2', 'codex:1']);
});
test('confirmed close blocks a delayed old snapshot after the removal snapshot arrived first', () => {
  const model = connected({now: () => 2000});
  model.accept(snapshot([session(1), session(2)]));
  model.accept({...snapshot([session(2)]), ts: 1999});
  assert.deepEqual(model.items.map(item => item.id), ['codex:2']);
  model.forgetMonitoring('codex:1', 'r1');
  model.accept(snapshot([session(1), session(2)]));
  assert.deepEqual(model.items.map(item => item.id), ['codex:2']);
  model.accept({...snapshot([session(1, 'running', {roundId: 'r2'}), session(2)]), ts: 2001});
  assert.deepEqual(model.items.map(item => item.id), ['codex:2', 'codex:1']);
});
test('recovered Codex completions appear once, expire, and respect dismissal across rail restarts', () => {
  let time = 2000;
  const values = new Map();
  const storage = {getItem: key => values.get(key), setItem: (key, value) => values.set(key, value)};
  const complete = session(1, 'done', {recovered: true, endedAt: 1500, updatedAt: 1500});
  let model = connected({now: () => time, holdMs: 1000, storage});
  model.accept(snapshot([complete]));
  assert.equal(model.items.length, 1);
  model.dismiss('codex:1', 'r1');
  model = connected({now: () => time, holdMs: 1000, storage});
  model.accept(snapshot([complete]));
  assert.equal(model.items.length, 0);
  model.accept(snapshot([session(1, 'running', {roundId: 'r2', updatedAt: 2100})]));
  assert.equal(model.items.length, 1);
  model = connected({now: () => time, holdMs: 1000, storage});
  model.accept(snapshot([{...complete, roundId: 'r2', endedAt: 1900, updatedAt: 1900}]));
  assert.equal(model.items.length, 1);
  model = connected({now: () => time, holdMs: 1000, storage});
  model.accept(snapshot([{...complete, viewedRoundId: 'r1'}]));
  assert.equal(model.items.length, 0);
  model.accept(snapshot([complete]));
  assert.equal(model.items.length, 0);
  time = 2500;
  model = connected({now: () => time, holdMs: 1000, storage});
  model.accept(snapshot([{...complete, roundId: 'r3', endedAt: 1500}]));
  assert.equal(model.items.length, 0);
  model.accept(snapshot([session(2, 'done', {endedAt: 2400})]));
  assert.equal(model.items.length, 0);
});
test('unknown and disconnected sessions are retained; disabling a source clears its avatars', () => {
  const model = connected(); model.accept(snapshot([session(1)]));
  model.accept(snapshot([session(1, 'unknown')])); assert.equal(model.items.length, 1);
  model.connect('offline'); model.accept({ ...snapshot([]), sources: { codex: { state: 'error' } } });
  assert.equal(model.items.length, 1); assert.equal(model.items[0].offline, true);
  model.accept({ ...snapshot([]), sources: { codex: { state: 'disabled' } } }); assert.equal(model.items.length, 0);
});
test('only observed completions linger, then expire; a resumed session restores its avatar', () => {
  let now = 2000; const model = connected({ now: () => now, holdMs: 100 });
  model.accept(snapshot([session(1)])); const identity = model.items[0].identity;
  model.accept(snapshot([session(1, 'done', { endedAt: 2000 })])); assert.equal(model.nextExpiry, 2100);
  now = 2101; model.refresh(); assert.equal(model.items.length, 0);
  model.accept(snapshot([session(1, 'running', { roundId: 'r2' })])); assert.deepEqual(model.items[0].identity, identity);
  model.dismiss('codex:1'); assert.equal(model.items.length, 1);
  model.accept(snapshot([session(1, 'done', { endedAt: now })])); model.dismiss('codex:1'); assert.equal(model.items.length, 0);
  model.accept(snapshot([session(1, 'running', { roundId: 'r3' })])); assert.equal(model.items.length, 1);
});
test('a disconnected completion cannot expire before the source reconnects', () => {
  let now = 2000; const model = connected({ now: () => now, holdMs: 100 });
  model.accept(snapshot([session(1)])); model.accept(snapshot([session(1, 'done', { endedAt: now })])); model.connect('offline');
  now += 200; model.refresh(); assert.equal(model.items.length, 1); assert.equal(model.nextExpiry, null);
  model.connect('connected'); assert.equal(model.items.length, 0);
});
test('completed avatars remain one hour by default and opening an old round cannot dismiss a new round', () => {
  let now=2000;const model=connected({now:()=>now});
  model.accept(snapshot([session(1)]));
  model.accept(snapshot([session(1,'done',{endedAt:now})]));
  assert.equal(model.nextExpiry,2000+60*60_000);
  now+=60*60_000-1;model.refresh();assert.equal(model.items.length,1);
  now++;model.refresh();assert.equal(model.items.length,0);
  model.accept(snapshot([session(1,'running',{roundId:'r2'})]));
  model.accept(snapshot([session(1,'done',{roundId:'r2',endedAt:now})]));
  model.dismiss('codex:1','r1');assert.equal(model.items.length,1);
  model.dismiss('codex:1','r2');assert.equal(model.items.length,0);
  model.accept(snapshot([session(1,'done',{roundId:'r2',endedAt:now})]));assert.equal(model.items.length,0);
});
test('shared card presentation preserves source-specific navigation and wait preview', () => {
  assert.equal(sessionPresentation(session(1)).action, '进入对话');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'codeg' })).action, '进入对话');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'codebuddy-ide', cwd: '/tmp/demo' })).action, '打开工程');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'codebuddy-ide', cwd: '/tmp/demo' })).url, 'codebuddy://file/tmp/demo');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'codebuddy-ide', agentType: 'codebuddy', cwd: '/tmp/demo' })).provider, 'CodeBuddy 国际版');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'codebuddy-ide', agentType: 'codebuddycn', cwd: '/tmp/demo' })).url, 'codebuddycn://file/tmp/demo');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'codebuddy-ide', agentType: 'codebuddycn' })).provider, 'CodeBuddy 国内版');
  const ask = session(1, 'wait', { pending: [{ text: '需要确认', questions: [{ text: '选择方案 A 或 B？' }] }] });
  assert.equal(sessionPresentation(ask).question, '选择方案 A 或 B？');
  assert.equal(sessionPresentation(ask, 'offline').status, 'offline');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'workbuddy' })).provider, 'WorkBuddy');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'workbuddy' })).url, 'workbuddy://chat/1');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'workbuddy', agentType: 'workbuddy-ai' })).provider, 'WorkBuddy 国际版');
  assert.equal(sessionPresentation(session(1, 'running', { source: 'workbuddy', agentType: 'workbuddy-ai' })).url, 'workbuddy-ai://chat/1');
  // A delegated Codeg child carries the 子任务 badge with its parent as the detail
  // the card shows as the provider tooltip, and links to its own conversation.
  const child = session(1, 'wait', { source: 'codeg', sessionId: '215', agentType: 'code_buddy', subagent: true, parentTitle: 'Build feature', pending: [{ id: 'p1', text: 'Allow shell?', questions: [{ text: 'Allow shell?' }] }] });
  const childCard = sessionPresentation(child);
  assert.deepEqual(childCard.badge, { host: 'codeg', id: 'codebuddy-ide', label: '子任务', detail: '父会话：Build feature' });
  assert.equal(childCard.provider, 'Codeg');
  assert.equal(childCard.url, 'codeg://session/215');
  assert.equal(childCard.question, 'Allow shell?');
  assert.equal(providerLabel({ session: child }, childCard), 'Codeg · 子任务');
});
test('the CodeBuddy VS Code plugin is labelled as a VS Code host and opens VS Code', () => {
  const vscode = session(1, 'running', { source: 'codebuddy-ide', agentType: 'codebuddy', hostKind: 'vscode', cwd: '/tmp/demo' });
  const withFolder = sessionPresentation(vscode);
  assert.equal(withFolder.provider, 'CodeBuddy 国际版');
  assert.deepEqual(withFolder.badge, { host: 'codebuddy-ide', id: 'codebuddy-vscode', label: 'VS Code', avatar: 'codebuddy-vscode' });
  assert.equal(withFolder.url, '/api/open-session?source=codebuddy-ide&host=vscode&session=1&cwd=%2Ftmp%2Fdemo');
  assert.equal(withFolder.action, '打开工程');
  assert.equal(providerLabel({ session: vscode }, withFolder), 'CodeBuddy 国际版 · VS Code');
  const withoutFolder = sessionPresentation({ ...vscode, cwd: '/' });
  assert.equal(withoutFolder.url, '/api/open-session?source=codebuddy-ide&host=vscode&session=1');
  assert.equal(withoutFolder.action, '打开 VS Code');
  const ide = session(1, 'running', { source: 'codebuddy-ide', agentType: 'codebuddy', cwd: '/tmp/demo' });
  assert.equal(sessionPresentation(ide).action, '打开工程');
  assert.equal(providerLabel({ session: ide }, sessionPresentation(ide)), 'CodeBuddy 国际版');
});

test('a confirmed Codex read transition hides only that completed round', () => {
  const model=connected({now:()=>2000});model.accept(snapshot([session(1)]));
  model.accept(snapshot([session(1,'done',{endedAt:2000,viewedRoundId:'other'})]));assert.equal(model.items.length,1);
  model.accept(snapshot([session(1,'done',{endedAt:2000,viewedRoundId:'r1'})]));assert.equal(model.items.length,0);
  model.accept(snapshot([session(1,'running',{roundId:'r2',viewedRoundId:'r1'})]));assert.equal(model.items.length,1);
});
test('unchanged snapshots neither dirty desktop state nor rewrite notification storage', () => {
  const a = snapshot([session(1, 'running', { elapsed: 1 })]);
  const b = { ...a, ts: 5000, sources: { codex: { state: 'ok', checkedAt: 5000 } }, sessions: [{ ...a.sessions[0], elapsed: 5 }] };
  assert.equal(snapshotKey(a), snapshotKey(b));
  let writes = 0; const tracker = createNotificationTracker({ storage: { getItem() {}, setItem() { writes++; } } });
  const done = session(1, 'done'); const value = { ...snapshot([done]), events: [{ id: JSON.stringify([done.id, 'r1', 'done', 'r1']), sessionId: done.id, kind: 'done', roundId: 'r1' }] };
  tracker.ingest(value); const initial = writes;
  for (let i = 0; i < 100; i++) tracker.ingest(value);
  assert.equal(writes, initial);
});
test('desktop helper owns hooks, pushes changed state, notifies without a UI, and exits with its host', async t => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), 'agent-studio-desktop-'));
  t.after(() => fs.rm(home, { recursive: true, force: true }));
  const settings = defaultSettings(); for (const id of Object.keys(settings.sources)) settings.sources[id].enabled = id === 'codex'; settings.notifications.desktop = true;
  await fs.mkdir(path.join(home, '.agent-studio')); await fs.writeFile(path.join(home, '.agent-studio/settings.json'), JSON.stringify(settings));
  const child = spawn(process.execPath, ['collector/desktop.js'], { env: { ...process.env, AGENT_STUDIO_HOME: home, NODE_NO_WARNINGS: '1' }, stdio: ['pipe', 'pipe', 'pipe'] });
  t.after(() => { if (child.exitCode === null) child.kill(); });
  const messages = [], lines = readline.createInterface({ input: child.stdout }); lines.on('line', text => { messages.push(JSON.parse(text)); });
  let stderr = ''; child.stderr.on('data', value => { stderr += value; });
  const wait = async predicate => { const start = Date.now(); while (Date.now() - start < 7000) { const found = messages.find(predicate); if (found) return found; if (child.exitCode !== null) throw Error(stderr || 'Helper exited'); await new Promise(resolve => setTimeout(resolve, 30)); } throw Error(`Timed out waiting for helper: ${stderr}`); };
  const ready = await wait(m => m.type === 'ready');
  assert.equal(await fs.readFile(path.join(home, '.codex/hooks/astra-office-monitor.url'), 'utf8'), ready.url);
  const hook = async payload => { const r = await fetch(ready.url + '/api/codex-hook', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(payload) }); assert.equal(r.status, 204); };
  await hook({ session_id: 'desktop-test', hook_event_name: 'UserPromptSubmit', prompt: '桌面采集测试', timestamp: Date.now() });
  await wait(m => m.type === 'state' && m.snapshot.sessions.some(s => s.sessionId === 'desktop-test' && s.status === 'running'));
  await hook({ session_id: 'desktop-test', hook_event_name: 'PermissionRequest', tool_use_id: 'permission-test', timestamp: Date.now() });
  await wait(m => m.type === 'state' && m.snapshot.sessions.some(s => s.permissionChecks?.length));
  assert.equal(messages.some(m=>m.type==='notification' && m.alert.kind==='wait'),false);
  await hook({session_id:'desktop-test',hook_event_name:'PreToolUse',tool_name:'request_user_input',tool_use_id:'question-test',timestamp:Date.now()});
  await wait(m => m.type === 'notification' && m.alert.kind === 'wait');
  child.stdin.write(JSON.stringify({ id: 7, command: 'settings_get' }) + '\n');
  assert.equal((await wait(m => m.type === 'reply' && m.id === 7)).value.version, 1);
  const count = messages.filter(m => m.type === 'state').length;
  await new Promise(resolve => setTimeout(resolve, 2200));
  assert.equal(messages.filter(m => m.type === 'state').length, count);
  const exited = new Promise(resolve => child.once('exit', resolve)); child.stdin.end();
  assert.equal(await Promise.race([exited, new Promise((_, reject) => setTimeout(() => reject(Error('Orphaned helper')), 5000).unref())]), 0);
  const persisted = JSON.parse(await fs.readFile(path.join(home, '.agent-studio/desktop-notifications.json'), 'utf8'));
  assert.equal(persisted.alerts.length, 1);
});

test('opened completed rail session survives viewed updates for ten seconds, repeated clicks do not extend it', () => {
 let time=2000; const model=connected({now:()=>time});
 model.accept(snapshot([session(1)])); model.accept(snapshot([session(1,'done')]));
 model.retainOpened('codex:1','r1');
 model.accept(snapshot([session(1,'done',{viewedRoundId:'r1'})]));
 assert.equal(model.items.length,1); assert.equal(model.nextExpiry,12000);
 time=9000; model.retainOpened('codex:1','r1'); assert.equal(model.nextExpiry,12000);
 time=11999; model.refresh(); assert.equal(model.items.length,1);
 time=12000; model.refresh(); assert.equal(model.items.length,0);
});
test('clicked aborted round expires offline, while hover entry resets only its live deadline', () => {
 let time=2000; const model=connected({now:()=>time});
 model.accept(snapshot([session(1),session(2)]));
 model.accept(snapshot([session(1,'aborted',{endedAt:time}),session(2,'done',{endedAt:time})]));
 assert.equal(model.items[0].openedUntil,undefined);
 assert.equal(model.resetOpened('codex:1','r1'),false,'ordinary hover has no countdown');
 model.retainOpened('codex:1','r1');
 assert.equal(model.nextExpiry,12000);
 time=5000; assert.equal(model.resetOpened('codex:1','r1'),true);
 assert.equal(model.items[0].openedUntil,15000);
 assert.equal(model.resetOpened('codex:2','r1'),false,'other row is unaffected');
 model.connect('offline');
 assert.equal(model.nextExpiry,15000,'the clicked deadline stays scheduled offline');
 time=15001; assert.equal(model.resetOpened('codex:1','r1'),false,'an expired row cannot restart');
 model.refresh();
 assert.deepEqual(model.items.map(item=>item.id),['codex:2']);
 model.connect('connected');
 assert.deepEqual(model.items.map(item=>item.id),['codex:2'],'reconnection cannot resurrect the expired round');
 model.accept(snapshot([session(1,'running',{roundId:'r2'}),session(2,'done',{endedAt:2000})]));
 assert.deepEqual(model.items.map(item=>item.id),['codex:2','codex:1'],'a new round remains available');
});
test('clicked terminal deadline remains authoritative when its host exits', () => {
 let time=2000; const model=connected({now:()=>time});
 model.accept(snapshot([session(1)]));
 model.accept(snapshot([session(1,'aborted',{endedAt:time})]));
 model.retainOpened('codex:1','r1');
 model.accept({...snapshot([]),sources:{codex:{state:'exited'}}});
 assert.equal(model.nextExpiry,12000);
 time=9000;model.refresh();assert.equal(model.items.length,1);
 time=12000;model.refresh();assert.equal(model.items.length,0);
});
test('a new active round cancels pending opened-session removal', () => {
 let time=2000; const model=connected({now:()=>time});
 model.accept(snapshot([session(1)])); model.accept(snapshot([session(1,'done')]));
 model.retainOpened('codex:1','r1');
 model.accept(snapshot([session(1,'running',{roundId:'r2'})]));
 time=15000; model.refresh(); assert.equal(model.items.length,1); assert.equal(model.nextExpiry,null);
});

test('low-poly rail repairs cached collisions and keeps all 13 identities distinct', async () => {
  const { PORTRAITS, PORTRAIT_STORAGE_KEY } = await import('../src/desktop/rail-model.js');
  const values = new Map([[PORTRAIT_STORAGE_KEY, JSON.stringify([['codex:1', 0], ['codex:2', 0]])]]);
  const storage = { getItem: key => values.get(key), setItem: (key, value) => values.set(key, value) };
  const model = connected({ storage });
  model.accept(snapshot(Array.from({ length: PORTRAITS.length }, (_, i) => session(i + 1))));
  assert.equal(new Set(model.items.map(row => row.identity.id)).size, PORTRAITS.length);
  const first = model.items.map(row => row.identity);
  const restarted = connected({ storage });
  restarted.accept(snapshot(Array.from({ length: PORTRAITS.length }, (_, i) => session(i + 1))));
  assert.deepEqual(restarted.items.map(row => row.identity), first);
  model.accept(snapshot(Array.from({ length: 30 }, (_, i) => session(i + 1))));
  assert.equal(new Set(model.items.map(row => row.identity.slot)).size, 30);
  assert.equal(new Set(model.items.map(row => row.identity.name)).size, 30);
  assert.deepEqual(model.items.slice(0, PORTRAITS.length).map(row => row.identity), first);
});

test('rail frees disappeared identities before admitting replacement sessions', () => {
  const model = connected();
  model.accept(snapshot([session(1)])); const initial = model.items[0].identity;
  model.accept(snapshot([session(2)]));
  assert.equal(model.items[0].identity.slot, initial.slot);
  model.accept(snapshot([session(1), session(2)]));
  assert.equal(new Set(model.items.map(row => row.identity.id)).size, 2);
  assert.equal(model.items.find(row => row.id === 'codex:2').identity.slot, initial.slot);
});

test('desktop ignores the old eight-character cache after switching portrait sets', () => {
  const storage = { getItem: key => key.endsWith('.v1') ? JSON.stringify([['codex:1', 7]]) : null, setItem() {} };
  const model = connected({ storage }); model.accept(snapshot([session(1)]));
  assert.equal(model.items[0].identity.id, 'red-panda');
  assert.equal(model.items[0].identity.number, 1);
});
