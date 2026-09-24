import { createRoot } from 'react-dom/client';
import { RailApp } from './components/Rail';
import { createRailController } from './rail-controller';
import { disableNativeContentDrag } from './host.js';
import './rail.css';

// A rail is a window, not a document: nothing in it may start a native drag.
disableNativeContentDrag();

const container = document.getElementById('desktop-rail');
if (container) {
  const controller = createRailController();
  // `#desktop-rail` is the React container, so React never writes its
  // attributes; the controller owns the classes and datasets that describe the
  // window as a whole (inactive, motion-paused, welcome phase).
  controller.attach.container(container);
  createRoot(container).render(<RailApp controller={controller} container={container} />);
  window.addEventListener('pagehide', () => controller.dispose(), {once: true});
  controller.start();
}
