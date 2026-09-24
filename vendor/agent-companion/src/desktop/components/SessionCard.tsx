import { avatarIdentity, type AvatarStyle } from '../avatar.js';
import { sessionPresentation } from '../../monitor/presentation.js';
import type { ConnectionState } from '../../types/snapshot.js';
import type { RailItem } from '../rail-model.js';
import type { RailController } from '../rail-controller.js';
import { ProviderIcons, providerLabel } from './provider.js';

const DISMISSABLE = new Set(['wait', 'done', 'error', 'aborted']);

/**
 * The body shared by the hover card and the automatic question card. The two
 * differ only in which connection state they are told about, which the caller
 * resolves — the automatic card ignores a session's own offline flag, exactly as
 * before.
 */
export function SessionCard({item, connection, avatarStyle, surfaceKey, controller}: {
  item: RailItem;
  connection: ConnectionState;
  avatarStyle: AvatarStyle;
  surfaceKey: string;
  controller: RailController;
}) {
  const presentation = sessionPresentation(item.session, connection);
  const text = presentation.question || presentation.title;
  const label = providerLabel(item, presentation);
  const badge = presentation.badge;
  const badgeLabel = badge && badge.host !== badge.id ? badge.label : label;
  const dismissTitle = item.session.status === 'wait' ? '关闭本次待确认提示' : '收起已完成任务';
  return (
    <div className="desktop-card-body">
      <div className="desktop-card-head">
        <strong>{avatarIdentity(avatarStyle, item.identity.slot).name}</strong>
        <span className="desktop-card-status" data-status={presentation.status}>{presentation.statusLabel}</span>
        <span className="desktop-provider" title={label} aria-label={label}>
          <ProviderIcons item={item} presentation={presentation} />
          <span>{badgeLabel}</span>
        </span>
      </div>
      <button
        type="button"
        className="desktop-preview"
        disabled={!presentation.url}
        aria-label={presentation.url ? `${presentation.action}：${text}` : undefined}
        onClick={() => controller.clickAvatar(item.id)}
        ref={element => controller.hitRegions.register(`${surfaceKey}:preview`, 'control', element)}
      >
        <span className="desktop-preview-text" title={text}>{text}</span>
        {presentation.url && <span className="desktop-chevron">›</span>}
      </button>
      {DISMISSABLE.has(item.session.status) && (
        <button
          type="button"
          className="desktop-dismiss"
          title={dismissTitle}
          aria-label={dismissTitle}
          onClick={() => controller.dismiss(item.id)}
          ref={element => controller.hitRegions.register(`${surfaceKey}:dismiss`, 'control', element)}
        />
      )}
    </div>
  );
}
