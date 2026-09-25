/** The demo shell owns placement and fictional scenarios; the rail stays unchanged. */
import { isDemoClient, type DemoClient } from './scenarios.js';
import { DEMO_CHANNEL, isDemoFrameMessage, type DemoBlockedAction, type DemoPageMessage } from './protocol.js';
import type { Snapshot } from '../types/snapshot.js';
import { playDemoIntro } from './intro.js';
import './demo-page.css';
import { createStory, STORY_CHOICES, type StoryState } from './story.js';

const BLOCKED_FEEDBACK: Record<DemoBlockedAction, string> = {
  'open-session': '会话跳转被拦截：演示不会打开真实会话（桌面版可打开原会话）',
  'close-monitoring': '「关闭本次监听」被拦截：演示不会向本机发送任何命令',
};



function required<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`演示页缺少元素：${selector}`);
  return element;
}

const stage = required<HTMLElement>('#demo-desktop');
const mockWindow = required<HTMLElement>('#demo-window');
const frame = required<HTMLIFrameElement>('#demo-frame');
const status = required<HTMLElement>('#demo-status');
const feedback = required<HTMLElement>('#demo-feedback');
const resetButton = required<HTMLButtonElement>('[data-action=reset]');

function say(text: string) {
  feedback.textContent = `最近操作：${text}`;
}

const clientWindows = [...document.querySelectorAll<HTMLElement>('[data-client]')];
const clientButtons = [...document.querySelectorAll<HTMLButtonElement>('[data-activate-client]')];
const clientScene = required<HTMLElement>('.demo-scene');
const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
const clientAnimations = new Map<HTMLElement, Animation>();

function settleClients() {
  for (const animation of clientAnimations.values()) animation.cancel();
  clientAnimations.clear();
}

function activateClient(client: DemoClient) {
  if (clientScene.dataset.activeClient === client) return;
  // Capture painted positions before cancelling an interrupted swap so
  // rapid client changes remain continuous.
  const previous = clientWindows.map(element => {
    const style = getComputedStyle(element);
    return {element, rect: element.getBoundingClientRect(), opacity: style.opacity, shadow: style.boxShadow};
  });
  settleClients();
  clientScene.dataset.activeClient = client;
  for (const window of clientWindows) window.classList.toggle('is-active', window.dataset.client === client);
  for (const button of clientButtons) button.setAttribute('aria-pressed', String(button.dataset.activateClient === client));
  if (reducedMotion.matches) return;

  for (const {element, rect, opacity, shadow} of previous) {
    const target = element.getBoundingClientRect();
    if (!target.width || !target.height) continue;
    const style = getComputedStyle(element);
    const animation = element.animate([
      {transform: `translate(${rect.left - target.left}px, ${rect.top - target.top}px) scale(${rect.width / target.width}, ${rect.height / target.height})`, opacity, boxShadow: shadow},
      {transform: 'none', opacity: style.opacity, boxShadow: style.boxShadow},
    ], {duration: 380, easing: 'cubic-bezier(.22, .75, .25, 1)'});
    clientAnimations.set(element, animation);
    animation.onfinish = () => {
      if (clientAnimations.get(element) !== animation) return;
      clientAnimations.delete(element);
      animation.cancel();
    };
  }
}

window.addEventListener('resize', settleClients);
window.addEventListener('pagehide', settleClients);
reducedMotion.addEventListener('change', settleClients);

for (const window of clientWindows) {
  window.addEventListener('click', () => {
    if (isDemoClient(window.dataset.client)) activateClient(window.dataset.client);
  });
}

// ------------------------------------------------------------------ placement

let position = {left: 0, top: 0};

/** Keep the transparent rail viewport inside the demo stage. */
function place(left: number, top: number) {
  const maxLeft = Math.max(0, stage.clientWidth - mockWindow.offsetWidth);
  const maxTop = Math.max(0, stage.clientHeight - mockWindow.offsetHeight);
  position = {
    left: Math.min(Math.max(0, left), maxLeft),
    top: Math.min(Math.max(stage.clientWidth > 800 ? -65 : 0, top), maxTop),
  };
  mockWindow.style.left = `${position.left}px`;
  mockWindow.style.top = `${position.top}px`;
}

function centreWindow() {
  const narrow = stage.clientWidth <= 800;
  const scene = clientScene.getBoundingClientRect();
  const bounds = stage.getBoundingClientRect();
  // Reserve a separate band for the rail so its transparent iframe cannot
  // eat client clicks: above the desktop pair, below the narrow-screen stack.
  place(narrow ? (stage.clientWidth - mockWindow.offsetWidth) / 2 : stage.clientWidth - mockWindow.offsetWidth - 20,
    narrow ? scene.bottom - bounds.top + 12 : -65);
}

// ----------------------------------------------------------------------- drag

/** The rail grip sends drag offsets through the frame protocol. */
let dragOrigin: {left: number; top: number} | null = null;

function beginDrag() { dragOrigin = {...position}; }
function moveDrag(dx: number, dy: number) { if (dragOrigin) place(dragOrigin.left + dx, dragOrigin.top + dy); }
function endDrag() {
  if (!dragOrigin) return;
  dragOrigin = null;
  say('已移动悬浮栏（位置只保存在本页）');
}

window.addEventListener('resize', centreWindow);

// Keep the inline loading screen until the styled iframe has committed its first
// snapshot. The bridge's ready message means transport-ready, not paint-ready.
let revealStarted = false;
async function revealDemo() {
  if (revealStarted) return;
  revealStarted = true;
  const loading = required<HTMLElement>('#demo-loading');
  try {
    if (frame.contentDocument?.readyState !== 'complete') {
      await new Promise<void>(resolve => frame.addEventListener('load', () => resolve(), {once: true}));
    }
    const documentInFrame = frame.contentDocument;
    if (!documentInFrame) throw new Error('Demo frame unavailable');
    await new Promise<void>((resolve, reject) => {
      const rendered = () => documentInFrame.querySelector('.desktop-avatar[data-status=running]');
      if (rendered()) { resolve(); return; }
      const observer = new MutationObserver(() => {
        if (rendered()) { observer.disconnect(); clearTimeout(timeout); resolve(); }
      });
      const timeout = window.setTimeout(() => {
        observer.disconnect();
        reject(new Error('Demo first frame timed out'));
      }, 15000);
      observer.observe(documentInFrame.body, {childList: true, subtree: true, attributes: true});
    });
    await Promise.all([document.fonts.ready, documentInFrame.fonts.ready]);
    await Promise.all([...document.images, ...documentInFrame.images].map(img => img.decode()));
    // The first snapshot animates the strip from its empty height to two rows.
    // Measure the opening silhouette only after that finite height transition.
    const strip = documentInFrame.querySelector<HTMLElement>('.desktop-strip');
    await Promise.allSettled((strip?.getAnimations() ?? []).map(animation => animation.finished));
    // Let layout and the first styled paint settle before exposing the scene.
    await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    const shell = required<HTMLElement>('.demo-shell');
    shell.inert = false;
    shell.removeAttribute('aria-busy');
    loading.hidden = true;
    intro = playDemoIntro(frame, () => { story.start(); });
  } catch {
    required<HTMLElement>('#demo-loading-text').textContent = '加载失败，请重新加载';
    required<HTMLElement>('#demo-loading-retry').hidden = false;
  }
}

// -------------------------------------------------------------------- protocol

function post(snapshot: Snapshot) {
  const target = frame.contentWindow;
  if (!target) return;
  const message: DemoPageMessage = {channel: DEMO_CHANNEL, type: 'snapshot', snapshot};
  target.postMessage(message, window.location.origin);
}

let intro: ReturnType<typeof playDemoIntro> | undefined;
let ready = false;
const story = createStory(renderStory);
function renderStory(state: StoryState) {
  for (const client of ['codex', 'workbuddy'] as const) {
    const window = required<HTMLElement>(`[data-client=${client}]`);
    window.dataset.storyStatus = state[client];
    const step = state[client === 'codex' ? 'codexStep' : 'workbuddyStep'];
    window.querySelectorAll<HTMLElement>('[data-reveal]').forEach(row => row.classList.toggle('is-revealed', step >= Number(row.dataset.reveal)));
    window.querySelectorAll<HTMLElement>('[data-step]').forEach(row => row.classList.toggle('is-checked', step >= Number(row.dataset.step)));
    window.querySelector<HTMLElement>('.story-result')!.hidden = !['done', 'error'].includes(state[client]);
    const choice = window.querySelector<HTMLElement>('.story-choice')!;
    choice.hidden = state[client] !== 'wait';
    const selected = client === 'codex' ? state.codexChoice : state.choice;
    const selection = window.querySelector<HTMLElement>('.story-selection')!;
    selection.hidden = selected === null;
    selection.textContent = selected ? `✓ ${selected}` : '';
    selection.setAttribute('aria-label', selected ? `已选择${STORY_CHOICES[selected - 1]}` : '');
  }
  required<HTMLElement>('.demo-shell').style.setProperty('--scene-glow', state.workbuddy === 'wait' ? '#fff0d8' : '#d9f3e7');
  status.textContent = `Codex：${state.codex === 'wait' ? '待确认' : state.codex === 'done' ? '已完成' : '工作中'}；WorkBuddy：${state.workbuddy === 'wait' ? '待确认' : state.workbuddy === 'error' ? '运行错误' : '工作中'}`;
  if (ready) post(story.snapshot());
}
for (const button of document.querySelectorAll<HTMLButtonElement>('[data-choice]')) {
  button.addEventListener('click', () => {
    const client = button.closest<HTMLElement>('[data-client]')?.dataset.client;
    if (isDemoClient(client) && story.choose(Number(button.dataset.choice), client)) clientButtons.find(button => button.dataset.activateClient === client)?.focus({preventScroll: true});
  });
}
document.addEventListener('visibilitychange', () => story.pause(document.hidden));
window.addEventListener('pagehide', () => { story.dispose(); intro?.dispose(); });
const readyTimeout = window.setTimeout(() => { if (!ready) say('悬浮栏尚未就绪，请重新加载'); }, 10000);

window.addEventListener('message', (event: MessageEvent<unknown>) => {
  // Same origin only, and only from the frame this page embedded.
  if (event.origin !== window.location.origin || event.source !== frame.contentWindow) return;
  if (!isDemoFrameMessage(event.data)) return;
  const message = event.data;
  if (message.type === 'ready') {
    ready = true;
    clearTimeout(readyTimeout);
    centreWindow();
    story.reset();
    story.pause(document.hidden);
    if (revealStarted) story.start();
    else void revealDemo();
    return;
  }
  if (message.type === 'activate-client') {
    activateClient(message.client);
    return;
  }
  if (message.type === 'drag') {
    if (message.phase === 'start') {
      if (!dragOrigin) { beginDrag(); say('正在拖动悬浮栏的握把…'); }
    } else if (message.phase === 'move') moveDrag(message.dx, message.dy);
    else endDrag();
    return;
  }
  say(BLOCKED_FEEDBACK[message.action]);
});

resetButton.addEventListener('click', () => {
  story.dispose();
  intro?.dispose();
  revealStarted = false;
  activateClient('workbuddy');
  ready = false;
  story.reset();
  frame.contentWindow?.location.reload();
});

place(0, 0);
centreWindow();
frame.src = new URL('desktop.html', window.location.href).href;
