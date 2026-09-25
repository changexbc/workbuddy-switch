import test from 'node:test';
import assert from 'node:assert/strict';
import { idleUpdateSnapshot, isUpdateConfig, isUpdateSnapshot } from '../src/types/update.ts';
import { updateAction, updateProxyInvalidMessage, updateStatusText, updatesSupported, validateProxyInput, watchUpdateState } from '../src/desktop/update.ts';

// The snapshots below are the payloads `src-tauri/src/update_service.rs` emits and
// returns; the Rust side asserts the same field names in `snapshot_serializes_camel_case_contract`.
const snapshot = extra => ({phase: 'idle', latest: null, percent: null, message: null, checkedAt: null, ...extra});

test('update snapshot guard accepts native payloads and rejects malformed ones', () => {
  assert.deepEqual(idleUpdateSnapshot(), snapshot({}));
  assert.ok(isUpdateSnapshot(snapshot({})));
  assert.ok(isUpdateSnapshot(snapshot({phase: 'available', latest: '0.2.0', checkedAt: 1_700_000_000})));
  assert.ok(isUpdateSnapshot(snapshot({phase: 'downloading', percent: 42})));
  assert.ok(isUpdateSnapshot(snapshot({phase: 'readyToRestart', latest: '0.2.0', percent: 100})));
  assert.ok(isUpdateSnapshot(snapshot({phase: 'error', latest: '0.2.0', message: '下载更新包失败：连接超时'})));

  for (const invalid of [null, undefined, 'idle', 42, {}, snapshot({phase: 'unknownPhase'}), snapshot({latest: 7}), snapshot({percent: 101}), snapshot({percent: 1.5}), snapshot({checkedAt: 'now'})]) {
    assert.equal(isUpdateSnapshot(invalid), false, `${JSON.stringify(invalid)} 不是合法快照`);
  }
});

test('update config guard accepts the native config and rejects partial payloads', () => {
  const config = {proxy: '', currentVersion: '0.1.0', releaseUrl: 'https://github.com/changexbc/agent-companion/releases/latest'};
  assert.ok(isUpdateConfig(config));
  assert.ok(isUpdateConfig({...config, proxy: 'http://127.0.0.1:7897'}));
  for (const invalid of [null, {}, {proxy: ''}, {...config, currentVersion: 1}, {...config, releaseUrl: null}]) {
    assert.equal(isUpdateConfig(invalid), false, `${JSON.stringify(invalid)} 不是合法配置`);
  }
});

test('proxy input only accepts http and https and mirrors the native message', () => {
  assert.deepEqual(validateProxyInput(''), {ok: true, value: ''});
  assert.deepEqual(validateProxyInput('   '), {ok: true, value: ''});
  assert.deepEqual(validateProxyInput('  http://127.0.0.1:7897 '), {ok: true, value: 'http://127.0.0.1:7897'});
  assert.deepEqual(validateProxyInput('https://proxy.example.com:8080'), {ok: true, value: 'https://proxy.example.com:8080'});
  assert.deepEqual(validateProxyInput('http://user:secret@127.0.0.1:7897'), {ok: true, value: 'http://user:secret@127.0.0.1:7897'});

  for (const invalid of ['127.0.0.1:7897', 'socks5://127.0.0.1:1080', 'ftp://127.0.0.1', 'http://', 'not a url']) {
    assert.deepEqual(validateProxyInput(invalid), {ok: false, message: updateProxyInvalidMessage}, `${invalid} 应被拒绝`);
  }
});

test('one action decides the tray item and the settings button', () => {
  assert.equal(updateAction(idleUpdateSnapshot()), 'check');
  assert.equal(updateAction(snapshot({phase: 'checking'})), 'check');
  assert.equal(updateAction(snapshot({phase: 'upToDate'})), 'check');
  assert.equal(updateAction(snapshot({phase: 'available', latest: '0.2.0'})), 'download');
  assert.equal(updateAction(snapshot({phase: 'downloading', latest: '0.2.0', percent: 10})), 'check');
  assert.equal(updateAction(snapshot({phase: 'readyToRestart', latest: '0.2.0'})), 'restart');
  assert.equal(updateAction(snapshot({phase: 'error', latest: '0.2.0'})), 'download');
  assert.equal(updateAction(snapshot({phase: 'error', message: '检查更新失败：连接超时'})), 'check');
});

test('status text covers every phase and keeps the native failure message', () => {
  assert.equal(updateStatusText(idleUpdateSnapshot(), '0.1.0'), '尚未检查更新');
  assert.equal(updateStatusText(snapshot({phase: 'checking'}), '0.1.0'), '正在检查更新…');
  assert.equal(updateStatusText(snapshot({phase: 'upToDate'}), '0.1.0'), '已是最新版本 v0.1.0');
  assert.equal(updateStatusText(snapshot({phase: 'available', latest: '0.2.0'}), '0.1.0'), '发现新版本 v0.2.0（当前 v0.1.0）');
  // 复查失败：native 快照保留目标版本，同时带上失败文案，两者都要显示。
  assert.equal(
    updateStatusText(snapshot({phase: 'available', latest: '0.2.0', message: '检查更新失败：网络请求失败'}), '0.1.0'),
    '发现新版本 v0.2.0（当前 v0.1.0）；检查更新失败：网络请求失败',
  );
  assert.equal(updateStatusText(snapshot({phase: 'downloading', latest: '0.2.0', percent: null}), '0.1.0'), '正在下载 v0.2.0…');
  assert.equal(updateStatusText(snapshot({phase: 'downloading', latest: '0.2.0', percent: 42}), '0.1.0'), '正在下载 v0.2.0 42%');
  assert.equal(updateStatusText(snapshot({phase: 'readyToRestart', latest: '0.2.0'}), '0.1.0'), 'v0.2.0 已下载，重启后完成安装');
  assert.equal(updateStatusText(snapshot({phase: 'error', message: '下载更新包失败：连接超时'}), '0.1.0'), '下载更新包失败：连接超时');
  assert.equal(updateStatusText(snapshot({phase: 'error'}), '0.1.0'), '更新失败，请重试');
});

test('updates stay unsupported without a standalone Tauri host', () => {
  // Node has neither `__TAURI_INTERNALS__` nor an embed host, which is the same
  // surface the browser build sees: no commands, no subscription, no section.
  assert.equal(updatesSupported(), false);
  let called = false;
  const release = watchUpdateState(() => { called = true; });
  assert.equal(typeof release, 'function');
  release();
  assert.equal(called, false);
});
