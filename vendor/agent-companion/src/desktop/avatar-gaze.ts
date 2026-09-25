const directions = [[0, 0], [-1, -1], [1, -1], [-1, 1], [1, 1]] as const;

/** Independent choices per portrait; never repeat the previous direction. */
export function nextWorkGaze(previous = 0, random = Math.random) {
  const direction = (previous + 1 + Math.floor(random() * (directions.length - 1))) % directions.length;
  const [dx, dy] = directions[direction];
  const enlarged = direction !== 0 && random() < .45;
  const scaleX = enlarged ? 1.4 + random() * .35 : 1;
  const verticalScale = dy > 0 ? .76 : dy < 0 ? 1.06 : 1;
  return {
    direction,
    x: dx * (3.2 + random() * 1),
    y: dy < 0 ? -(3.2 + random() * 1.1) : dy > 0 ? 5.8 + random() * 1.2 : 0,
    headY: dy > 0 ? 1.8 : dy < 0 ? -.8 : 0,
    headScaleY: dy > 0 ? .985 : 1,
    tiltDeg: dx === 0 ? 0 : dx * (dy > 0 ? -1 : -5),
    scaleX,
    scaleY: (enlarged ? 1.2 + random() * .35 : 1) * verticalScale,
    transitionMs: 650 + random() * 450,
  };
}

export function workGazeDelay(random = Math.random) {
  return 6500 + random() * 8000;
}
