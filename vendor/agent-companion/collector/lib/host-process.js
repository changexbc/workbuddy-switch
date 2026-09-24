// Hook-driven sources never report their own exit. The poll loop therefore
// probes the macOS host app directly; a missing main process is the only
// reliable signal that a force-quit happened without a SessionEnd hook.
import { execFile } from 'node:child_process';

const TERMINAL = new Set(['done', 'error', 'aborted']);

// Helper processes live under Contents/Frameworks, and a crashpad handler can
// outlive the app (ppid 1). Requiring the bundle-plus-Contents/MacOS shape
// keeps the probe on the main executable only.
export const HOST_BUNDLES = {
  workbuddy: ['WorkBuddy.app', 'WorkBuddy AI.app'],
  'codebuddy-ide': ['CodeBuddy.app', 'CodeBuddy CN.app'],
  // The VS Code plugin emits client=vscode; any live family member counts.
  vscode: ['Visual Studio Code.app', 'Code - Insiders.app', 'VSCodium.app', 'Cursor.app', 'Windsurf.app'],
};

export function matchHost(stdout, bundles) {
  const lines = String(stdout || '').split('\n');
  return bundles.some(bundle => lines.some(line => line.includes(`/${bundle}/Contents/MacOS/`)));
}

export function runPs() {
  return new Promise((resolve, reject) => {
    execFile('ps', ['-axo', 'comm='], { timeout: 1000, maxBuffer: 1024 * 1024 }, (error, stdout) => {
      if (error) reject(error); else resolve(String(stdout));
    });
  });
}

export class HostPresence {
  // `source` holds the host kind (workbuddy / codebuddy-ide / vscode).
  constructor({ source, run = runPs, now = Date.now, ttlMs = 5000, minMisses = 2, platform = process.platform } = {}) {
    this.source = source; this.run = run; this.now = now;
    this.ttlMs = ttlMs; this.minMisses = minMisses; this.platform = platform;
    this.seenAlive = false; this.misses = 0; this.lastCheck = -Infinity; this.cached = 'unknown'; this.declared = false;
  }
  // A hook proves the host was alive; it also invalidates a cached "gone" so
  // the next poll reports ok immediately instead of one beat later.
  noteHook() {
    this.seenAlive = true; this.misses = 0; this.declared = false;
    if (this.cached === 'gone') this.lastCheck = -Infinity;
  }
  async observe() {
    if (!['darwin', 'linux'].includes(this.platform)) return 'unknown';
    if (this.now() - this.lastCheck < this.ttlMs) return this.cached;
    this.lastCheck = this.now();
    let output;
    try { output = await this.run(); } catch { this.cached = 'unknown'; return 'unknown'; }
    const bundles = HOST_BUNDLES[this.source] || [];
    if (matchHost(output, bundles)) {
      this.seenAlive = true; this.misses = 0; this.declared = false;
      this.cached = 'alive'; return 'alive';
    }
    // Never claim an exit before the host was ever observed; a user who has
    // not launched the app should not see the source marked as exited.
    if (!this.seenAlive) { this.cached = 'unknown'; return 'unknown'; }
    this.misses++;
    if (this.misses >= this.minMisses) { this.declared = true; this.cached = 'gone'; return 'gone'; }
    this.cached = 'alive'; return 'alive';
  }
}

// Ending sessions keeps the record honest: the round did not finish, the app
// went away, and the front-end shows "已退出" for a short grace then drops it.
// Hook timestamps may run ahead of the collector clock, so the synthetic end
// must never look older than the round it closes.
export function endHostSessions(hub, source, { hostKind, ts = Date.now() } = {}) {
  const sessions = [...hub.sessions.values()].filter(s => s.source === source && !TERMINAL.has(s.status) && (!hostKind || s.hostKind === hostKind));
  for (const s of sessions) hub.ingest({ source, sessionId: s.sessionId, roundId: s.roundId, type: 'end', status: 'aborted', endedBy: 'host', ts: Math.max(ts, (s.updatedAt || 0) + 1) });
  return sessions.length;
}
