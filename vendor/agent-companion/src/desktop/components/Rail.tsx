import * as React from 'react';
import { sessionPresentation } from '../../monitor/presentation.js';
import { openDesktopView } from '../host.js';
import type { AvatarStyle } from '../avatar.js';
import type { ConnectionState } from '../../types/snapshot.js';
import type { RailItem } from '../rail-model.js';
import type { DepartingGhost, RailController } from '../rail-controller.js';
import { AvatarPortrait } from './AvatarPortrait.js';
import { SleepingAvatar } from './SleepingAvatar.js';
import { SessionAvatar } from './SessionAvatar.js';
import { SessionCard } from './SessionCard.js';
import { ProviderIcons } from './provider.js';

interface Row { item: RailItem; presentation: ReturnType<typeof sessionPresentation> }

/**
 * The whole rail window.
 *
 * The layout effect with no dependency list is deliberate and is the only place
 * animations start: it runs after every commit, before paint, and compares what
 * is on screen now against what was measured at the end of the previous commit.
 * That is the same information the pre-migration `render()` had when it captured
 * `oldHeight` and `oldPositions` before mutating the DOM.
 */
export function RailApp({controller, container}: {controller: RailController; container: HTMLElement}) {
  const state = React.useSyncExternalStore(controller.subscribe, controller.getState);
  React.useLayoutEffect(() => { controller.afterCommit(); });

  // Escape and focus-loss live on the container rather than on a wrapper
  // element: a wrapper would become a direct child of the rail and would then be
  // hidden by `.welcome-blocking > :not(.desktop-welcome)`, taking the welcome
  // overlay with it.
  React.useEffect(() => {
    const onFocusOut = (event: FocusEvent) => { if (!container.contains(event.relatedTarget as Node | null)) controller.leaveAvatar(); };
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === 'Escape') controller.escape(); };
    const onPointerDown = (event: PointerEvent) => {
      if (event.button !== 2 && event.target instanceof Element && !event.target.closest('.desktop-context-menu')) controller.closeMenu();
    };
    container.addEventListener('focusout', onFocusOut);
    container.addEventListener('keydown', onKeyDown);
    document.addEventListener('pointerdown', onPointerDown);
    return () => {
      container.removeEventListener('focusout', onFocusOut);
      container.removeEventListener('keydown', onKeyDown);
      document.removeEventListener('pointerdown', onPointerDown);
    };
  }, [container, controller]);

  const rows: Row[] = state.items.map(item => ({item, presentation: sessionPresentation(item.session, item.offline ? 'offline' : state.connection)}));

  return (
    <>
      <section
        className={`desktop-strip${state.stripEmpty ? ' desktop-strip-empty' : ''}`}
        data-connection={state.connection}
        ref={element => controller.attach.strip(element)}
        onContextMenu={event => { event.preventDefault(); controller.openMenu(event.clientY); }}
      >
        <div
          className="desktop-grip"
          title="拖动会话栏"
          data-tauri-drag-region=""
          ref={element => controller.hitRegions.register('grip', 'control', element, 'grab')}
        >
          <span className="desktop-connection" role="status" data-connection={state.connection} title={state.connectionLabel} aria-label={state.connectionLabel} />
          <span className="desktop-grip-dots" />
        </div>
        <div
          className="desktop-list"
          aria-label="活跃会话"
          hidden={state.listHidden}
          ref={element => controller.attach.list(element)}
          onScroll={() => controller.onListScroll()}
        >
          {rows.map((row, index) => (
            <SessionAvatar
              key={row.item.id}
              item={row.item}
              presentation={row.presentation}
              avatarStyle={state.avatarStyle}
              hidden={!state.expanded && index >= state.visibleCount}
              controller={controller}
            />
          ))}
        </div>
        {!state.emptyHidden && <SleepingAvatar avatar={state.restingAvatar} label={state.emptyLabel} controller={controller} />}
        <button
          type="button"
          className="desktop-overflow"
          hidden={state.overflowHidden}
          aria-expanded={state.expanded}
          aria-label={state.overflowLabel}
          onClick={() => controller.toggleExpanded()}
          ref={element => controller.hitRegions.register('overflow', 'control', element)}
        >
          {state.overflowText}
        </button>
      </section>

      {state.card && (
        <section
          className="desktop-card"
          id="desktop-session-card"
          aria-label="会话信息"
          ref={element => controller.attach.card(element)}
          onPointerEnter={() => controller.holdCard()}
          onPointerLeave={() => controller.leaveAvatar()}
          onContextMenu={event => { event.preventDefault(); controller.openMenu(event.clientY); }}
        >
          {(() => {
            const row = rows.find(candidate => candidate.item.id === state.card!.id);
            return row && (
              <SessionCard item={row.item} connection={row.item.offline ? 'offline' : state.connection} avatarStyle={state.avatarStyle} surfaceKey="card" controller={controller} />
            );
          })()}
        </section>
      )}

      {state.notice && (
        <p className="desktop-notice" role="status" ref={element => controller.attach.notice(element)}>{state.notice.text}</p>
      )}

      <div
        className="desktop-context-menu"
        role="menu"
        hidden={!state.menuOpen}
        ref={element => controller.attach.menu(element)}
        onKeyDown={event => {
          if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
          event.preventDefault();
          const options = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('button')];
          const index = options.indexOf(document.activeElement as HTMLButtonElement);
          options[(index + (event.key === 'ArrowDown' ? 1 : options.length - 1)) % options.length].focus();
        }}
      >
        <button
          type="button"
          role="menuitem"
          className="desktop-menu-item"
          ref={element => controller.hitRegions.register('menu:settings', 'control', element)}
          onClick={() => { controller.closeMenu(); void openDesktopView('settings').catch(error => controller.reportError(error)); }}
        >
          悬浮窗设置
        </button>
        <button
          type="button"
          role="menuitem"
          className="desktop-menu-item"
          disabled={state.replayDisabled}
          ref={element => controller.hitRegions.register('menu:replay', 'control', element)}
          onClick={() => { controller.closeMenu(); controller.replayWelcome(); }}
        >
          重播小猫欢迎动画
        </button>
      </div>

      {state.automaticIds.map(id => {
        const row = rows.find(candidate => candidate.item.id === id);
        if (!row) return null;
        return (
          <AutomaticCard
            key={id}
            item={row.item}
            connection={state.connection}
            avatarStyle={state.avatarStyle}
            controller={controller}
          />
        );
      })}

      {state.departing.map(ghost => (
        <Ghost key={ghost.key} ghost={ghost} connection={state.connection} avatarStyle={state.avatarStyle} controller={controller} />
      ))}

      {state.welcome.running && (
        <svg className="desktop-welcome" aria-hidden="true" ref={element => controller.attach.welcome(element)} />
      )}
    </>
  );
}

function AutomaticCard({item, connection, avatarStyle, controller}: {
  item: RailItem;
  connection: ConnectionState;
  avatarStyle: AvatarStyle;
  controller: RailController;
}) {
  // An inline ref detaches on every commit, erasing the visibility baseline
  // even when this same reminder remains mounted.
  const attach = React.useCallback((element: HTMLElement | null) => {
    controller.attach.automatic(item.id, element);
  }, [controller, item.id]);
  return (
    <section
      className="desktop-card desktop-automatic-card"
      aria-label="任务提醒"
      ref={attach}
      onContextMenu={event => { event.preventDefault(); controller.openMenu(event.clientY); }}
    >
      <SessionCard item={item} connection={connection} avatarStyle={avatarStyle} surfaceKey={`automatic:${item.id}`} controller={controller} />
    </section>
  );
}

/**
 * A row that has already left the list, held in place for one animation.
 *
 * The pre-migration code cloned the live node; React cannot be cloned, so the
 * ghost is rendered from the item's last known data instead. That is why
 * `RailController` keeps `lastKnown` — by the time the ghost exists, the model
 * has already forgotten the session.
 */
function Ghost({ghost, connection, avatarStyle, controller}: {
  ghost: DepartingGhost;
  connection: ConnectionState;
  avatarStyle: AvatarStyle;
  controller: RailController;
}) {
  const node = React.useRef<HTMLElement | null>(null);
  // Mounted once and dropped from state when the animation ends; replaying the
  // fade on a re-render would restart it.
  React.useLayoutEffect(() => { controller.animateGhost(node.current, ghost.key); }, []);
  const style: React.CSSProperties = {
    position: 'absolute', left: ghost.style.left, top: ghost.style.top, right: 'auto',
    width: ghost.style.width, height: ghost.style.height, pointerEvents: 'none',
  };
  const presentation = sessionPresentation(ghost.item.session, ghost.item.offline ? 'offline' : connection);
  if (ghost.kind === 'avatar') {
    return (
      <button ref={node as React.RefObject<HTMLButtonElement>} type="button" className="desktop-avatar desktop-departing" data-status={presentation.status} aria-hidden="true" inert style={style}>
        <AvatarPortrait style={avatarStyle} slot={ghost.item.identity.slot} status={presentation.status} />
        <span className="desktop-source"><ProviderIcons item={ghost.item} presentation={presentation} hostOnly /></span>
        <i className="desktop-dot" />
      </button>
    );
  }
  return (
    <section ref={node} className="desktop-card desktop-automatic-card desktop-departing" aria-label="任务提醒" aria-hidden="true" inert style={style}>
      <SessionCard item={ghost.item} connection={connection} avatarStyle={avatarStyle} surfaceKey={`ghost:${ghost.key}`} controller={controller} />
    </section>
  );
}
