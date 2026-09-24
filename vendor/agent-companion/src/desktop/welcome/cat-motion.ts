// The kitten gathers into one open stroke, whose tip traces the desktop rail.
// Analytic poses allow deterministic scrubbing. No per-frame physics integration or timers.

export type Point = [number, number];
export interface Rect { x: number; y: number; width: number; height: number }
export interface LineCatOptions { rail?: Rect; motionScale?: number }
export interface LineCatPose { gather: number; draw: number; points: Point[] }
export interface LineCatRenderer { render: (t: number, study?: boolean) => LineCatPose }

const TAU = Math.PI * 2;
const clamp = (x: number) => Math.max(0, Math.min(1, x));
const smooth = (x: number) => { x = clamp(x); return x * x * (3 - 2 * x); };
const ease = (x: number) => { x = clamp(x); return x * x * x * (x * (x * 6 - 15) + 10); };
const mix = (a: number, b: number, t: number) => a + (b - a) * t;
const f = (n: number) => Number(n.toFixed(2));
const lerp = (a: Point, b: Point, t: number): Point => [mix(a[0], b[0], t), mix(a[1], b[1], t)];
const N = 112;
const identity = (p: Point): Point => p;
type Project = (point: Point) => Point;

// Centripetal-looking local cubic construction, with short tangent handles at ears/paws.
function curve(points: Point[], closed = true, tightness = .13) {
  let d = `M${points[0].map(f).join(' ')}`;
  const count = points.length;
  for (let i = 0; i < count - (closed ? 0 : 1); i++) {
    const at = (n: number) => points[closed ? (n + count) % count : Math.max(0, Math.min(count - 1, n))];
    const a = at(i - 1), b = at(i), c = at(i + 1), e = at(i + 2);
    const c1 = [b[0] + (c[0] - a[0]) * tightness, b[1] + (c[1] - a[1]) * tightness];
    const c2 = [c[0] - (e[0] - b[0]) * tightness, c[1] - (e[1] - b[1]) * tightness];
    d += `C${[...c1, ...c2, ...c].map(f).join(' ')}`;
  }
  return d + (closed ? 'Z' : '');
}
function sampleContour(points: Point[], count = N): Point[] {
  // Sample the same named anatomical segments on every frame. No changing vertex order.
  const samples: Point[] = [];
  for (let n = 0; n < count; n++) {
    const u = n / count * points.length, i = Math.floor(u), t = u - i;
    const a = points[(i - 1 + points.length) % points.length], b = points[i];
    const c = points[(i + 1) % points.length], e = points[(i + 2) % points.length];
    samples.push([0, 1].map(k => {
      const c1 = b[k] + (c[k] - a[k]) * .13, c2 = c[k] - (e[k] - b[k]) * .13;
      return (1 - t) ** 3 * b[k] + 3 * (1 - t) ** 2 * t * c1 + 3 * (1 - t) * t * t * c2 + t ** 3 * c[k];
    }) as Point);
  }
  return samples;
}
function railPoint(u: number, rect: Rect = {x: 720, y: 181, width: 64, height: 251}): Point {
  // Clockwise capsule, starting at bottom-left: exact arcs and straight sides.
  const r = Math.min(rect.width / 2, rect.height / 2), straight = rect.height - 2 * r, length = 2 * straight + TAU * r;
  const left = rect.x, cx = left + r, top = rect.y + r, bottom = rect.y + rect.height - r;
  let s = ((u % 1) + 1) % 1 * length;
  if (s < straight) return [left, bottom - s]; s -= straight;
  if (s < Math.PI * r) { const a = Math.PI + s / r; return [cx + r * Math.cos(a), top + r * Math.sin(a)]; } s -= Math.PI * r;
  if (s < straight) return [left + rect.width, top + s]; s -= straight;
  const a = s / r; return [cx + r * Math.cos(a), bottom + r * Math.sin(a)];
}
function leadPoint(u: number, project: Project = identity): Point {
  // The line that the cat becomes. Its endpoint is exactly the start of the rail.
  return project([648 + 72 * u, 400 - 15 * Math.sin(Math.PI * u)]);
}
function releasePoint(tail: Point, u: number, project: Project = identity, scale = 1): Point {
  if (u >= .6) return leadPoint((u - .6) / .4, project);
  const v = u / .6, a = tail, b: Point = [tail[0] - 45 * scale, tail[1] + 10 * scale], c = project([578, 410]), d = leadPoint(0, project);
  return [0, 1].map(k => (1 - v) ** 3 * a[k] + 3 * (1 - v) ** 2 * v * b[k] + 3 * (1 - v) * v * v * c[k] + v ** 3 * d[k]) as Point;
}
function step(q: number, amplitude: number): Point {
  q = ((q % 1) + 1) % 1;
  // Flat, planted stance; eased forward swing with a distinct lifted paw.
  if (q < .6) return [(22 - 44 * q / .6) * amplitude, 0];
  const u = (q - .6) / .4;
  return [(-22 + 44 * smooth(u)) * amplitude, -22 * Math.sin(Math.PI * u) * amplitude];
}
function blink(t: number, at: number, width = .09) { return Math.max(.08, 1 - .96 * Math.exp(-(((t - at) / width) ** 4))); }

function pose(t: number, study: boolean) {
  const runStart = .95, runEnd = 4.22;
  const u = Math.max(0, t - runStart);
  const fade = smooth(u / .30) * (1 - smooth((t - runEnd) / .3));
  const amp = study ? .94 : fade;
  // Integrated acceleration and braking keep velocity continuous at departure/arrival.
  let distance;
  if (u < .3) { const q = u / .3; distance = 146 * .3 * (q ** 3 - .5 * q ** 4); }
  else if (t < 4.15) distance = 146 * (u - .15);
  else { const q = clamp((t - 4.15) / .35); distance = 146 * (3.05 + .35 * (q - q ** 3 + .5 * q ** 4)); }
  // Phase is tied to distance so planted paws travel backwards at the ground speed.
  const phase = study ? t / .54 : distance / (44 / .6 * .86);
  const bodyBob = -3.5 * Math.cos(phase * TAU * 2) * amp;
  const anticipation = Math.sin(Math.PI * clamp((t - .55) / .4)) * (t < .95 ? 1 : 0);
  const headLag = Math.sin(phase * TAU * 2 - .65) * 1.6 * amp;
  const tailLag = Math.sin(phase * TAU - .9) * amp;
  const headNod = study ? 0 : (Math.sin(Math.PI * clamp((t - .18) / .6)) * 4 * (t < .78 ? 1 : 0));
  const a = step(phase, amp), b = step(phase + .5, amp);
  const ear = Math.sin(phase * TAU - 1.1) * 5 * amp;
  const dx = study ? 0 : 150 + distance;
  const scale = study ? 1 : .86;
  const baseline = study ? 0 : 397;
  const bodyY = bodyBob + anticipation * 6;
  const headY = bodyY + headLag + headNod;
  const points: Point[] = [
    // Curled tail tip flows into the back and head.
    [-81, -106 + tailLag * 8], [-96, -112 + tailLag * 9], [-106, -96 + tailLag * 7],
    [-99, -76 + tailLag * 4], [-68, -67 + bodyY], [-58, -86 + bodyY], [-27, -93 + bodyY],
    [1, -89 + bodyY], [19, -98 + headY],
    [18 + ear * .3, -137 + headY], [36, -123 + headY], [52, -123 + headY],
    [73 - ear * .3, -144 + headY], [78, -110 + headY], [89, -94 + headY],
    [86, -72 + headY], [69, -59 + headY], [44, -62 + headY],
    // Near front leg: thin wire limb, rounded paw, then back to chest.
    [38, -43 + bodyY], [31 + a[0] * .6, -22 + a[1] * .7], [35 + a[0], -4 + a[1]],
    [47 + a[0], -2 + a[1]], [50 + a[0], 3 + a[1]], [31 + a[0], 4 + a[1]],
    [18 + a[0] * .45, -32 + bodyY], [-4, -38 + bodyY], [-25, -37 + bodyY],
    // Near rear leg and haunch.
    [-41 + b[0] * .35, -21 + b[1] * .5], [-45 + b[0], -4 + b[1]], [-32 + b[0], -2 + b[1]],
    [-30 + b[0], 3 + b[1]], [-51 + b[0], 4 + b[1]], [-62 + b[0] * .35, -26 + bodyY],
    [-68, -49 + bodyY], [-93, -57 + tailLag * 3], [-114, -73 + tailLag * 6],
    [-118, -98 + tailLag * 8], [-106, -120 + tailLag * 9], [-87, -122 + tailLag * 8], [-77, -112 + tailLag * 8],
  ];
  const transform: Project = ([x, y]) => [(x - anticipation * 5) * scale + dx, y * scale + baseline];
  return {points: sampleContour(points).map(transform), transform, phase, amp, headY, bodyY, dx, scale, baseline};
}

export function createLineCat(svg: SVGSVGElement, options: LineCatOptions = {}): LineCatRenderer {
  // The caller fills the scene, so a missing node means the overlay was emptied
  // underneath the animation. Say so instead of failing on a null dereference.
  const $ = (id: string) => {
    const node = svg.querySelector<SVGElement>('#' + id);
    if (!node) throw new Error(`welcome scene is missing #${id}`);
    return node;
  };
  const line = $('living-line'), far = $('far-legs'), eyes = $('eyes');
  const eyeLeft = $('eye-left'), eyeRight = $('eye-right');
  const spectrum = svg.querySelector<SVGLinearGradientElement>('#welcome-spectrum');
  const stops = spectrum ? Array.from(spectrum.querySelectorAll('stop')) : [];
  const palette = [42, 48, 65, 112, 225, 150, 76, 49, 42];
  const rect = options.rail || {x: 720, y: 181, width: 64, height: 251};
  const size = options.motionScale ?? 1;
  const base = rect.y + rect.height - Math.min(rect.width / 2, rect.height / 2);
  const project: Project = ([x, y]) => [rect.x + (x - 720) * size, base + (y - 400) * size];
  function render(t: number, study = false): LineCatPose {
    const p = pose(study ? t : Math.min(t, 4.5), study);
    if (!study) { const transform = p.transform; p.points = p.points.map(project); p.transform = point => project(transform(point)); p.scale *= size; }
    const gather = study ? 0 : ease((t - 4.5) / .62);
    const draw = study ? 0 : ease((t - 5.12) / 1.53);
    let current: Point[];
    if (study || t <= 5.12) {
      // Unthread the contour, rather than crumpling or scaling the cat into a blob.
      // The released end feeds a clean curved stroke outside the animal's silhouette.
      current = [];
      const eaten = smooth(gather / .74), feedStart = .6 * smooth((gather - .74) / .26);
      if (eaten < 1) {
        const at = eaten * N, index = Math.floor(at), fraction = at - index;
        current.push(lerp(p.points[index % N], p.points[(index + 1) % N], fraction));
        for (let i = index + 1; i < N; i++) current.push(p.points[i]);
        current.push(p.points[0]);
      }
      if (gather > 0) for (let i = 0; i <= 60; i++) current.push(releasePoint(p.points[0], mix(feedStart, gather, i / 60), project, size));
      line.setAttribute('d', curve(current, gather === 0, .16));
    } else {
      // The short stroke feeds the drawing tip. Only the visited perimeter is visible.
      // Once consumed, no loose tail remains outside the finished frame.
      const consumed = smooth(draw / .30);
      current = [];
      if (consumed < 1) for (let i = 0; i < 24; i++) current.push(leadPoint(mix(consumed, 1, i / 23), project));
      const count = Math.max(1, Math.ceil(draw * 180));
      for (let i = 0; i <= count; i++) current.push(railPoint(draw * i / count, rect));
      line.setAttribute('d', curve(current, false, .16));
    }
    // Use the same analytic clock as the silhouette: replay, interruption and
    // reduced-motion cleanup need no second animation or timer. Keep a nonzero
    // user-space gradient even when the cat contracts into a short drawing tip.
    if (spectrum) {
      const xs = current.map(point => point[0]), ys = current.map(point => point[1]);
      const left = Math.min(...xs), top = Math.min(...ys);
      const width = Math.max(60 * size, Math.max(...xs) - left);
      const height = Math.max(60 * size, Math.max(...ys) - top);
      spectrum.setAttribute('x1', String(left));
      spectrum.setAttribute('y1', String(top));
      spectrum.setAttribute('x2', String(left + width));
      spectrum.setAttribute('y2', String(top + height * .15));
      // Stay charcoal throughout the greeting/run. A narrow silver highlight
      // wakes up as the kitten slows down, just before unthreading.
      const flow = Math.max(0, t - 3.9) * 1.6;
      stops.forEach((stop, i) => {
        const color = smooth((t - 3.9 - i * .02) / .4);
        const phase = (i + flow) % (palette.length - 1);
        const index = Math.floor(phase);
        const flowing = mix(palette[index], palette[index + 1], smooth(phase - index));
        const silver = Math.round(mix(48, flowing, color));
        stop.setAttribute('stop-color', `rgb(${silver},${silver},${silver})`);
      });
      // Once the frame closes, give it one soft pulse before the overlay leaves.
      // Keep the silver highlight throughout; opacity alone hands over to the real rail.
      const pulse = Math.sin(Math.PI * smooth((t - 6.65) / .35));
      svg.style.setProperty('--welcome-glow', String(.2 * smooth((t - 3.9) / .56) + .3 * pulse));
      svg.style.setProperty('--welcome-halo', String(.32 * pulse));
      svg.style.setProperty('--welcome-glow-radius', `${2 + 3 * pulse}px`);
    }
    line.setAttribute('fill-opacity', gather === 0 ? '1' : '0');
    const farAlpha = (1 - smooth((t - 4.4) / .1));
    far.setAttribute('opacity', String(study ? .62 : farAlpha * .62));
    const f0 = step(p.phase + .5, p.amp), r0 = step(p.phase, p.amp);
    far.setAttribute('d', [
      // Closed silhouettes share the near legs' ankle/rounded paw construction.
      [[31, -52 + p.bodyY], [27 + f0[0] * .6, -22 + f0[1] * .7], [31 + f0[0], -4 + f0[1]],
        [43 + f0[0], -2 + f0[1]], [46 + f0[0], 3 + f0[1]], [27 + f0[0], 4 + f0[1]],
        [14 + f0[0] * .45, -32 + p.bodyY]],
      [[-45, -49 + p.bodyY], [-45 + r0[0] * .35, -21 + r0[1] * .5], [-49 + r0[0], -4 + r0[1]],
        [-36 + r0[0], -2 + r0[1]], [-34 + r0[0], 3 + r0[1]], [-55 + r0[0], 4 + r0[1]],
        [-66 + r0[0] * .35, -26 + p.bodyY]],
    ].map(pts => curve((pts as Point[]).map(p.transform), true, .13)).join(' '));
    const ex = p.transform([57, -93 + p.headY]);
    const eyelid = study ? blink(t, 1.2) : blink(t, .42) * blink(t, 4.3);
    eyes.setAttribute('transform', `translate(${f(ex[0])} ${f(ex[1])}) scale(${f(p.scale)})`);
    eyeLeft.setAttribute('transform', `translate(-8 0) scale(1 ${f(eyelid)})`);
    eyeRight.setAttribute('transform', `translate(8.8 0) scale(1 ${f(eyelid)})`);
    // The cat is gone once its outline feeds the pen. No head returns at the end.
    eyes.setAttribute('opacity', study ? '1' : String(f(1 - smooth((t - 4.5) / .28))));
    line.setAttribute('opacity', study ? '1' : String(f(1 - smooth((t - 6.78) / .22))));
    svg.setAttribute('data-time', String(f(t)));
    svg.setAttribute('data-gather', String(f(gather)));
    svg.setAttribute('data-draw', String(f(draw)));
    return {gather, draw, points: current};
  }
  return {render};
}
