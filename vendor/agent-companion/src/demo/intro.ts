/** Reuse the desktop's kitten renderer in the demo, without changing the rail. */
import { createRailWelcome } from '../desktop/welcome/index.js';

export function playDemoIntro(frame: HTMLIFrameElement, onFinish: () => void) {
  const strip = frame.contentDocument?.querySelector<HTMLElement>('.desktop-strip');
  if (!strip) { onFinish(); return {dispose() {}}; }
  const viewport = frame.getBoundingClientRect();
  const rect = strip.getBoundingClientRect();
  const anchor = document.createElement('div');
  anchor.className = 'demo-intro-anchor';
  anchor.style.cssText = `position:fixed;pointer-events:none;visibility:hidden;left:${viewport.x + rect.x}px;top:${viewport.y + rect.y}px;width:${rect.width}px;height:${rect.height}px`;
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.classList.add('desktop-welcome', 'demo-intro');
  svg.setAttribute('aria-hidden', 'true');
  document.body.append(anchor, svg);
  let finished = false;
  const welcome = createRailWelcome({
    rail: () => anchor,
    onElapsed: seconds => { svg.dataset.elapsed = seconds.toFixed(2); },
    onStateChange(state) {
      svg.dataset.phase = state.phase;
      frame.style.visibility = state.blocking ? 'hidden' : '';
      if (state.running) queueMicrotask(() => { if (!finished) welcome.attach(svg); });
      if (!state.running && !['idle', 'preparing'].includes(state.phase) && !finished) {
        finished = true;
        // Defer teardown until the renderer has finished publishing its state.
        queueMicrotask(() => { welcome.dispose(); anchor.remove(); svg.remove(); onFinish(); });
      }
    },
  });
  welcome.setPreferences({animation: true});
  welcome.play();
  return {dispose() { finished = true; welcome.dispose(); anchor.remove(); svg.remove(); frame.style.visibility = ''; }};
}
