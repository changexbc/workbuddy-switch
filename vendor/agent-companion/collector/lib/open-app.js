import { spawn } from 'node:child_process';

export const DESKTOP_APPS = {
  codeg: 'app.codeg',
  'codebuddy-ide': 'com.tencent.codebuddycn',
  'codebuddy-ide-international': 'com.tencent.codebuddy',
};

export function openMacApp(bundleId, spawnProcess = spawn) {
  if (process.platform !== 'darwin') {
    const error = new Error('APP_OPEN_UNSUPPORTED');
    error.code = 'APP_OPEN_UNSUPPORTED';
    throw error;
  }
  if (typeof bundleId !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9.-]+$/.test(bundleId)) {
    const error = new Error('APP_OPEN_INVALID');
    error.code = 'APP_OPEN_INVALID';
    throw error;
  }
  return new Promise((resolve, reject) => {
    const child = spawnProcess('open', ['-b', bundleId], { stdio: 'ignore' });
    child.on('error', reject);
    child.on('close', code => {
      if (code === 0) resolve();
      else {
        const error = new Error('APP_OPEN_FAILED');
        error.code = 'APP_OPEN_FAILED';
        error.exitCode = code;
        reject(error);
      }
    });
  });
}

export async function openDesktopSource(source, { openApp = openMacApp, edition } = {}) {
  const domestic = source === 'codebuddy-ide' && ['domestic', 'codebuddycn'].includes(String(edition || '').toLowerCase());
  const bundleId = DESKTOP_APPS[source === 'codebuddy-ide' && !domestic ? 'codebuddy-ide-international' : source];
  if (!bundleId) {
    const error = new Error('UNSUPPORTED_SOURCE');
    error.code = 'UNSUPPORTED_SOURCE';
    throw error;
  }
  await openApp(bundleId);
  return { ok: true, app: source };
}
