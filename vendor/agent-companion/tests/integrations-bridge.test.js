import test from 'node:test';
import assert from 'node:assert/strict';
import { openIntegrationFolder, requestIntegrations } from '../src/desktop/integrations.ts';

test('integration actions use native RPC and preserve native errors', async t => {
  const calls = [];
  let failure = false;
  const response = {sources:[]};
  globalThis.window = {parent:{__AGENT_STUDIO_EMBED_HOST__:{desktop:true,invoke:async (command,args) => {
    calls.push({command,args});
    if (failure) throw '配置不可写';
    return response;
  }}}};
  t.after(() => { delete globalThis.window; });
  assert.deepEqual(await requestIntegrations(),response);
  assert.deepEqual(await requestIntegrations({source:'codex',action:'uninstall'}),response);
  assert.deepEqual(calls,[
    {command:'plugin:agent-studio|collector_request',args:{command:'integrations_get',payload:null}},
    {command:'plugin:agent-studio|collector_request',args:{command:'integrations_set',payload:{source:'codex',action:'uninstall'}}},
  ]);
  failure = true;
  await assert.rejects(requestIntegrations({source:'codeg',action:'install'}),/配置不可写/);
});

test('malformed integration status is rejected before rendering', async t => {
  globalThis.window = {parent:{__AGENT_STUDIO_EMBED_HOST__:{desktop:true,invoke:async () => ({sources:[{source:'codex',locations:null}]})}}};
  t.after(() => { delete globalThis.window; });
  await assert.rejects(requestIntegrations(),/接入状态响应无效/);
});

test('open folder uses the native command with its exact source and path', async t => {
  const calls = [];
  globalThis.window = {parent:{__AGENT_STUDIO_EMBED_HOST__:{desktop:true,invoke:async (command,args) => {
    calls.push({command,args});
  }}}};
  t.after(() => { delete globalThis.window; });
  await openIntegrationFolder('codex','/tmp/test-home/.codex/hooks.json');
  assert.deepEqual(calls,[{command:'plugin:agent-studio|open_integration_folder',args:{source:'codex',location:'/tmp/test-home/.codex/hooks.json'}}]);
});
