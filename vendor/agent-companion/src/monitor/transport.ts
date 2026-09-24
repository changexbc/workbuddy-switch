import { isDesktop, subscribeDesktop, enableNotifications } from '../desktop/host.js';
import type { MonitorAlert, ConnectionState, Snapshot } from '../types/snapshot.js';
import type { NotificationSettings } from '../types/settings.js';

export type Subscribe = (onSnapshot: (snapshot: Snapshot) => void, onConnection: (state: ConnectionState) => void) => () => void;

// Desktop and browser share the snapshot contract, independently of the scene.
export function createSSETransport(url = '/events'): { subscribe: Subscribe } {
  if (isDesktop()) return { subscribe: subscribeDesktop };
  return { subscribe(onSnapshot, onConnection) {
    const stream = new EventSource(url); let last = Date.now();
    onConnection('connecting');
    stream.onmessage = event => {
      try {
        const snapshot = JSON.parse(event.data) as Snapshot;
        if (snapshot.version !== 1 || !Array.isArray(snapshot.sessions) || !Array.isArray(snapshot.events)) throw new Error('Unsupported monitor protocol');
        last = Date.now(); onSnapshot(snapshot); onConnection(snapshot.ready ? 'connected' : 'connecting');
      } catch { onConnection('offline'); }
    };
    stream.onerror = () => onConnection('offline');
    const timer = setInterval(() => { if (Date.now() - last > 8000) onConnection('offline'); }, 2000);
    return () => { clearInterval(timer); stream.close(); };
  } };
}

export interface BrowserNotifierOptions {
  onSelect?: (sessionId: string) => void;
  storage?: Storage;
  options?: () => NotificationSettings;
}

export function createBrowserNotifier({onSelect, storage, options}: BrowserNotifierOptions = {}) {
  if (isDesktop()) return { enable: enableNotifications, send(_alert: MonitorAlert) {} };
  let enabled = false;
  try {enabled = storage?.getItem('astra.monitor.systemNotifications') === 'yes';} catch {}
  return {
    async enable() {
      if (!('Notification' in globalThis)) return 'unsupported';
      try {
        const permission = await Notification.requestPermission(); enabled = permission === 'granted';
        try {storage?.setItem('astra.monitor.systemNotifications', enabled ? 'yes' : 'no');} catch {}
        return permission;
      } catch {return 'denied';}
    },
    send(alert: MonitorAlert) {
      const prefs = options?.(); if (prefs && (!prefs.desktop || prefs[alert.kind] === false)) return;
      if (!(prefs?.desktop ?? enabled) || !('Notification' in globalThis) || Notification.permission !== 'granted') return;
      try {
        if (prefs?.sound) {try {const ctx = new AudioContext(), osc = ctx.createOscillator(), gain = ctx.createGain(); osc.connect(gain); gain.connect(ctx.destination); gain.gain.setValueAtTime(.035, ctx.currentTime); gain.gain.exponentialRampToValueAtTime(.001, ctx.currentTime + .2); osc.frequency.value = 660; osc.start(); osc.stop(ctx.currentTime + .2); osc.onended = () => ctx.close();} catch {}}
        const n = new Notification(`Agent Companion · ${alert.kind === 'wait' ? '需要确认' : alert.kind === 'done' ? '任务完成' : '任务失败'}`, {body: alert.title, tag: alert.id});
        n.onclick = () => {window.focus(); onSelect?.(alert.sessionId); n.close();};
      } catch { /* In-app notification remains available. */ }
    }
  };
}
