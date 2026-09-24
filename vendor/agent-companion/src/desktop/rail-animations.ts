/**
 * The rail's motion, as Web Animations calls on nodes React already rendered.
 *
 * The ownership split this file exists to enforce:
 *
 *   React owns  element existence, `className`, `data-*`, children and text.
 *   This file   owns `element.animate(...)` and, for the surfaces it positions
 *               (`hidden` and `style.top` on the card / automatic cards /
 *               notice), the two attributes that placement is made of.
 *
 * Neither side ever writes a property the other declares. React never receives
 * `style` or `hidden` for a positioned surface, so a re-render cannot undo a
 * placement or restart an animation; and nothing here writes `className`,
 * children or text, so an animation cannot undo a render.
 *
 * Every frame is produced by the compositor from an existing keyframe list.
 * There is no per-frame `setState` anywhere in the rail.
 */

export const reducedMotion = () => matchMedia('(prefers-reduced-motion: reduce)').matches;

const entrance = [
  { opacity: 0, transform: 'translateX(10px) scale(.94)' },
  { opacity: 1, transform: 'translateX(-2px) scale(1.012)', offset: .75 },
  { opacity: 1, transform: 'translateX(0) scale(1)' },
];

/**
 * Shows a surface and, if it was not already visible, plays it in.
 *
 * `wasVisible` is passed in rather than read back from `surface.hidden`, because
 * React mounts and unmounts these elements: a freshly mounted node is never
 * `hidden`, so reading the attribute would silently skip the entrance animation
 * every time a card appeared.
 *
 * The pre-migration code only cancelled a stale exit animation when a card
 * happened to be open. That guard was redundant: the only animation that can be
 * filling forwards on a *visible* surface is an exit that was interrupted, and
 * that can only happen here.
 */
export function revealSurface(surface: HTMLElement, wasVisible: boolean) {
  if (wasVisible) surface.getAnimations().filter(animation => animation.effect?.getTiming().fill === 'forwards').forEach(animation => animation.cancel());
  surface.hidden = false;
  if (!wasVisible && !reducedMotion()) surface.animate(entrance, { duration: 260, easing: 'cubic-bezier(.22,1,.36,1)' });
}

/**
 * Plays a surface out and hides it. `onDone` runs after the element is already
 * hidden, so the caller can drop it from React state without a visible frame at
 * full opacity in between.
 */
export function leaveSurface(surface: HTMLElement, onDone: () => void) {
  surface.getAnimations().forEach(animation => animation.cancel());
  if (surface.hidden || reducedMotion()) { surface.hidden = true; onDone(); return; }
  const animation = surface.animate(
    [{ opacity: 1, transform: 'translateX(0) scale(1)' }, { opacity: 0, transform: 'translateX(7px) scale(.97)' }],
    { duration: 130, easing: 'ease-in', fill: 'forwards' },
  );
  animation.onfinish = () => { surface.hidden = true; animation.cancel(); onDone(); };
}

/** Fades out a node that is already absolutely positioned where the original was. */
export function retireGhost(ghost: HTMLElement, onDone: () => void) {
  if (reducedMotion()) { onDone(); return; }
  const animation = ghost.animate(
    [{ opacity: 1, transform: 'scale(1)' }, { opacity: 0, transform: 'translateX(8px) scale(.75)' }],
    { duration: 200, easing: 'ease-in', fill: 'forwards' },
  );
  animation.onfinish = () => { animation.cancel(); onDone(); };
}

/**
 * Animates the rail from the height it had before this commit to the one it has
 * now. The caller measures; this only decides whether the change is worth a
 * transition.
 */
export function animateHeight(element: HTMLElement, previous: number, next: number) {
  if (reducedMotion() || Math.abs(next - previous) <= 1) return null;
  return element.animate([{ height: `${previous}px` }, { height: `${next}px` }], { duration: 300, easing: 'cubic-bezier(.22,1,.36,1)' });
}

/** FLIP: move an element back to where it was, then let it settle. */
export function animateReorder(element: HTMLElement, delta: number) {
  if (reducedMotion() || Math.abs(delta) <= 1) return;
  element.animate([{ transform: `translateY(${delta}px)` }, { transform: 'translateY(0)' }], { duration: 300, easing: 'cubic-bezier(.22,1,.36,1)' });
}

/** A row that was not on screen before pops in. */
export function animateArrival(element: HTMLElement) {
  if (reducedMotion()) return;
  element.animate([{ opacity: 0, transform: 'scale(.65)' }, { opacity: 1, transform: 'scale(1)' }], { duration: 280, easing: 'cubic-bezier(.2,1.4,.4,1)' });
}

/**
 * Cancels any animation still holding the rail's height, so a measurement taken
 * afterwards is the real layout height rather than a frame of the last one.
 */
export function cancelHeightAnimations(element: HTMLElement) {
  element.getAnimations().forEach(animation => animation.cancel());
}
