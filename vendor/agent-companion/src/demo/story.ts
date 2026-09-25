/** One clock drives the fictional clients and the real rail snapshots. */
import { buildStorySnapshot, type DemoClient } from './scenarios.js';
export type StoryPhase = 'running' | 'wait' | 'done' | 'error';
export const STORY_CHOICES = ['精简版', '标准版', '详细版', '双语版'] as const;
export const STORY_QUESTION = '选择版本';
export interface StoryState {
  roundId: string;
  codex: StoryPhase;
  workbuddy: StoryPhase;
  codexStep: number;
  workbuddyStep: number;
  choice: number | null;
  codexChoice: number | null;
  codexEndedAt?: number;
  workbuddyEndedAt?: number;
}
export function createStory(onChange: (state: StoryState) => void) {
  let sequence = 0;
  let state: StoryState = initial();
  let paused = false;
  let timers: {remaining: number; due: number; timer?: ReturnType<typeof setTimeout>; run: () => void}[] = [];
  function initial(): StoryState { return {roundId: `story-${Date.now()}-${++sequence}`, codex: 'running', workbuddy: 'running', codexStep: 0, workbuddyStep: 0, choice: null, codexChoice: null}; }
  function emit() { onChange({...state}); }
  function arm(job: typeof timers[number]) {
    job.due = performance.now() + job.remaining;
    job.timer = setTimeout(() => { timers = timers.filter(other => other !== job); job.run(); emit(); }, job.remaining);
  }
  function later(delay: number, run: () => void) {
    const job = {remaining: delay, due: 0, run}; timers.push(job); if (!paused) arm(job);
  }
  function stop() { timers.forEach(job => clearTimeout(job.timer)); timers = []; }
  return {
    reset() { stop(); state = initial(); emit(); },
    start() {
      stop();
      later(600, () => { state.codexStep = 1; });
      later(800, () => { state.workbuddyStep = 1; });
      later(2500, () => { state.workbuddy = 'wait'; });
      later(1800, () => { state.codexStep = 2; });
      later(3000, () => { state.codex = 'wait'; });
    },
    choose(choice: number, client: DemoClient = 'workbuddy') {
      if (state[client] !== 'wait' || !Number.isInteger(choice) || choice < 1 || choice > STORY_CHOICES.length) return false;
      if (client === 'codex') state.codexChoice = choice;
      else state.choice = choice;
      state[client] = 'running'; emit();
      later(1200, () => { if (client === 'codex') state.codexStep = 3; else state.workbuddyStep = 2; });
      later(3500, () => {
        if (client === 'codex') { state.codex = 'done'; state.codexEndedAt = Date.now(); }
        else { state.workbuddy = 'error'; state.workbuddyEndedAt = Date.now(); }
      });
      return true;
    },
    pause(value: boolean) {
      if (paused === value) return;
      paused = value;
      for (const job of timers) {
        if (paused) { clearTimeout(job.timer); job.remaining = Math.max(0, job.due - performance.now()); }
        else arm(job);
      }
    },
    dispose: stop,
    snapshot: () => buildStorySnapshot(state, Date.now()),
  };
}
