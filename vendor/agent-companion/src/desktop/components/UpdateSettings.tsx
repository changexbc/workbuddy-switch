import * as React from 'react';
import { Button } from '@/components/ui/button';
import { Label } from '@/components/ui/label';
import { errorMessage } from '@/types/commands.js';
import { idleUpdateSnapshot, type UpdateConfig, type UpdateSnapshot } from '@/types/update.js';
import {
  checkForUpdates,
  downloadUpdate,
  installUpdate,
  loadUpdateConfig,
  openReleasePage,
  saveUpdateProxy,
  updateAction,
  updateStatusText,
  updatesSupported,
  validateProxyInput,
  watchUpdateState,
} from '../update.js';

type Pending = 'check' | 'download' | 'install' | 'proxy' | 'release' | null;
type Notice = { kind: 'info' | 'error'; text: string };
/** The standalone host answered the config read, or it never will. */
type Availability = { kind: 'unknown' } | { kind: 'hidden' } | { kind: 'ready'; config: UpdateConfig } | { kind: 'broken'; message: string };

/** A standalone build older than the updater has no command at all: stay hidden then. */
const isMissingCommand = (message: string) => /not found|unknown command|不存在/i.test(message);

/**
 * Update section of the standalone settings window.
 *
 * It owns its state instead of riding the rail-preference autosave, so a rejected
 * proxy cannot corrupt or block unrelated settings. Embedded hosts, the browser
 * and standalone builds without the updater commands render nothing at all.
 */
export function UpdateSettings() {
  const [availability, setAvailability] = React.useState<Availability>({kind: 'unknown'});
  const [snapshot, setSnapshot] = React.useState<UpdateSnapshot>(idleUpdateSnapshot);
  const [proxy, setProxy] = React.useState('');
  const [pending, setPending] = React.useState<Pending>(null);
  const [notice, setNotice] = React.useState<Notice | null>(null);

  React.useEffect(() => {
    if (!updatesSupported()) {
      setAvailability({kind: 'hidden'});
      return;
    }
    let disposed = false;
    const release = watchUpdateState(next => {
      if (!disposed) setSnapshot(next);
    });
    loadUpdateConfig()
      .then(config => {
        if (disposed) return;
        setAvailability({kind: 'ready', config});
        setProxy(config.proxy);
      })
      .catch(error => {
        if (disposed) return;
        const message = errorMessage(error);
        setAvailability(isMissingCommand(message) ? {kind: 'hidden'} : {kind: 'broken', message});
      });
    return () => {
      disposed = true;
      release();
    };
  }, []);

  const config = availability.kind === 'ready' ? availability.config : null;
  const phase = snapshot.phase;
  const busy = pending !== null;
  const primary = updateAction(snapshot);

  const run = async (action: Exclude<Pending, null>, task: () => Promise<unknown>) => {
    setPending(action);
    setNotice(null);
    try {
      await task();
    } catch (error) {
      setNotice({kind: 'error', text: errorMessage(error)});
    } finally {
      setPending(null);
    }
  };

  const saveProxy = async () => {
    const checked = validateProxyInput(proxy);
    if (!checked.ok) {
      setNotice({kind: 'error', text: checked.message});
      return;
    }
    await run('proxy', async () => {
      const saved = await saveUpdateProxy(checked.value);
      setAvailability({kind: 'ready', config: saved});
      setProxy(saved.proxy);
      setNotice({kind: 'info', text: saved.proxy ? '更新代理已保存' : '已关闭更新代理'});
    });
  };

  if (!config) {
    if (availability.kind !== 'broken') return null;
    return (
      <section aria-labelledby="updates" className="update-section">
        <h2 id="updates">更新</h2>
        <div className="settings-group update-panel">
          <p className="update-notice" data-error={true}>{availability.message}</p>
        </div>
      </section>
    );
  }

  const percent = snapshot.percent;
  // 复查失败但目标版本仍在：阶段保持 available，失败文案随快照到达，标识为错误态。
  const staleFailure = phase === 'available' && snapshot.message !== null;
  return (
    <section aria-labelledby="updates" className="update-section">
      <h2 id="updates">更新</h2>
      <div className="settings-group update-panel">
        <div className="row update-version">
          <span>
            <strong>当前版本 v{config.currentVersion}</strong>
            <small>自动检查公开发布的最新稳定版，不会自动安装或重启</small>
          </span>
          <Button
            type="button"
            variant="outline"
            data-action="update-release"
            disabled={busy}
            onClick={() => void run('release', openReleasePage)}
          >
            发布页
          </Button>
        </div>
        <div className="update-proxy">
          <Label htmlFor="update-proxy">
            <strong>更新代理</strong>
            <small>仅用于更新检查和安装包下载；留空表示不使用显式代理</small>
          </Label>
          <div className="update-proxy-row">
            <input
              id="update-proxy"
              data-field="updateProxy"
              value={proxy}
              onChange={event => setProxy(event.target.value)}
              placeholder="例如 http://127.0.0.1:7897"
              spellCheck={false}
              autoComplete="off"
              disabled={busy}
            />
            <Button type="button" variant="outline" data-action="update-save-proxy" disabled={busy} onClick={() => void saveProxy()}>
              {pending === 'proxy' ? '保存中…' : '保存'}
            </Button>
          </div>
        </div>
        <div className="update-status" data-phase={phase} data-error={staleFailure || undefined} role="status" aria-live="polite">
          <p>{updateStatusText(snapshot, config.currentVersion)}</p>
          {phase === 'downloading' && (
            <div
              className="update-progress"
              role="progressbar"
              aria-label="下载进度"
              aria-valuemin={0}
              aria-valuemax={100}
              {...(percent === null ? {} : {'aria-valuenow': percent})}
              data-indeterminate={percent === null}
            >
              <span style={percent === null ? undefined : {width: `${percent}%`}} />
            </div>
          )}
        </div>
        <div className="update-actions">
          {primary === 'download' && (
            <Button type="button" data-action="update-download" disabled={busy} onClick={() => void run('download', downloadUpdate)}>
              {pending === 'download' ? '下载中…' : `下载更新 v${snapshot.latest ?? ''}`}
            </Button>
          )}
          {primary === 'restart' && (
            <Button type="button" data-action="update-restart" disabled={busy} onClick={() => void run('install', installUpdate)}>
              {pending === 'install' ? '正在重启…' : `重启并安装 v${snapshot.latest ?? ''}`}
            </Button>
          )}
          <Button
            type="button"
            variant={primary === 'check' ? 'default' : 'outline'}
            data-action="update-check"
            disabled={busy || phase === 'checking' || phase === 'downloading'}
            onClick={() => void run('check', checkForUpdates)}
          >
            {phase === 'checking' ? '正在检查…' : '检查更新'}
          </Button>
        </div>
        {notice && <p className="update-notice" data-error={notice.kind === 'error'}>{notice.text}</p>}
      </div>
    </section>
  );
}
