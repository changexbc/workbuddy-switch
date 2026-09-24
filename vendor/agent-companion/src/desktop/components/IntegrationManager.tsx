import * as React from 'react';
import { AlertDialog, AlertDialogTrigger, AlertDialogContent, AlertDialogTitle, AlertDialogDescription, AlertDialogCancel, AlertDialogAction } from '@/components/ui/alert-dialog';
import { Collapsible, CollapsibleTrigger, CollapsibleContent } from '@/components/ui/collapsible';
import { Switch } from '@/components/ui/switch';
import type { SourceId } from '@/types/settings.js';
import { Button } from '@/components/ui/button';
import { openIntegrationFolder, requestIntegrations } from '../integrations.js';
import { agents } from '../listening.js';
import { errorMessage } from '@/types/commands.js';
import type { IntegrationAction, IntegrationStatus } from '@/types/integrations.js';

const labels: Record<IntegrationStatus['status'], string> = {
  installed: '已接入', not_installed: '未接入', partial: '需要修复',
  error: '接入失败', unavailable: '暂不可用', pending: '待处理',
};

export function IntegrationManager({disabled, acquire, release, enabled, onEnabledChange, refreshRevision = 0}: {
  refreshRevision?: number;
  enabled: Record<SourceId, boolean>; onEnabledChange: (source: SourceId, enabled: boolean) => void;
  disabled: boolean; acquire: () => boolean; release: () => void;
}) {
  const [sources, setSources] = React.useState<IntegrationStatus[]>([]);
  const [expandedSource, setExpandedSource] = React.useState<SourceId | null>(null);
  const [readFailed, setReadFailed] = React.useState(false);
  const [loading, setLoading] = React.useState(true);
  const [active, setActive] = React.useState<string | null>(null);
  const [confirmSource, setConfirmSource] = React.useState<SourceId | null>(null);
  const [message, setMessage] = React.useState('');
  const [openingLocation, setOpeningLocation] = React.useState<string | null>(null);
  const mounted = React.useRef(false);
  const running = React.useRef(false);
  const requestId = React.useRef(0);

  React.useEffect(() => {
    mounted.current = true;
    let current = true;
    const id = ++requestId.current;
    setLoading(true);
    requestIntegrations().then(value => { if (current && id === requestId.current) { setSources(value.sources); setReadFailed(false); setMessage(''); } })
      .catch(error => { if (current && id === requestId.current) { setReadFailed(true); setMessage(`读取失败：${errorMessage(error)}`); } })
      .finally(() => { if (current && id === requestId.current) setLoading(false); });
    return () => { current = false; mounted.current = false; };
  }, [refreshRevision]);

  async function run(action?: IntegrationAction) {
    if (running.current || !acquire()) return;
    running.current = true;
    ++requestId.current;
    setLoading(false);
    setActive(action ? `${action.source}-${action.action}` : 'refresh');
    setMessage('');
    try {
      const value = await requestIntegrations(action);
      if (mounted.current) {
        setSources(value.sources);
        setReadFailed(false);
        const item = action && value.sources.find(item => item.source === action.source);
        setMessage(item ? item.message : '状态已刷新');
      }
    } catch (error) {
      // A partial mutation may already be durable. Re-read without claiming success.
      const failure = errorMessage(error);
      if (action) {
        try {
          const value = await requestIntegrations();
          if (mounted.current) { setSources(value.sources); setReadFailed(false); }
        } catch {
          if (mounted.current) setReadFailed(true); // The retained snapshot is no longer verified.
        }
      }
      if (mounted.current) {
        if (!action) setReadFailed(true);
        setMessage(`操作失败：${failure}`);
      }
    } finally {
      running.current = false;
      release();
      if (mounted.current) setActive(null);
    }
  }

  async function openFolder(source: SourceId, location: string) {
    if (openingLocation !== null) return;
    setOpeningLocation(location);
    setMessage('');
    try {
      await openIntegrationFolder(source, location);
    } catch (error) {
      setMessage(`打开配置文件夹失败：${errorMessage(error)}`);
    } finally {
      if (mounted.current) setOpeningLocation(null);
    }
  }

  return <div className="integration-manager agent-card" aria-label="Agent 监听与接入" aria-busy={loading || active !== null}>
    <div className="integration-heading">
      <h2>Agent 监听与接入</h2>
      <Button className="integration-refresh" type="button" variant="outline" aria-label="刷新状态" disabled={disabled || loading || active !== null} onClick={() => void run()}>
        <svg viewBox="0 0 16 16" fill="none" aria-hidden="true"><path d="M13 6a5.2 5.2 0 1 0 .1 3M13 2.5V6H9.5" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" /></svg>
        刷新
      </Button>
    </div>
    <div className="settings-group">
    {agents.map(([source, name]) => {
      const item = sources.find(item => item.source === source);
      const webhook = source === 'codeg';
      const expanded = expandedSource === source;
      const status = loading ? '检查中' : readFailed ? '读取失败' : item ? labels[item.status] : '状态未知';
      return <Collapsible open={expanded} onOpenChange={open => setExpandedSource(open ? source : null)} className="integration-item" key={source} data-integration={source} data-expanded={expanded}>
        <div className="integration-title">
          <span className="integration-identity"><img src={`${import.meta.env.BASE_URL}icons/agents/${source}.png`} alt="" /><strong>{name}</strong></span>
          <span className="integration-badge" data-state={readFailed ? 'error' : loading ? 'loading' : item?.status}>{status}</span>
          <Switch data-field={`source-${source}`} aria-label={`监听 ${name}`} checked={enabled[source]} onCheckedChange={checked => onEnabledChange(source, checked)} />
          <CollapsibleTrigger asChild><Button variant="outline" className="integration-disclosure" type="button" aria-label={`${name} 接入详情`}><span className="integration-chevron" aria-hidden="true" /></Button></CollapsibleTrigger>
        </div>
        <CollapsibleContent className="integration-content">
          {item ? <div className="integration-detail-panel">
            {item.status !== 'installed' && <p className="integration-detail" data-state={item.status}>{item.message}</p>}
            <div className="integration-metadata">
              <div className="integration-event"><span>最近事件</span><span>{item.lastEventAt ? <time dateTime={new Date(item.lastEventAt).toISOString()}>{new Date(item.lastEventAt).toLocaleString('zh-CN', {year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false})}</time> : '尚无事件记录'}</span></div>
              {!!item.locations.length && <div className="integration-locations">
                <span className="integration-metadata-label">配置位置</span>
                <ul className="integration-location-list">{item.locations.map(location => <li key={location} className="integration-location-row">
                  <code title={location}>{location}</code>
                  <Button type="button" variant="outline" className="integration-open-folder" aria-label={`打开 ${location} 所在文件夹`} title="打开所在文件夹" disabled={disabled || active !== null || openingLocation !== null || readFailed || loading} onClick={() => void openFolder(source, location)}>
                    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true"><path d="M2.5 6.5V5a1.5 1.5 0 0 1 1.5-1.5h4l1.8 2h6.2A1.5 1.5 0 0 1 17.5 7v9A1.5 1.5 0 0 1 16 17.5H4A1.5 1.5 0 0 1 2.5 16V6.5Z" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round"/><path d="m10.5 13 4-4m-3.2 0h3.2v3.2" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round"/></svg>
                  </Button>
                </li>)}</ul>
              </div>}
            </div>
            <div className="integration-detail-footer">
            <p className="integration-policy">{webhook ? '关闭监听会注销 Webhook。' : '关闭监听会保留 Hooks。'}{item.automatic ? '卸载后不会自动安装。' : '已关闭自动接入。'}</p>
            <div className="integration-actions">
              <Button className="integration-repair" type="button" variant="outline" disabled={disabled || active !== null || loading} onClick={() => void run({source, action: 'install'})}>
                {active === `${source}-install` ? '正在处理…' : webhook ? (item.status === 'installed' ? '重新注册' : '注册 / 重试') : item.status === 'installed' || item.status === 'partial' ? '修复 Hooks' : '安装 Hooks'}
              </Button>
              <AlertDialog open={confirmSource === source} onOpenChange={open => { if (!running.current) setConfirmSource(open ? source : null); }}>
                <AlertDialogTrigger asChild>
                  <Button className="integration-remove" type="button" variant="outline" disabled={disabled || active !== null || loading || (!item.automatic && item.status === 'not_installed')}>{webhook ? '注销' : '卸载'}</Button>
                </AlertDialogTrigger>
                <AlertDialogContent onEscapeKeyDown={event => { event.stopPropagation(); if (running.current) event.preventDefault(); }}>
                  <AlertDialogTitle>{webhook ? `注销 ${name} Webhook？` : `卸载 ${name} Hooks？`}</AlertDialogTitle>
                  <AlertDialogDescription>此应用将停止通过{webhook ? ' Webhook' : ' Hooks'}上报新事件，且不会自动重新接入。之后可在这里重新{webhook ? '注册' : '安装'}。</AlertDialogDescription>
                  <div className="settings-dialog-actions">
                    <AlertDialogCancel disabled={active !== null}>取消</AlertDialogCancel>
                    <AlertDialogAction disabled={disabled || active !== null} onClick={event => {
                      event.preventDefault();
                      if (!running.current) void run({source, action: 'uninstall'}).then(() => setConfirmSource(null));
                    }}>{active === `${source}-uninstall` ? '正在处理…' : webhook ? '确认注销' : '确认卸载'}</AlertDialogAction>
                  </div>
                </AlertDialogContent>
              </AlertDialog>
            </div>
            </div>
          </div> : <p className="integration-detail">{loading ? '正在检查接入状态…' : '暂时无法获取接入信息，请刷新重试。监听开关仍可使用。'}</p>}
        </CollapsibleContent>
      </Collapsible>;
    })}
    </div>
    <p role="status" className="integration-message">{message}</p>
  </div>;
}
