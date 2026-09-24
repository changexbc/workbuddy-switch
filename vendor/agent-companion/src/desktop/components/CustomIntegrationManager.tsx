import * as React from 'react';
import { Button } from '@/components/ui/button';
import { requestCustomIntegrations, requestCustomPreview } from '../custom-integrations.js';
import { errorMessage } from '@/types/commands.js';
import type { CustomAction, CustomIntegrations, CustomPreview } from '@/types/custom-integrations.js';

const TEMPLATE_BYTES = 1024 * 1024;
const outcomeLabels: Record<string, string> = {accepted: '已映射', ignored: '已忽略', rejected: '已拒绝'};

const time = (value: number | null) => value ? new Date(value).toLocaleString() : '尚无';

function ReadFailure({error}: {error: string}) {
  return <p className="integration-detail" data-state="error">{error}</p>;
}

/**
 * Importing and managing declarative custom agent hooks. Templates are data:
 * nothing here executes a template, installs a hook or edits another tool's
 * configuration, and the import preview never touches the live hub.
 */
export function CustomIntegrationManager({disabled, acquire, release}: {
  disabled: boolean; acquire: () => boolean; release: () => void;
}) {
  const [state, setState] = React.useState<CustomIntegrations | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [active, setActive] = React.useState<string | null>(null);
  const [message, setMessage] = React.useState('');
  const [file, setFile] = React.useState<{name: string; text: string} | null>(null);
  const [payloadText, setPayloadText] = React.useState('');
  const [preview, setPreview] = React.useState<CustomPreview | null>(null);
  const [previewError, setPreviewError] = React.useState('');
  const [copied, setCopied] = React.useState('');
  const mounted = React.useRef(false);
  const running = React.useRef(false);

  React.useEffect(() => {
    mounted.current = true;
    requestCustomIntegrations().then(value => { if (mounted.current) setState(value); })
      .catch(error => { if (mounted.current) setMessage(errorMessage(error)); })
      .finally(() => { if (mounted.current) setLoading(false); });
    return () => { mounted.current = false; };
  }, []);

  async function run(action?: CustomAction, note?: string) {
    if (running.current || !acquire()) return;
    running.current = true;
    setActive(action ? ('id' in action ? `${action.action}-${action.id}` : action.action) : 'refresh');
    setMessage('');
    try {
      const value = await requestCustomIntegrations(action);
      if (mounted.current) {
        setState(value);
        setMessage(note || (action ? '操作已生效' : '状态已刷新'));
        setCopied('');
      }
    } catch (error) {
      // A partial mutation may already be durable. Re-read without claiming success.
      const failure = errorMessage(error);
      if (action) {
        try { const value = await requestCustomIntegrations(); if (mounted.current) setState(value); } catch { /* retain last known snapshot */ }
      }
      if (mounted.current) setMessage(`${action?.action === 'import' ? '导入失败' : '操作失败'}：${failure}`);
    } finally {
      running.current = false;
      release();
      if (mounted.current) setActive(null);
    }
  }

  function parsedFile(): unknown | null {
    try { return JSON.parse(file!.text) as unknown; }
    catch { setMessage(`导入失败：${file?.name || '模板'} 不是有效 JSON`); return null; }
  }

  async function pickFile(event: React.ChangeEvent<HTMLInputElement>) {
    const selected = event.target.files?.[0];
    setPreview(null);
    setPreviewError('');
    if (!selected) { setFile(null); return; }
    if (selected.size > TEMPLATE_BYTES) {
      setFile(null);
      setMessage('导入失败：模板超过 1 MiB 上限');
      return;
    }
    setFile({name: selected.name, text: await selected.text()});
    setMessage('');
  }

  async function runPreview() {
    if (!file || running.current || !acquire()) return;
    const template = parsedFile();
    if (template === null) { release(); return; }
    running.current = true;
    setActive('preview');
    setMessage('');
    setPreview(null);
    setPreviewError('');
    try {
      let payload: unknown = {};
      if (payloadText.trim()) {
        try { payload = JSON.parse(payloadText) as unknown; }
        catch { setPreviewError('样例载荷不是有效 JSON'); return; }
      }
      const value = await requestCustomPreview(template, payload);
      if (mounted.current) setPreview(value);
    } catch (error) {
      if (mounted.current) setPreviewError(errorMessage(error));
    } finally {
      running.current = false;
      release();
      if (mounted.current) setActive(null);
    }
  }

  async function copyCommand(command: string, id: string) {
    try {
      await navigator.clipboard?.writeText(command);
      setCopied(id);
      setMessage('命令已复制，请自行添加到目标工具的 Hook 配置。');
    } catch {
      setMessage('无法访问剪贴板，请手动复制上方命令。');
    }
  }

  const disabledAll = disabled || active !== null;
  const templates = state?.templates || [];

  return <div className="custom-integration-manager" aria-label="自定义 Agent 接入" aria-busy={loading || active !== null}>
    <div className="integration-heading">
      <h2>自定义 Agent</h2>
      <Button type="button" variant="outline" disabled={disabledAll || loading} onClick={() => void run()}>刷新状态</Button>
    </div>
    <p className="section-hint">导入声明式 JSON 模板接入其他 Agent。应用只接收你手动配置的 Hook 事件，不执行模板中的任何代码，也不修改第三方工具的配置。</p>
    {state && !state.storage.ok && <ReadFailure error={state.storage.error || '自定义接入配置不可用'} />}
    {state && !state.binaryInstalled && <p className="integration-detail">尚未找到本机 Hook 可执行文件，请先启动一次桌面应用后再复制命令。</p>}
    {loading && <p className="section-hint">正在读取自定义接入…</p>}
    {!loading && <>
      <div className="custom-import" data-field="custom-import">
        <input
          type="file"
          accept=".json,application/json"
          aria-label="选择自定义接入模板"
          disabled={disabledAll}
          onChange={event => { void pickFile(event); }}
        />
        <label className="custom-payload">
          <span>样例载荷（可选，仅用于预览，不会创建会话）</span>
          <textarea
            rows={3}
            value={payloadText}
            disabled={disabledAll}
            placeholder='{"event_name":"prompt_submitted","session_id":"demo"}'
            onChange={event => setPayloadText(event.target.value)}
          />
        </label>
        <div className="integration-actions">
          <Button type="button" variant="outline" disabled={disabledAll || !file} onClick={() => void runPreview()}>
            {active === 'preview' ? '正在校验…' : '校验与预览'}
          </Button>
          <Button type="button" disabled={disabledAll || !file} onClick={() => {
            const template = parsedFile();
            if (template !== null) void run({action: 'import', template}, `已导入 ${file?.name || '模板'}`);
          }}>
            {active === 'import' ? '正在导入…' : '导入模板'}
          </Button>
        </div>
        {previewError && <p className="integration-detail" data-state="error">{previewError}</p>}
        {preview && <p className="integration-detail" data-state={preview.ok ? 'ok' : 'error'}>
          {preview.ok
            ? `预览成功：${preview.action || '已映射'}（${preview.events.length} 个事件，未创建会话）`
            : `预览未通过：${preview.reason}${preview.path ? ` · ${preview.path}` : ''}${preview.error ? ` · ${preview.error}` : ''}`}
        </p>}
      </div>
      <details className="integration-locations custom-setup" data-custom-setup="">
        <summary>如何配置到目标工具？</summary>
        <ol>
          <li>展开任意已导入来源，复制其中的「事件提交命令」，加入目标工具的 Hook / 命令配置（各工具格式不同，以该工具官方文档为准）。</li>
          <li>把该工具的事件 JSON 写到命令的标准输入；来源只由 <code>--integration</code> 参数决定，载荷里的 <code>source</code> 字段不会改变来源。</li>
          <li>触发一次真实事件，回到本页查看「最近收到事件 / 最近成功映射」；排障时展开下方「事件诊断」。</li>
          <li>命令标准输出始终为空：宿主需要 JSON 回包、或把非 0 退出码当故障时，请在目标工具侧包装（例如追加 <code>|| true</code>，或用输出 <code>{'{}'}</code> 的包装脚本）。</li>
          <li>停止使用时：先在此处删除来源，再到目标工具里手动移除这条 Hook。</li>
        </ol>
      </details>
      {templates.map(template => <details className="integration-item" key={template.id} data-custom={template.id}>
        <summary className="integration-title">
          <span className="integration-identity">
            <span className="custom-source-fallback" aria-hidden="true">{(template.name || template.id).slice(0, 1).toUpperCase()}</span>
            <span><strong>{template.name}</strong><small>{template.source}</small></span>
          </span>
          <span className="integration-badge" data-state={template.enabled ? 'installed' : 'pending'}>{template.enabled ? '已启用' : '已停用'}</span>
          <span className="integration-chevron" aria-hidden="true" />
        </summary>
        <div className="integration-content">
          <p className="integration-detail">已导入：{new Date(template.importedAt).toLocaleString()} · 声明能力：{template.capabilities.join('、') || '无'}</p>
          <p className="integration-detail">最近收到事件：{time(template.lastReceivedAt)} · 最近成功映射：{time(template.lastMappedAt)}</p>
          <details className="integration-locations">
            <summary>事件映射（{template.events.length}）</summary>
            {template.events.map(row => <code key={row.event}>{row.event} → {row.action}</code>)}
          </details>
          <p className="integration-policy">将下面这条命令添加到该工具的 Hook 配置；应用不会代你写入，删除来源后也请在该工具中手动移除。</p>
          <div className="custom-command">
            <code>{template.command}</code>
            <Button type="button" variant="outline" disabled={disabledAll} onClick={() => void copyCommand(template.command, template.id)}>
              {copied === template.id ? '已复制' : '复制命令'}
            </Button>
          </div>
          <div className="integration-actions">
            <Button type="button" variant="outline" disabled={disabledAll} onClick={() => void run({action: template.enabled ? 'disable' : 'enable', id: template.id}, template.enabled ? `已停用 ${template.name}` : `已启用 ${template.name}`)}>
              {active === `${template.enabled ? 'disable' : 'enable'}-${template.id}` ? '正在处理…' : template.enabled ? '停用' : '启用'}
            </Button>
            <Button type="button" variant="outline" disabled={disabledAll} onClick={() => void run({action: 'remove', id: template.id}, `已删除 ${template.name}`)}>
              {active === `remove-${template.id}` ? '正在处理…' : '删除'}
            </Button>
          </div>
        </div>
      </details>)}
      {!templates.length && state?.storage.ok && <p className="section-hint">还没有自定义来源，先导入一个 JSON 模板。</p>}
      {!!state?.diagnostics.length && <details className="integration-item" data-custom-diagnostics="">
        <summary className="integration-title">
          <span className="integration-identity"><span><strong>事件诊断</strong><small>最近的接受、忽略与拒绝记录</small></span></span>
          <span className="integration-badge">{state.diagnostics.length}</span>
          <span className="integration-chevron" aria-hidden="true" />
        </summary>
        <div className="integration-content">
          {[...state.diagnostics].reverse().map((item, index) => <p className="integration-detail" key={`${item.at}-${index}`} data-state={item.outcome}>
            {new Date(item.at).toLocaleTimeString()} · {item.source} · {item.event || '（无事件名）'} · {outcomeLabels[item.outcome] || item.outcome} · {item.reason}{item.detail ? ` · ${item.detail}` : ''}
          </p>)}
        </div>
      </details>}
    </>}
    <p className="integration-footnote">导入与启停立即生效，无需保存更改；预览不会创建会话。</p>
    <p role="status" className="integration-message">{message}</p>
  </div>;
}
