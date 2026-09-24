import test from 'node:test';
import assert from 'node:assert/strict';
import { requestCustomIntegrations, requestCustomPreview, decodeCustomIntegrations } from '../src/desktop/custom-integrations.ts';

const status = {
  version: 1,
  storage: {ok: true, error: null},
  binaryInstalled: true,
  templates: [{
    id: 'example-agent', source: 'custom:example-agent', name: 'Example Agent', enabled: true, importedAt: 1,
    capabilities: ['start'], events: [{event: 'prompt_submitted', action: 'start'}],
    lastReceivedAt: null, lastMappedAt: null, command: "'/tmp/bin' custom-hook --home '/tmp/home' --integration example-agent",
  }],
  diagnostics: [{at: 1, source: 'custom:example-agent', event: 'prompt_submitted', outcome: 'accepted', reason: 'start', detail: ''}],
};

function host(responder) {
  const calls = [];
  globalThis.window = {parent:{__AGENT_STUDIO_EMBED_HOST__:{desktop:true,invoke:async (command,args) => {
    calls.push({command,args});
    return responder(args);
  }}}};
  return calls;
}

test('custom integration actions use native RPC and preserve native errors', async t => {
  let failure = false;
  const calls = host(() => {
    if (failure) throw '已存在 ID 为 example-agent 的自定义来源，请先删除后再导入';
    return status;
  });
  t.after(() => { delete globalThis.window; });
  assert.deepEqual(await requestCustomIntegrations(), status);
  assert.deepEqual(await requestCustomIntegrations({action: 'disable', id: 'example-agent'}), status);
  assert.deepEqual(await requestCustomIntegrations({action: 'import', template: {schemaVersion: 1}}), status);
  assert.deepEqual(calls, [
    {command:'plugin:agent-studio|collector_request',args:{command:'custom_integrations_get',payload:null}},
    {command:'plugin:agent-studio|collector_request',args:{command:'custom_integrations_set',payload:{action:'disable',id:'example-agent'}}},
    {command:'plugin:agent-studio|collector_request',args:{command:'custom_integrations_set',payload:{action:'import',template:{schemaVersion:1}}}},
  ]);
  failure = true;
  await assert.rejects(requestCustomIntegrations({action: 'import', template: {}}), /请先删除/);
});

test('preview is a separate pure command', async t => {
  const calls = host(() => ({ok: false, outcome: 'rejected', reason: 'missing_session_id', path: '/mapping/sessionId', error: '载荷中缺少会话 ID', events: []}));
  t.after(() => { delete globalThis.window; });
  const result = await requestCustomPreview({schemaVersion: 1}, {event_name: 'x'});
  assert.equal(result.ok, false);
  assert.equal(result.path, '/mapping/sessionId');
  assert.deepEqual(calls, [{
    command:'plugin:agent-studio|collector_request',
    args:{command:'custom_preview',payload:{template:{schemaVersion:1},payload:{event_name:'x'}}},
  }]);
});

test('malformed custom integration status is rejected before rendering', () => {
  assert.throws(() => decodeCustomIntegrations({version: 1, storage: {ok: true, error: null}, binaryInstalled: true, templates: [{id: 'x', source: 'codex'}], diagnostics: []}), /自定义接入响应无效/);
  assert.throws(() => decodeCustomIntegrations({...status, storage: {ok: 'yes'}}), /自定义接入响应无效/);
  assert.throws(() => decodeCustomIntegrations({...status, templates: [status.templates[0], {...status.templates[0]}]}), /自定义接入响应无效/);
  assert.throws(() => decodeCustomIntegrations({...status, diagnostics: [{at: 1, source: 'custom:x', outcome: 'unknown', reason: 'r', detail: ''}]}), /自定义接入响应无效/);
  assert.deepEqual(decodeCustomIntegrations(status).templates[0].id, 'example-agent');
});
