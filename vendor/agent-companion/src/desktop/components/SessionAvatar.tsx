import { avatarIdentity, type AvatarStyle } from '../avatar.js';
import type { SessionPresentation } from '../../monitor/presentation.js';
import type { RailItem } from '../rail-model.js';
import type { RailController } from '../rail-controller.js';
import { AvatarPortrait } from './AvatarPortrait.js';
import { ProviderIcons, providerLabel } from './provider.js';
import { OPENED_HOLD_MS } from '../../monitor/session-model.js';

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
  const countingDown = !!item.openedUntil && item.openedUntil > Date.now();
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
      onPointerLeave={() => { controller.clearPointer(item.id); controller.leaveAvatar(item.id); }}
      onPointerMove={event => controller.pointerMove(item.id, {x: event.clientX, y: event.clientY})}
      onFocus={() => controller.focusAvatar(item.id)}
      onClick={() => controller.clickAvatar(item.id)}
      onContextMenu={event => {
        event.preventDefault();
        controller.openContextMenu(item.id, event.clientX, event.clientY);
      }}
    >
      <AvatarPortrait style={avatarStyle} slot={item.identity.slot} status={presentation.status} />
      <span className="desktop-source" title={label} aria-label={label}>
        <ProviderIcons item={item} presentation={presentation} hostOnly />
      </span>
      <i className={`desktop-dot${countingDown ? ' desktop-dot-countdown' : ''}`}>
        {countingDown && <svg className="desktop-countdown" viewBox="0 0 18 18" aria-hidden="true" focusable="false" key={`${item.session.roundId}:${item.openedUntil}`}>
          <circle className="desktop-countdown-track" cx="9" cy="9" r="7.5" />
          <circle className="desktop-countdown-progress" cx="9" cy="9" r="7.5" style={{animationDuration: `${OPENED_HOLD_MS}ms`}} />
        </svg>}
      </i>
    </button>
  );
}
