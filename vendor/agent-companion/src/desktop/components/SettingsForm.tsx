import * as React from 'react';
import { IntegrationManager } from './IntegrationManager.js';
import { UpdateSettings } from './UpdateSettings.js';
import { BOT_AVATAR_COUNT, createAvatar } from '../avatar.js';
import { desktopCommand, isDesktop } from '../host.js';
import { agents, loadListening, saveListening } from '../listening.js';
import { defaultPreferences, loadPreferences, savePreferencePatch } from '../preferences.js';
import { Button } from '@/components/ui/button';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Switch } from '@/components/ui/switch';
import { errorMessage } from '@/types/commands.js';
import type { AvatarStyle, RailPreferencesState, SourceConfig, SourceId } from '@/types/settings.js';

import { createSettingsAutosave, type PreferencePatch, type SaveState } from '../settings-autosave.js';

const counts = Array.from({length: 14}, (_, index) => index + 3);

const styles: readonly (readonly [AvatarStyle, string, string])[] = [
  ['animal', '小动物', '10 种动物伙伴'],
  ['bot', '几何伙伴', `${BOT_AVATAR_COUNT} 种简洁造型`],
];

const allEnabled = () => Object.fromEntries(agents.map(([id]) => [id, true])) as Record<SourceId, boolean>;

const enabledOf = (sources: Record<SourceId, SourceConfig>) =>
  Object.fromEntries(agents.map(([id]) => [id, sources[id]?.enabled !== false])) as Record<SourceId, boolean>;

type Values = RailPreferencesState & { enabled: Record<SourceId, boolean> };

// Placeholder values stay hidden until the first read has settled.
const initialEnabled = allEnabled();
const initialValues: Values = {...defaultPreferences(), animation: false, autostart: false, autostartSupported: true, autostartManaged: true, enabled: initialEnabled};

/**
 * Reproduces the old `.styles button` rules, including the 1px inset that the
 * pressed state applies so the card does not resize when its border thickens.
 */
const styleCard = [
  'block whitespace-normal cursor-pointer rounded-lg border border-line bg-secondary px-2 py-[14px] text-center text-inherit',
  'transition-colors hover:border-soft hover:bg-accent',
  'focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring',
  'aria-pressed:border-2 aria-pressed:border-pressed aria-pressed:bg-muted aria-pressed:px-[7px] aria-pressed:py-[13px]',
].join(' ');

/** `avatar.js` builds its SVG imperatively, so the previews are appended by hand. */
function StylePreview({ style }: { style: AvatarStyle }) {
  const host = React.useRef<HTMLSpanElement>(null);

  React.useEffect(() => {
    const element = host.current;
    if (!element) return;
    for (const slot of style === 'bot' ? [0, 3, 8] : [0, 1, 2]) element.append(createAvatar(style, slot) as Element);
    return () => element.replaceChildren();
  }, [style]);

  return <span ref={host} className="portraits" />;
}

interface ToggleRowProps {
  title: string;
  hint: string;
  hintId?: string;
  checked: boolean;
  disabled?: boolean;
  field: string;
  onChange: (checked: boolean) => void;
}

/**
 * The whole row is the label, as it was before the migration, so the gap between
 * the text and the switch stays clickable. A label wrapping a button forwards its
 * activation to it, and a click that lands on the button itself is not forwarded
 * again, so the switch never toggles twice.
 */
function ToggleRow({title, hint, hintId, checked, disabled, field, onChange}: ToggleRowProps) {
  return (
    <Label className="row">
      <span>
        <strong>{title}</strong>
        <small id={hintId}>{hint}</small>
      </span>
      <Switch data-field={field} checked={checked} disabled={disabled} onCheckedChange={onChange} />
    </Label>
  );
}

export function SettingsForm() {
  const [values, setValues] = React.useState<Values>(initialValues);
  const [ready, setReady] = React.useState(false);
  const [failed, setFailed] = React.useState(false);
  const [saveState, setSaveState] = React.useState<SaveState>({busy: false, error: null, pending: false});
  const [integrationRevision, setIntegrationRevision] = React.useState(0);
  const [autosave] = React.useState(() => createSettingsAutosave({
    preferences: savePreferencePatch, listening: saveListening, changed: setSaveState,
    listeningSaved: () => setIntegrationRevision(revision => revision + 1),
  }));
  const busy = saveState.busy;
  const [status, setStatus] = React.useState('正在读取设置…');
  // An older configuration read must never settle over a newer retry.
  const readId = React.useRef(0);
  const loading = !ready && !failed;

  React.useLayoutEffect(() => {
    // This bootstrap shell lives outside the React root. Keep it across module
    // startup and data loading, and show it again for an explicit retry.
    const shell = document.getElementById('settings-loading');
    if (shell) shell.hidden = !loading;
  }, [loading]);

  const edit = (patch: PreferencePatch) => {
    setValues(current => ({...current, ...patch}));
    autosave.editPreferences(patch);
  };

  const load = React.useCallback(async () => {
    const id = ++readId.current;
    setFailed(false);
    setStatus('正在读取设置…');
    try {
      const [preferences, sources] = await Promise.all([loadPreferences(), loadListening()]);
      if (id !== readId.current) return;
      const enabled = enabledOf(sources);
      setValues({...preferences, enabled});
      setReady(true);
      setFailed(false);
      setStatus('设置保存在本机');
    } catch (error) {
      if (id !== readId.current) return;
      setReady(false);
      setFailed(true);
      setStatus(`读取失败：${errorMessage(error)}`);
    }
  }, []);

  React.useEffect(() => { load().catch(() => {}); }, [load]);

  React.useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !event.defaultPrevented && !busy && isDesktop()) desktopCommand('close_settings').catch(() => {});
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [busy]);

  return (
    <main className="rail-settings settings-form" hidden={loading}>
      <header>
        <span className="eyebrow">AGENT COMPANION</span>
        <h1>悬浮窗设置</h1>
        <p>让桌面上的小伙伴，按你的习惯陪伴。</p>
      </header>
      <form onSubmit={event => event.preventDefault()}>
        <fieldset disabled={!ready}>
          <section aria-labelledby="appearance">
            <h2 id="appearance">小伙伴的模样</h2>
            <div className="styles" role="group" aria-label="头像风格">
              {styles.map(([style, title, hint]) => (
                <Button
                  key={style}
                  type="button"
                  data-style={style}
                  aria-pressed={values.avatarStyle === style}
                  className={styleCard}
                  onClick={() => edit({avatarStyle: style})}
                >
                  <StylePreview style={style} />
                  <strong>{title}</strong>
                  <small>{hint}</small>
                </Button>
              ))}
            </div>
          </section>
          <section aria-labelledby="display">
            <h2 id="display">显示</h2>
            <div className="settings-group">
            <div className="row">
              <Label htmlFor="rail-size">
                <strong>悬浮窗尺寸</strong>
                <small>头像和提示卡片一起调整</small>
              </Label>
              <Select value={values.size} onValueChange={size => edit({size: size as RailPreferencesState['size']})}>
                <SelectTrigger id="rail-size" data-field="size" aria-label="悬浮窗尺寸"><SelectValue /></SelectTrigger>
                <SelectContent onEscapeKeyDown={event => event.stopPropagation()}>
                  <SelectItem value="small">小号</SelectItem>
                  <SelectItem value="medium">中号</SelectItem>
                  <SelectItem value="standard">标准</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <ToggleRow
              field="animation"
              title="头像动画"
              hint="眨眼、转头与轻轻摇摆"
              checked={values.animation}
              onChange={animation => edit({animation})}
            />
            <div className="row">
              <Label htmlFor="visible-count">
                <strong>默认显示数量</strong>
                <small>更多会话收起在展开按钮中</small>
              </Label>
              <Select
                disabled={!ready}
                value={String(values.visibleCount)}
                onValueChange={value => edit({visibleCount: Number(value)})}
              >
                <SelectTrigger id="visible-count" data-field="visibleCount" aria-label="默认显示数量">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent onEscapeKeyDown={event => event.stopPropagation()}>
                  {counts.map(count => <SelectItem key={count} value={String(count)}>{count} 个</SelectItem>)}
                </SelectContent>
              </Select>
            </div>
            </div>
          </section>
          {ready && <section><IntegrationManager
            disabled={busy || saveState.pending} acquire={autosave.acquire} release={autosave.release} refreshRevision={integrationRevision}
            enabled={values.enabled}
            onEnabledChange={(source, enabled) => {
              setValues(current => ({...current, enabled: {...current.enabled, [source]: enabled}}));
              autosave.editListening({[source]: enabled});
            }}
          /></section>}
          {/* 入口暂未开放（见文件头说明）：如需临时启用，恢复下面一行。
          {ready && <section><CustomIntegrationManager disabled={busy || saveState.pending} acquire={autosave.acquire} release={autosave.release} /></section>}
          */}
          {values.autostartManaged && <section>
            <h2>启动</h2>
            <div className="settings-group">
            <ToggleRow
              field="autostart"
              title="开机自启"
              hint={values.autostartSupported ? '登录电脑后自动显示悬浮窗' : '请在独立桌面应用中设置'}
              hintId="login-hint"
              checked={values.autostart}
              disabled={!values.autostartSupported}
              onChange={autostart => edit({autostart})}
            />
            </div>
          </section>}
        </fieldset>
      </form>
      {/* 独立版的应用更新；嵌入宿主、浏览器与旧版独立应用不渲染这一块。 */}
      <UpdateSettings />
      <div className="settings-save-feedback" data-error={!!saveState.error}>
        <p role="status" id="save-status">{!ready ? status : saveState.error ? `部分更改可能已生效，请重试：${errorMessage(saveState.error)}` : busy || saveState.pending ? '正在应用…' : '更改实时生效，保存在本机'}</p>
        {!!saveState.error && <Button type="button" variant="outline" data-action="retry-save" onClick={autosave.retry}>重试保存</Button>}
      </div>
      {failed && (
        <Button type="button" variant="outline" data-action="retry" onClick={() => { load().catch(() => {}); }}>重新读取</Button>
      )}
    </main>
  );
}
