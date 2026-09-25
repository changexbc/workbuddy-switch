import { createRoot } from 'react-dom/client';
import { RailApp } from './components/Rail';
import { createRailController } from './rail-controller';
import { disableNativeContentDrag } from './host.js';
import './rail.css';

// A rail is a window, not a document: nothing in it may start a native drag.
disableNativeContentDrag();

const container = document.getElementById('desktop-rail');
if (container) void boot(container);

/**
 * The demo build runs this same rail against the stage page that embeds it
 * instead of against a host. The choice is a build-time constant, so a query
 * parameter can never turn the production rail into the demo one; in the
 * production build the branch is dead code and the demo module is never
 * emitted.
 */
async function boot(target: HTMLElement) {
  const demo = import.meta.env.VITE_DEMO_MODE === '1';
  const controller = demo
    ? createRailController((await import('../demo/rail-bridge.js')).createDemoRailOptions())
    : createRailController();
  // `#desktop-rail` is the React container, so React never writes its
  // attributes; the controller owns the classes and datasets that describe the
  // window as a whole (inactive, motion-paused, welcome phase).
  controller.attach.container(target);
  createRoot(target).render(<RailApp controller={controller} container={target} />);
  window.addEventListener('pagehide', () => controller.dispose(), {once: true});
  controller.start();
}
