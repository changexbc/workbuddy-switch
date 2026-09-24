import { AGENT_ICON_IDS } from '../../monitor/session-link.js';
import type { SessionPresentation } from '../../monitor/presentation.js';
import type { RailItem } from '../rail-model.js';

/**
 * The provider name shown next to the badge, which is not always the source:
 * Codeg and the CodeBuddy IDE report the agent they are running.
 */
export function providerLabel(item: RailItem, presentation: SessionPresentation) {
  const badge = presentation.badge;
  if (item.session.source === 'codeg' && badge?.id !== 'codeg') return `Codeg · ${badge?.label || presentation.provider}`;
  // A nested badge on the IDE source means the CodeBuddy hook reported another
  // host (the VS Code plugin), which the label names next to CodeBuddy.
  if (item.session.source === 'codebuddy-ide' && badge && badge.host !== badge.id) return `${presentation.provider} · ${badge.label}`;
  if (item.session.source === 'workbuddy' || item.session.source === 'codebuddy-ide') return badge?.label || presentation.provider;
  return presentation.provider;
}

/**
 * One or two icons, depending on whether the badge names a host *and* a nested
 * agent. Unknown ids fall back to their initial rather than a broken image.
 *
 * `hostOnly` is the single icon drawn on the rail avatar: `avatar` wins when a
 * badge names its own application, so a CodeBuddy VS Code plugin session wears
 * the plugin's mark instead of the IDE icon it shares a source with.
 */
export function ProviderIcons({item, presentation, hostOnly = false}: {item: RailItem; presentation: SessionPresentation; hostOnly?: boolean}) {
  const info = presentation.badge;
  const nested = Boolean(info?.host && info.host !== info.id);
  const ids = hostOnly
    ? [info?.avatar || info?.host || item.session.source]
    : nested ? [info!.host, info!.id] : [info?.id || item.session.source];
  return (
    <>
      {ids.map((id, index) => AGENT_ICON_IDS.includes(id)
        ? <img key={`${id}:${index}`} src={`${import.meta.env.BASE_URL}icons/agents/${id}.png`} alt="" data-agent={id} />
        : <span key={`${id}:${index}`} className="desktop-provider-fallback">{(info?.label || '?').slice(0, 1).toUpperCase()}</span>)}
    </>
  );
}
