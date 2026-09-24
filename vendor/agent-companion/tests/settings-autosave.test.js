import test from 'node:test';
import assert from 'node:assert/strict';
import {createSettingsAutosave} from '../src/desktop/settings-autosave.ts';
const tick=()=>new Promise(resolve=>setImmediate(resolve));
const deferred=()=>{let resolve;return {promise:new Promise(r=>resolve=r),resolve:()=>resolve()};};

test('autosave retains newest edits while serializing settings and integration mutations',async()=>{
  const gate=deferred(), writes=[], states=[];
  const queue=createSettingsAutosave({preferences:async patch=>{writes.push(patch);if(writes.length===1)await gate.promise;},listening:async patch=>writes.push(patch),changed:s=>states.push(s),listeningSaved:()=>{}});
  assert.equal(writes.length,0,'creation never writes');
  queue.editPreferences({visibleCount:5});
  queue.editPreferences({visibleCount:7});
  queue.editPreferences({visibleCount:9,animation:false});
  assert.equal(queue.acquire(),false);
  assert.deepEqual(writes,[{visibleCount:5}]);
  gate.resolve();await tick();
  assert.deepEqual(writes,[{visibleCount:5},{visibleCount:9,animation:false}]);
  assert.equal(queue.acquire(),true);
  queue.editListening({codeg:false});
  assert.equal(writes.length,2,'integration lock holds subsequent edits');
  queue.release();await tick();
  assert.deepEqual(writes[2],{codeg:false});
  assert.deepEqual(states.at(-1),{busy:false,error:null,pending:false});
});

test('partial success is not replayed; failed patch merges newer choice and retries explicitly',async()=>{
  const writes=[], states=[];let fail=true, refreshed=0;
  const queue=createSettingsAutosave({preferences:async patch=>{writes.push(['preferences',patch]);if(fail)throw Error('disk');},listening:async patch=>writes.push(['listening',patch]),changed:s=>states.push(s),listeningSaved:()=>refreshed++});
  queue.acquire();queue.editListening({codex:false});queue.editPreferences({visibleCount:5});queue.release();await tick();
  assert.equal(refreshed,1);assert.equal(states.at(-1).error.message,'disk');
  queue.editPreferences({visibleCount:8});await tick();
  assert.equal(writes.length,2,'failure is not automatically retried');assert.equal(queue.acquire(),false);
  fail=false;queue.retry();await tick();
  assert.deepEqual(writes,[['listening',{codex:false}],['preferences',{visibleCount:5}],['preferences',{visibleCount:8}]]);
  assert.equal(states.at(-1).pending,false);
});
