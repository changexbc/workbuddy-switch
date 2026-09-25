/**
 * The rail as it runs inside the public demo page.
 *
 * `desktop.html` is unchanged; the demo build only replaces the controller's
 * native edges, so the frame renders the real rail components against fictional
 * snapshots that arrive over `postMessage` from the stage around it. Nothing
 * here can reach a host: the data source is the page, the link opener switches fictional clients, the
 * close command stays isolated from the host, preferences are
 * fixed in code, and the portrait cache lives in this frame's memory.
 */
import type { RailControllerOptions } from '../desktop/rail-controller.js';
import type { PortraitStorage } from '../desktop/rail-model.js';
import type { RailPreferencesState } from '../types/settings.js';
import type { ConnectionState, Snapshot } from '../types/snapshot.js';
import { DEMO_CHANNEL, isDemoPageMessage, type DemoFrameReport } from './protocol.js';

import { DEMO_CAST, demoClientForSession } from './scenarios.js';

const OPEN_SESSION_TEXT = '桌面版可打开原会话';
const CLOSE_MONITORING_TEXT = '关闭监听仅在桌面版可用';

/** Session-only cache: the demo never reads or writes the visitor's storage. */
function memoryStorage(): PortraitStorage {
  const values = new Map<string, string>();
  return {
    getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); },
  };
}

/**
 * Two GUI tasks are visible together; the production rail still owns sizing.
 */
const DEMO_PREFERENCES: RailPreferencesState = {
  avatarStyle: 'animal',
  visibleCount: 2,
  animation: true,
  size: 'standard',
  autostart: false,
  autostartSupported: false,
  autostartManaged: true,
};

export function createDemoRailOptions(): RailControllerOptions {
  const snapshots = new Set<(snapshot: Snapshot) => void>();
  const connections = new Set<(state: ConnectionState) => void>();
  let connected = false;

  const tell = (report: DemoFrameReport) => {
    // Opened on its own the frame has no stage to talk to; it then simply waits.
    if (window.parent === window) return;
    window.parent.postMessage({channel: DEMO_CHANNEL, ...report}, window.location.origin);
  };

  window.addEventListener('message', (event: MessageEvent<unknown>) => {
    // Same origin only, and only from the window that embeds this frame.
    if (event.origin !== window.location.origin || event.source !== window.parent) return;
    if (!isDemoPageMessage(event.data)) return;
    // The first snapshot also means the page is alive, so the rail stops saying
    // it is connecting and admits the rows.
    if (!connected) {
      connected = true;
      for (const notify of connections) notify('connected');
    }
    for (const listener of snapshots) listener(event.data.snapshot);
  });

  // The grip is the desktop window's drag region. In the stage it moves the
  // simulated window instead: the frame owns the pointer, the page owns the
  // position, and the offsets stay relative to the drag's start.
  //
  // Screen coordinates, not client coordinates: this frame travels with the
  // window it is dragging, so `clientX` only ever reports the lag that is left
  // after the page caught up (the window would crawl at half the pointer). The
  // screen position is measured against the desktop and keeps moving.
  let drag: {pointerId: number; screenX: number; screenY: number} | null = null;
  document.addEventListener('pointerdown', event => {
    if (event.button !== 0 || drag) return;
    const grip = (event.target as Element).closest('.desktop-grip');
    if (!(grip instanceof HTMLElement)) return;
    drag = {pointerId: event.pointerId, screenX: event.screenX, screenY: event.screenY};
    grip.setPointerCapture(event.pointerId);
    tell({type: 'drag', phase: 'start', dx: 0, dy: 0});
  });
  document.addEventListener('pointermove', event => {
    if (!drag || event.pointerId !== drag.pointerId) return;
    tell({type: 'drag', phase: 'move', dx: event.screenX - drag.screenX, dy: event.screenY - drag.screenY});
  });
  const endDrag = (event: PointerEvent) => {
    if (!drag || event.pointerId !== drag.pointerId) return;
    drag = null;
    tell({type: 'drag', phase: 'end', dx: 0, dy: 0});
  };
  document.addEventListener('pointerup', endDrag);
  document.addEventListener('pointercancel', endDrag);

  // Capture before React's native-launch handler. Keyboard activation also emits
  // click, and the cast allowlist prevents arbitrary DOM IDs becoming clients.
  document.addEventListener('click', event => {
    if (!(event.target instanceof Element)) return;
    const avatar = event.target.closest<HTMLElement>('.desktop-avatar[data-session-id]');
    const client = demoClientForSession(avatar?.dataset.sessionId);
    if (!client) return;
    event.preventDefault();
    event.stopPropagation();
    tell({type: 'activate-client', client});
  }, true);

  tell({type: 'ready'});

  return {
    subscribe(onSnapshot, onConnection) {
      onConnection('connecting');
      snapshots.add(onSnapshot);
      connections.add(onConnection);
      return () => { snapshots.delete(onSnapshot); connections.delete(onConnection); };
    },
    loadPreferences: async () => DEMO_PREFERENCES,
    storage: memoryStorage(),
    openSessionLink: (url: string) => {
      // Resolve only the two fictional identities; never navigate to the URL.
      const link = new URL(url);
      const member = DEMO_CAST.find(member => link.protocol === `${member.source}:`
        && link.hostname === (member.source === 'codex' ? 'threads' : 'chat')
        && link.pathname === `/${encodeURIComponent(member.sessionId)}`);
      if (member) { tell({type: 'activate-client', client: member.source}); return; }
      tell({type: 'blocked', action: 'open-session', text: OPEN_SESSION_TEXT});
      throw new Error(OPEN_SESSION_TEXT);
    },
    closeMonitoring: async () => {
      tell({type: 'blocked', action: 'close-monitoring', text: CLOSE_MONITORING_TEXT});
      throw new Error(CLOSE_MONITORING_TEXT);
    },
  };
}
