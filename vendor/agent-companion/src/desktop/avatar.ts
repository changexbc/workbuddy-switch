import './avatar.css';
import { nextWorkGaze, workGazeDelay } from './avatar-gaze.js';

// Nested groups keep ear pose, state follow-through and occasional twitches independent.
function ear(side: string, x: number, y: number, shape: string) {
  return `<g class="companion-ear companion-ear-${side}" style="--ear-x:${x}px;--ear-y:${y}px"><g class="companion-ear-settle"><g class="companion-ear-tip">${shape}</g></g></g>`;
}
const animals = [
  ear('left', 29, 42, '<path d="M19 45 Q12 9 26 15 L46 31 L40 51Z"/>') +
  ear('right', 73, 42, '<path d="M57 31 L78 15 Q89 10 83 46 L65 51Z"/>') +
  '<path d="M19 45 Q22 29 43 28 Q52 25 60 29 Q78 30 83 46 Q94 78 69 85 Q47 93 28 82 Q13 72 19 45Z"/>',
  '<circle cx="25" cy="30" r="15"/><circle cx="75" cy="30" r="15"/><rect x="15" y="24" width="70" height="63" rx="30"/>',
  '<path d="M16 45 L18 13 Q20 9 40 28 L60 28 Q80 8 82 13 L85 45 Q92 68 72 80 L50 91 L28 80 Q9 69 16 45Z"/>',
  ear('left', 34, 41, '<ellipse cx="34" cy="24" rx="10" ry="22"/>') +
  ear('right', 66, 41, '<ellipse cx="66" cy="24" rx="10" ry="22"/>') +
  '<rect x="18" y="32" width="64" height="56" rx="28"/>',
  '<ellipse cx="20" cy="48" rx="13" ry="28"/><ellipse cx="80" cy="48" rx="13" ry="28"/><rect x="23" y="24" width="54" height="62" rx="25"/>',
  '<circle cx="18" cy="36" r="17"/><circle cx="82" cy="36" r="17"/><rect x="22" y="23" width="56" height="63" rx="27"/>',
  '<path d="M23 40 L19 17 Q32 13 41 30 L60 30 Q69 13 82 17 L77 42 Q87 85 50 88 Q13 85 23 40Z"/>',
  '<ellipse cx="17" cy="50" rx="16" ry="25"/><ellipse cx="83" cy="50" rx="16" ry="25"/><rect x="25" y="22" width="50" height="56" rx="24"/><path d="M40 65 L40 85 Q40 98 56 93 L65 87 L59 79 Q51 87 52 78 L55 65Z"/>',
  '<path d="M17 18 L36 27 Q50 22 64 27 L83 18 L80 64 Q78 89 50 90 Q22 89 20 64Z"/><ellipse cx="37" cy="50" rx="15" ry="19" fill="#fff" opacity=".6"/><ellipse cx="63" cy="50" rx="15" ry="19" fill="#fff" opacity=".6"/>',
  '<circle cx="29" cy="24" r="15"/><circle cx="71" cy="24" r="15"/><rect x="15" y="24" width="70" height="63" rx="31"/><path d="M18 45 L30 50 L18 56 M82 45 L70 50 L82 56 M43 26 L50 38 L57 26" fill="none" stroke="#725b43" stroke-width="5" stroke-linecap="round"/>',
];
const animalNames = ['小猫', '小熊', '小狐狸', '小兔', '小狗', '考拉', '小猪', '大象', '猫头鹰', '小老虎'];
const bots = [
  '<path d="M26 18 Q47 9 64 18 L84 34 Q91 42 86 57 L77 78 Q73 87 60 88 L33 85 Q20 83 17 70 L12 44 Q10 31 26 18Z"/>',
  '<rect x="14" y="15" width="73" height="73" rx="25" transform="rotate(-7 50 50)"/>',
  '<path d="M50 10 C57 10 88 47 88 63 C88 96 12 96 12 63 C12 46 43 10 50 10Z"/>',
  '<path d="M50 30 C33 6 10 19 12 40 C14 59 35 78 46 87 Q50 91 54 87 C65 78 86 59 88 40 C90 19 67 6 50 30Z"/>',
  '<circle cx="50" cy="51" r="37"/>',
  '<path d="M43 17 Q50 5 57 17 L89 74 Q96 87 81 87 H19 Q4 87 11 74Z"/>',
  '<path d="M43 14 Q50 7 57 14 L86 43 Q94 51 86 59 L57 88 Q50 95 43 88 L14 59 Q6 51 14 43Z"/>',
  '<path d="M33 15 H67 Q72 15 75 20 L90 46 Q93 51 90 56 L75 82 Q72 87 67 87 H33 Q28 87 25 82 L10 56 Q7 51 10 46 L25 20 Q28 15 33 15Z"/>',
  '<path d="M46 13 Q50 5 54 13 L64 32 L85 36 Q95 38 88 46 L73 62 L75 84 Q76 94 67 89 L50 79 L33 89 Q24 94 25 84 L27 62 L12 46 Q5 38 15 36 L36 32Z"/>',
  '<rect x="10" y="24" width="80" height="55" rx="27.5" transform="rotate(-8 50 51)"/>',
];
const botNames = ['多边形伙伴', '方块伙伴', '水滴伙伴', '爱心伙伴', '圆球伙伴', '三角伙伴', '菱形伙伴', '六边形伙伴', '星星伙伴', '胶囊伙伴'];
export const BOT_AVATAR_COUNT = bots.length;
export type AvatarStyle = 'animal' | 'bot';
export type AvatarStatus = 'idle' | 'sleep' | 'running' | 'wait' | 'done' | 'error' | 'offline';

export function avatarIdentity(style: AvatarStyle, slot = 0) {
  const index = Number.isInteger(slot) && slot >= 0 ? slot : 0;
  const names = style === 'bot' ? botNames : animalNames;
  const variant = index % names.length;
  return {variant, name: names[variant]};
}
const colors = ['#b9cb91', '#ebbd8f', '#a8c8c4', '#d6b0b4', '#b8b9d5', '#d5c482', '#a4c2a2'];
const eyes: Record<AvatarStatus, string[]> = {
  sleep: ['M35 53 Q40 58 45 53', 'M56 53 Q61 58 66 53'],
  idle: ['M40 45 Q40 51 40 57', 'M61 45 Q61 51 61 57'],
  running: ['M39 49 Q41 52 42 57', 'M60 48 Q62 51 63 56'],
  wait: ['M40 44 Q40 50 40 56', 'M61 44 Q61 50 61 56'],
  done: ['M35 52 Q40 42 45 52', 'M56 52 Q61 42 66 52'],
  error: ['M36 46 Q40 48 44 49', 'M61 47 Q61 52 61 58'],
  offline: ['M35 54 Q40 57 45 54', 'M56 54 Q61 57 66 54'],
};

/**
 * Everything that distinguishes one portrait from another, without touching the
 * DOM. React renders the wrapper `<svg>` from these values; `createAvatar` below
 * builds the same element imperatively for the settings preview, which has no
 * React tree of its own to live in.
 */
export function avatarParts(style: AvatarStyle, slot = 0) {
  const index = Number.isInteger(slot) && slot >= 0 ? slot : 0;
  const variant = avatarIdentity(style, index).variant;
  const character = style !== 'bot' && variant === 0 ? 'cat' : style !== 'bot' && variant === 3 ? 'rabbit' : 'other';
  const resolved: AvatarStyle = style === 'bot' ? 'bot' : 'animal';
  return {
    index,
    character,
    style: resolved,
    color: colors[index % colors.length],
    shape: (style === 'bot' ? bots : animals)[variant],
    variables: {
      '--blink-time': `${5.3 + index % 5 * .73}s`,
      '--motion-delay': `${-(index * 1.37 % 7)}s`,
      '--ear-time': `${12.7 + index % 7 * 1.13}s`,
      '--wait-time': `${9 + index % 4}s`,
      '--work-time': `${4.8 + index % 5 * .37}s`,
      '--work-motion': variant === 3 ? 'companion-heart-work' : 'companion-bot-work',
    } as Record<string, string>,
  };
}

/**
 * The inside of the portrait. `shape` is a static constant from this module, so
 * React can inject it as markup; the eyelids stay real elements because their
 * `d` is rewritten on every status change by `updateAvatar`.
 */
export function avatarBody(parts: ReturnType<typeof avatarParts>) {
  const face = `<g class="companion-attention"><g fill="${parts.color}">${parts.shape}</g><g class="companion-pointer"><g class="companion-look"><g class="companion-lids"><path/><path/></g></g></g></g>`;
  // A separate group keeps working motion independent of acknowledgement and hover.
  return `<g class="companion-body">${parts.style === 'bot' ? `<g class="companion-work">${face}</g>` : face}</g>`;
}

export function createAvatar(style: AvatarStyle = 'animal', slot = 0) {
  const parts = avatarParts(style, slot);
  const template = document.createElement('template');
  template.innerHTML = `<svg class="companion-avatar desktop-portrait" viewBox="0 0 100 100" aria-hidden="true" data-character="${parts.character}" data-style="${parts.style}">${avatarBody(parts)}</svg>`;
  const node = template.content.firstElementChild as SVGSVGElement;
  for (const [name, value] of Object.entries(parts.variables)) node.style.setProperty(name, value);
  updateAvatar(node, 'idle');
  return node;
}
export function updateAvatar(node: SVGSVGElement, status: string) {
  const state: AvatarStatus = status === 'aborted' || status === 'unknown' ? 'offline' : (eyes as Record<string, string[]>)[status] ? status as AvatarStatus : 'idle';
  if (node.dataset.state === state) return;
  node.classList.toggle('companion-resuming', node.dataset.state === 'wait' && state === 'running');
  node.dataset.state = state;
  node.querySelectorAll('.companion-lids path').forEach((path, i) => path.setAttribute('d', eyes[state][i]));
}
export function pointAvatar(button: HTMLElement | null | undefined, point: {x: number; y: number} | null) {
  const node = button?.querySelector<SVGGElement>('.companion-pointer');
  if (!node) return;
  const avatar = button!.querySelector('.companion-avatar');
  avatar?.classList.toggle('companion-attentive', Boolean(point));
  const rect = button!.getBoundingClientRect();
  const x = point ? Math.max(-5, Math.min(5, (point.x - rect.x - rect.width / 2) / rect.width * 12)) : 0;
  const y = point ? Math.max(-3, Math.min(3, (point.y - rect.y - rect.height / 2) / rect.height * 8)) : 0;
  node.style.transform = `translate(${x}px,${y}px)`;
}
// CSS owns interpolation; one sparse timeout per visible working portrait picks targets.
export function observeAvatars(root: Element) {
  const media = matchMedia('(prefers-reduced-motion: reduce)');
  const portraits = new Map<SVGSVGElement, {timer?: ReturnType<typeof setTimeout>; direction: number}>();
  const clear = (state: {timer?: ReturnType<typeof setTimeout>}) => {
    if (state.timer !== undefined) clearTimeout(state.timer);
    state.timer = undefined;
  };
  const reset = (node: SVGSVGElement) => {
    for (const name of ['x', 'y', 'scale-x', 'scale-y', 'transition']) node.style.removeProperty(`--gaze-${name}`);
  };
  const active = (node: SVGSVGElement) => node.dataset.state === 'running'
    && node.classList.contains('companion-visible') && !document.hidden && !media.matches
    && !node.closest('.companion-motion-paused, .companion-system-paused')
    && !node.classList.contains('companion-attentive');
  const schedule = (node: SVGSVGElement, state: {timer?: ReturnType<typeof setTimeout>; direction: number}) => {
    state.timer = setTimeout(() => {
      state.timer = undefined;
      if (!root.contains(node) || !active(node)) { reconcile(); return; }
      // Native attention and DOM hover both take priority over autonomous glances.
      if (!node.closest('.desktop-avatar:hover')) {
        const pose = nextWorkGaze(state.direction);
        state.direction = pose.direction;
        node.style.setProperty('--gaze-x', `${pose.x}px`);
        node.style.setProperty('--gaze-y', `${pose.y}px`);
        node.style.setProperty('--gaze-scale-x', String(pose.scaleX));
        node.style.setProperty('--gaze-scale-y', String(pose.scaleY));
        node.style.setProperty('--gaze-transition', `${pose.transitionMs}ms`);
      }
      schedule(node, state);
    }, workGazeDelay());
  };
  const reconcile = () => {
    for (const [node, state] of portraits) {
      if (!root.contains(node)) { clear(state); observer.unobserve(node); portraits.delete(node); reset(node); continue; }
      if (!active(node)) { clear(state); reset(node); state.direction = 0; }
      else if (state.timer === undefined) schedule(node, state);
    }
  };
  const visibility = () => {
    root.classList.toggle('companion-system-paused', document.hidden || media.matches);
    reconcile();
  };
  const observer = new IntersectionObserver(entries => {
    entries.forEach(({target, isIntersecting}) => target.classList.toggle('companion-visible', isIntersecting));
    reconcile();
  });
  // Status, attention, settings, and React detachment all cancel pending work.
  const changes = new MutationObserver(reconcile);
  changes.observe(root, {subtree: true, childList: true, attributes: true, attributeFilter: ['class', 'data-state']});
  document.addEventListener('visibilitychange', visibility);
  media.addEventListener('change', visibility);
  visibility();
  return {
    observe: (node: Element | null | undefined) => {
      if (node instanceof SVGSVGElement && !portraits.has(node)) {
        portraits.set(node, {direction: 0});
        observer.observe(node);
        reconcile();
      }
    },
    unobserve: (node: Element | null | undefined) => {
      if (!(node instanceof SVGSVGElement)) return;
      const state = portraits.get(node);
      if (state) clear(state);
      portraits.delete(node);
      reset(node);
      observer.unobserve(node);
    },
    dispose() {
      changes.disconnect();
      observer.disconnect();
      for (const [node, state] of portraits) { clear(state); reset(node); }
      portraits.clear();
      document.removeEventListener('visibilitychange', visibility);
      media.removeEventListener('change', visibility);
    },
  };
}
