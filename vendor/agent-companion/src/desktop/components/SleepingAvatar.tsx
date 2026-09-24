import * as React from 'react';
import type { RailController, RailState } from '../rail-controller.js';
import { AvatarPortrait } from './AvatarPortrait.js';
import '../sleeping-avatar.css';

export function SleepingAvatar({avatar, label, controller}: {
  avatar: RailState['restingAvatar']; label: string; controller: RailController;
}) {
  const attach = React.useCallback((element: HTMLDivElement | null) => controller.attach.resting(element), [controller]);
  return (
    <div className="desktop-empty desktop-sleeping" role="img" title={label} aria-label={label} ref={attach}>
      <AvatarPortrait style={avatar.style} slot={avatar.slot} status="sleep" initialStatus={avatar.fromStatus} />
      <span className="sleep-symbols" aria-hidden="true"><i>z</i><i>Z</i><i>Z</i></span>
    </div>
  );
}
