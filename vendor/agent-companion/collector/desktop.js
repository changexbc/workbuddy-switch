// One supervised helper for every desktop window. Stdout is exclusively JSONL.
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import readline from 'node:readline';
import { fileURLToPath } from 'node:url';
import { createCollector } from './lib/collector.js';
import { startServer } from './server.js';
import { createNotificationTracker } from '../src/monitor/model.js';

export function snapshotKey(snapshot) {
  return JSON.stringify({ ...snapshot, ts: undefined,
    sessions: snapshot.sessions.map(s => ({ ...s, elapsed: undefined })),
    sources: Object.fromEntries(Object.entries(snapshot.sources).map(([id, value]) => [id, { ...value, checkedAt: undefined }])),
  });
}

export async function startDesktopCollector({ home = process.env.AGENT_STUDIO_HOME || os.homedir(), input = process.stdin, output = process.stdout } = {}) {
  const file = path.join(home, '.agent-studio', 'desktop-notifications.json');
  let saved = '', pendingSave = Promise.resolve(), shuttingDown = false, lastKey = '';
  try { saved = await fs.readFile(file, 'utf8'); } catch {}
  const storage = {
    getItem: () => saved,
    setItem(_key, value) {
      saved = value;
      pendingSave = pendingSave.then(async () => {
        await fs.mkdir(path.dirname(file), { recursive: true });
        const temporary = `${file}.${process.pid}.tmp`;
        await fs.writeFile(temporary, value, { mode: 0o600 }); await fs.rename(temporary, file);
      }).catch(error => emit({ type: 'warning', message: `提醒记录保存失败：${error.code || error.message}` }));
    },
  };
  const emit = value => { if (!output.destroyed) output.write(`${JSON.stringify(value)}\n`); };
  const tracker = createNotificationTracker({ storage });
  const collector = createCollector({ home });
  const runtime = await startServer({ port: 0, collector, staticRoot: path.join(home, '.agent-studio', 'no-static-files') });
  function publish(force = false) {
    const snapshot = collector.hub.snapshot(), key = snapshotKey(snapshot);
    if (!force && key === lastKey) return;
    lastKey = key; emit({ type: 'state', snapshot });
    const prefs = collector.getSettings().notifications;
    for (const alert of tracker.ingest(snapshot)) if (prefs.desktop && prefs[alert.kind] !== false) {
      emit({ type: 'notification', alert: { id: alert.id, sessionId: alert.sessionId, kind: alert.kind, title: alert.title, sound: prefs.sound } });
    }
  }
  emit({ type: 'ready', url: `http://127.0.0.1:${runtime.server.address().port}` }); publish(true);
  const timer = setInterval(publish, 1000);
  let commands = Promise.resolve();
  const lines = readline.createInterface({ input, crlfDelay: Infinity });
  lines.on('line', line => {
    if (line.length > 65536 || shuttingDown) return;
    commands = commands.then(async () => {
      let request;
      try {
        request = JSON.parse(line);
        let value;
        switch (request.command) {
          case 'settings_get': value = collector.getSettings(); break;
          case 'settings_set': value = await collector.updateSettings(request.payload); break;
          case 'settings_check': value = await collector.checkSource(request.payload); break;
          case 'custom_integrations_get': value = await collector.customIntegrationsGet(); break;
          case 'custom_integrations_set': value = await collector.customIntegrationsSet(request.payload); break;
          case 'custom_preview': value = await collector.customPreview(request.payload); break;
          case 'custom_hook': value = await collector.ingestCustomHook(request.payload); break;
          default: throw Error('未知的采集命令');
        }
        emit({ type: 'reply', id: request.id, value });
        if (request.command === 'settings_set') emit({ type: 'settings', value });
        publish();
      } catch (error) { emit({ type: 'reply', id: request?.id, error: error.message }); }
    });
  });
  async function close() {
    if (shuttingDown) return;
    shuttingDown = true; clearInterval(timer); lines.close(); await commands; publish(true); await runtime.close(); await pendingSave;
  }
  return { close, runtime, lines };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const desktop = await startDesktopCollector();
    const quit = async () => { await desktop.close(); process.exit(0); };
    process.stdin.once('end', quit);
    for (const signal of ['SIGTERM', 'SIGINT']) process.once(signal, quit);
  } catch (error) { process.stdout.write(`${JSON.stringify({ type: 'fatal', message: error.message })}\n`); process.exit(1); }
}
