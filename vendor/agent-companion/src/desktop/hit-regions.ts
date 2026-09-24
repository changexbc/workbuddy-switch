/**
 * The click-through contract with the native shell.
 *
 * The rail lives in a fully transparent WebView, so macOS sends no mouse events
 * to it until the host is told which rectangles are interactive. Rust applies
 * `set_hit_regions` to the window's input shape.
 *
 * Two things about the previous implementation drove this rewrite:
 *
 * 1. It discovered controls by walking `root.querySelectorAll`. That works only
 *    for nodes inside the rail's own subtree; anything mounted in a Portal on
 *    `document.body` is invisible to it and stays unclickable. Surfaces now
 *    *register* themselves, so where the DOM node lives no longer matters.
 * 2. It had no way to tell "the user cannot click anything right now" from
 *    "there is nothing to click". While the welcome animation blocks input the
 *    correct payload is an explicit empty list, which the old code had to
 *    special-case at the call site.
 *
 * Measurement is still a microtask-coalesced, JSON-deduplicated push: layout
 * changes arrive in bursts (a snapshot, a scroll, a resize), and each push is an
 * IPC round trip to the host.
 */
import { desktopCommand, isDesktop } from './host.js';

export interface HitRegion {
  x: number;
  y: number;
  width: number;
  height: number;
  cursor?: 'pointer' | 'grab';
}

export type SurfaceKind = 'surface' | 'control';

interface Registration {
  element: HTMLElement;
  cursor: 'pointer' | 'grab';
}

export interface HitRegionOptions {
  /** True while an overlay owns input and nothing underneath may be clicked. */
  isBlocked: () => boolean;
}

export function createHitRegions({ isBlocked }: HitRegionOptions) {
  const surfaces = new Map<string, Registration>();
  const controls = new Map<string, Registration>();
  let queued = false;
  let last = '';

  const remember = (map: Map<string, Registration>, id: string, element: HTMLElement | null, cursor: Registration['cursor']) => {
    if (element) map.set(id, { element, cursor });
    else map.delete(id);
  };

  /** A clickable region: the element's own box, no padding. */
  const regionOf = ({ element, cursor }: Registration): HitRegion => {
    const rect = element.getBoundingClientRect();
    return { x: rect.x, y: rect.y, width: rect.width, height: rect.height, cursor };
  };

  /** A surface: the element's box grown by 10px, so a near miss still lands. */
  const surfaceOf = ({ element }: Registration): HitRegion => {
    const rect = element.getBoundingClientRect();
    return { x: rect.x - 10, y: rect.y - 10, width: rect.width + 20, height: rect.height + 20 };
  };

  /**
   * A surface counts only while it occupies space. `hidden` is checked separately
   * because it is what the caller means, but a zero-sized box is the more
   * dangerous case: padding a 0x0 box by 10px produces a perfectly valid 20x20
   * region at a negative offset, which the host would happily accept as a
   * clickable hole in the corner of the window.
   */
  const occupied = (element: HTMLElement) => {
    if (element.hidden) return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };

  /**
   * A control is hittable only while it is actually painted and reachable: a
   * `hidden` ancestor, a departing ghost, a disabled button or a zero-sized box
   * must not claim a rectangle, or the transparent area around it would swallow
   * clicks meant for whatever is behind the rail.
   *
   * The zero-size check is not cosmetic. `set_hit_regions` rejects the *entire*
   * payload when any single region has a non-positive width or height, and the
   * caller swallows that rejection, so one empty box would silently freeze the
   * window's input shape on its previous value.
   */
  const hittable = (element: HTMLElement) => {
    if (element.hasAttribute('disabled') || element.closest('[hidden], .desktop-departing')) return false;
    if (!element.getClientRects().length) return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };

  function measure(): HitRegion[] {
    if (isBlocked()) return [];
    const regions: HitRegion[] = [];
    // Controls first: a button that sits on top of a surface must win the hit
    // test, and the host resolves overlaps in array order.
    for (const entry of controls.values()) if (hittable(entry.element)) regions.push(regionOf(entry));
    for (const entry of surfaces.values()) if (occupied(entry.element)) regions.push(surfaceOf(entry));
    return regions;
  }

  function flush() {
    const regions = measure();
    const key = JSON.stringify(regions);
    if (key === last) return;
    last = key;
    void desktopCommand('set_hit_regions', { regions }).catch(() => {});
  }

  /** Coalesced push. Safe to call from a layout effect, a ref or an event. */
  function sync() {
    if (!isDesktop() || queued) return;
    queued = true;
    queueMicrotask(() => { queued = false; flush(); });
  }

  return {
    register(id: string, kind: SurfaceKind, element: HTMLElement | null, cursor: Registration['cursor'] = 'pointer') {
      remember(kind === 'surface' ? surfaces : controls, id, element, cursor);
      sync();
    },
    unregister(id: string, kind: SurfaceKind) {
      remember(kind === 'surface' ? surfaces : controls, id, null, 'pointer');
      sync();
    },
    sync,
    /** Forces the next push even if the geometry is unchanged. */
    invalidate() { last = ''; },
  };
}

export type HitRegions = ReturnType<typeof createHitRegions>;
