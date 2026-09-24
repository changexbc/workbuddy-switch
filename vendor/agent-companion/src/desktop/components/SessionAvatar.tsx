import { avatarIdentity, type AvatarStyle } from '../avatar.js';
import type { SessionPresentation } from '../../monitor/presentation.js';
import type { RailItem } from '../rail-model.js';
import type { RailController } from '../rail-controller.js';
import { AvatarPortrait } from './AvatarPortrait.js';
import { ProviderIcons, providerLabel } from './provider.js';

/**
 * One row of the rail. The portrait is rendered by React but the source badge is
 * not: `updateAvatar` owns `data-state` and the eyelids, and the rail reads
 * `data-status` back out of the DOM in the native QA report, so both stay as
 * attributes this component declares rather than values it recomputes.
 */
export function SessionAvatar({item, presentation, avatarStyle, hidden, controller}: {
  item: RailItem;
  presentation: SessionPresentation;
  avatarStyle: AvatarStyle;
  hidden: boolean;
  controller: RailController;
}) {
  const name = avatarIdentity(avatarStyle, item.identity.slot).name;
  const label = providerLabel(item, presentation);
  return (
    <button
      type="button"
      className="desktop-avatar"
      data-session-id={item.id}
      data-status={presentation.status}
      aria-controls="desktop-session-card"
      aria-label={`${name} · ${label} · ${presentation.statusLabel} · ${presentation.title}`}
      hidden={hidden}
      ref={element => controller.attach.avatar(item.id, element)}
      onPointerEnter={event => { if (event.pointerType !== 'touch') controller.hoverAvatar(item.id); }}
      onPointerLeave={() => { controller.clearPointer(item.id); controller.leaveAvatar(); }}
      onPointerMove={event => controller.pointerMove(item.id, {x: event.clientX, y: event.clientY})}
      onFocus={() => controller.focusAvatar(item.id)}
      onClick={() => controller.clickAvatar(item.id)}
    >
      <AvatarPortrait style={avatarStyle} slot={item.identity.slot} status={presentation.status} />
      <span className="desktop-source" title={label} aria-label={label}>
        <ProviderIcons item={item} presentation={presentation} hostOnly />
      </span>
      <i className="desktop-dot" />
    </button>
  );
}
