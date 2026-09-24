import * as React from 'react';
import { avatarBody, avatarParts, updateAvatar, type AvatarStyle } from '../avatar.js';

/**
 * The portrait.
 *
 * The wrapper `<svg>` and the constant anatomy belong to React; the eyelids do
 * not, because `updateAvatar` rewrites their `d` on every status change and the
 * transition into `running` depends on reading the *previous* state back off the
 * node. Declaring `data-state` or the lid paths in JSX would make React the
 * second writer of the same two attributes.
 *
 * `updateAvatar` is idempotent — it returns immediately when the state has not
 * changed — so running it after every render costs nothing and keeps the node
 * correct even when React has just replaced the markup for a new avatar style.
 */
export function AvatarPortrait({style, slot, status, initialStatus}: {style: AvatarStyle; slot: number; status: string; initialStatus?: string}) {
  const parts = React.useMemo(() => avatarParts(style, slot), [style, slot]);
  const node = React.useRef<SVGSVGElement | null>(null);
  const previousParts = React.useRef(parts);
  React.useLayoutEffect(() => {
    if (!node.current) return;
    // Replacing the inner markup creates blank eye paths while the outer SVG
    // retains its status marker. Invalidate that marker before the usual update.
    if (previousParts.current !== parts) delete node.current.dataset.state;
    previousParts.current = parts;
    if (!node.current.dataset.state && initialStatus) {
      updateAvatar(node.current, initialStatus);
      // Establish the starting eyelids before the CSS path transition to sleep.
      node.current.getBoundingClientRect();
    }
    updateAvatar(node.current, status);
  });
  return (
    <svg
      ref={node}
      className="companion-avatar desktop-portrait"
      viewBox="0 0 100 100"
      aria-hidden="true"
      data-character={parts.character}
      data-style={parts.style}
      style={parts.variables as React.CSSProperties}
      dangerouslySetInnerHTML={{__html: avatarBody(parts)}}
    />
  );
}
