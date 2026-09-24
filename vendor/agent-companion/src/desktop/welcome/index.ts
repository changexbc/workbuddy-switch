import { createLineCat, type LineCatRenderer } from './cat-motion.js';
import './welcome.css';

const eye = (id: string) => `<g id="eye-${id}"><path d="M0-4.8Q0 0 0 4.8" fill="none" stroke="#263c31" stroke-width="4.4" stroke-linecap="round"/></g>`;

/**
 * The overlay element itself is rendered by React, which is what keeps the
 * blocking contract honest: `.welcome-blocking > :not(.desktop-welcome)` hides
 * every other direct child of the rail container, so the overlay has to be one.
 * This module fills that element and empties it again; it never creates,
 * removes or re-parents it, and it never writes a `className` or a child that
 * React also declares.
 */
function fillScene(svg: SVGSVGElement, width: number, height: number) {
  svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
  svg.innerHTML = `<defs><linearGradient id="welcome-spectrum" gradientUnits="userSpaceOnUse">
      <stop offset="0"/><stop offset=".125"/><stop offset=".25"/><stop offset=".375"/>
      <stop offset=".5"/><stop offset=".625"/><stop offset=".75"/><stop offset=".875"/><stop offset="1"/>
    </linearGradient></defs>
    <path id="far-legs" fill="#fff" stroke="url(#welcome-spectrum)" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"/>
    <path id="living-line" fill="#fff" stroke="url(#welcome-spectrum)" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"/>
    <g id="eyes">${eye('left')}${eye('right')}</g>`;
}

export interface RailWelcomeState {
  /** `pending` before the first play, then `preparing` / `running` / a finish reason. */
  phase: string;
  blocking: boolean;
  running: boolean;
}

export interface RailWelcomeOptions {
  /** The strip, measured for the character's scale and start point. */
  rail: () => HTMLElement | null;
  auto?: boolean;
  /** Phase changes only; the per-frame elapsed value has its own channel. */
  onStateChange: (state: RailWelcomeState) => void;
  onElapsed: (seconds: number) => void;
}

export interface RailWelcome {
  readonly blocking: boolean;
  readonly running: boolean;
  /** Called by React when the overlay element mounts or unmounts. */
  attach: (host: SVGSVGElement | null) => void;
  play: () => boolean;
  setPreferences: (value: {animation?: boolean} | null) => void;
  update: (hasUrgentSession: boolean) => void;
  setActive: (value: boolean) => void;
  dispose: () => void;
}

// One bounded animation in the existing transparent rail WebView. No extra window,
// synthetic sessions, collector traffic, timers or RAF survive completion.
export function createRailWelcome({ rail, auto = false, onStateChange, onElapsed }: RailWelcomeOptions): RailWelcome {
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  let preferences: {animation?: boolean} | null = null;
  let urgent = false, disposed = false, host: SVGSVGElement | null = null, renderer: LineCatRenderer | null = null;
  let frame = 0, timer: ReturnType<typeof setTimeout> | undefined, timeout: ReturnType<typeof setTimeout> | undefined;
  let active = true, started = 0, elapsed = 0, bounds: DOMRect | null = null, consumed = false;
  let blocking = false, running = false, phase = 'idle';
  let pendingPlay: {bounds: DOMRect; motionScale: number} | null = null;
  // Each new rail controller is a new launch; previous launches must not suppress it.
  let pending = auto && !reduced.matches;

  function publish(next: Partial<RailWelcomeState>) {
    const beforeBlocking = blocking;
    if (next.phase !== undefined) phase = next.phase;
    if (next.blocking !== undefined) blocking = next.blocking;
    if (next.running !== undefined) running = next.running;
    if (blocking !== beforeBlocking || next.running !== undefined || next.phase !== undefined) onStateChange({phase, blocking, running});
  }

  publish({phase: pending ? 'pending' : 'idle', blocking: pending, running: false});

  function finish(reason = 'complete') {
    clearTimeout(timer); clearTimeout(timeout); cancelAnimationFrame(frame); frame = 0;
    pending = false;
    pendingPlay = null;
    if (host) host.replaceChildren();
    renderer = null;
    bounds = null;
    publish({phase: reason === 'complete' ? 'settled' : reason, blocking: false, running: false});
  }
  function eligible() {
    return !disposed && active && !document.hidden && !reduced.matches && preferences?.animation !== false && preferences !== null && !urgent;
  }
  function play(): boolean {
    if (!eligible()) { finish('skipped'); return false; }
    finish('preparing');
    const strip = rail();
    if (!strip) { finish('no-room'); return false; }
    strip.getAnimations().forEach(animation => animation.cancel());
    bounds = strip.getBoundingClientRect();
    // Character remains entirely inside the resident 368px window.
    const motionScale = Math.min(.55, (bounds.x - 10) / 680, (bounds.bottom - bounds.width / 2 - 4) / 160);
    if (motionScale < .16 || !bounds.height) { finish('no-room'); return false; }
    // React mounts the overlay from this state change; the animation itself
    // starts in attach(), once there is an element to draw into.
    pendingPlay = {bounds, motionScale};
    publish({phase: 'running', blocking: true, running: true});
    return true;
  }

  function start() {
    if (!pendingPlay || !host) return;
    const {bounds: rect, motionScale} = pendingPlay;
    pendingPlay = null;
    fillScene(host, innerWidth, innerHeight);
    renderer = createLineCat(host, {rail: {x: rect.x, y: rect.y, width: rect.width, height: rect.height}, motionScale});
    elapsed = 0; consumed = true;
    started = performance.now();
    renderer.render(0); onElapsed(0);
    const tick = (now: number) => {
      if (!eligible()) return finish('interrupted');
      elapsed = Math.min(7, (now - started) / 1000);
      try { renderer!.render(elapsed); } catch { return finish('render-error'); }
      onElapsed(elapsed);
      if (elapsed >= 6.65 && blocking) publish({blocking: false});
      if (elapsed >= 7) return finish();
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    // Fail open if a suspended renderer does not reach its last frame.
    timeout = setTimeout(() => finish('timeout'), 9000);
  }

  const layout = new ResizeObserver(() => {
    const strip = rail();
    if (!running || !bounds || !strip) return;
    const next = strip.getBoundingClientRect();
    if (Math.abs(next.height - bounds.height) > 1 || Math.abs(next.x - bounds.x) > 1 || Math.abs(next.y - bounds.y) > 1) finish('layout-change');
  });
  const visibility = () => { if (document.hidden) finish('hidden'); };
  const motion = () => { if (reduced.matches) finish('reduced-motion'); };
  const resize = () => { if (running) finish('resize'); };
  document.addEventListener('visibilitychange', visibility);
  reduced.addEventListener('change', motion);
  window.addEventListener('resize', resize);
  // Settings failures cannot leave the actual rail hidden.
  if (pending) timeout = setTimeout(() => finish('settings-timeout'), 1800);

  return {
    get blocking() { return blocking; },
    get running() { return running; },
    attach(next) {
      host = next;
      if (!next) return;
      const strip = rail();
      if (strip) layout.observe(strip);
      start();
    },
    play,
    setPreferences(value) {
      preferences = value;
      if (!value || !value.animation || reduced.matches) { if (pending || running) finish('disabled'); return; }
      if (pending && !consumed) { clearTimeout(timer); timer = setTimeout(play, 400); }
    },
    update(hasUrgentSession) { urgent = hasUrgentSession; if (urgent && (pending || running)) finish('urgent-session'); },
    setActive(value) { active = value; if (!active && (pending || running)) finish('hidden'); },
    dispose() {
      disposed = true; finish('disposed'); layout.disconnect();
      document.removeEventListener('visibilitychange', visibility);
      reduced.removeEventListener('change', motion); window.removeEventListener('resize', resize);
    },
  };
}
