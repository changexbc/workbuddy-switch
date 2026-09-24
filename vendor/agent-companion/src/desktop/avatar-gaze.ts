const directions = [[0, 0], [-1, -1], [1, -1], [-1, 1], [1, 1]] as const;

/** Independent choices per portrait; never repeat the previous direction. */
export function nextWorkGaze(previous = 0, random = Math.random) {
  const direction = (previous + 1 + Math.floor(random() * (directions.length - 1))) % directions.length;
  const [dx, dy] = directions[direction];
  const enlarged = direction !== 0 && random() < .45;
  return {
    direction,
    x: dx * (3 + random() * 2),
    y: dy * (2.5 + random() * 1.5),
    scaleX: enlarged ? 1.15 + random() * .25 : 1,
    scaleY: enlarged ? 1.2 + random() * .35 : 1,
    transitionMs: 650 + random() * 450,
  };
}

export function workGazeDelay(random = Math.random) {
  return 6500 + random() * 8000;
}
