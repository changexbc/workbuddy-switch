import test from 'node:test';
import assert from 'node:assert/strict';
import {nextWorkGaze, workGazeDelay} from '../src/desktop/avatar-gaze.ts';

test('every gaze can choose every other direction, without a fixed successor', () => {
  for (let previous = 0; previous < 5; previous++) {
    const choices = new Set([0, .25, .5, .75].map(value => nextWorkGaze(previous, () => value).direction));
    assert.equal(choices.size, 4);
    assert(!choices.has(previous));
  }
});
test('enlargement is optional and bounded; neutral gaze restores natural eyes', () => {
  const enlarged = nextWorkGaze(0, () => 0);
  assert(enlarged.scaleX > 1 && enlarged.scaleY > 1);
  const plain = nextWorkGaze(0, () => .8);
  assert.equal(plain.scaleX, 1);
  assert.equal(plain.scaleY, 1);
  const neutral = nextWorkGaze(4, () => 0);
  assert.equal(neutral.x, 0);
  assert.equal(neutral.y, 0);
  assert.equal(neutral.scaleX, 1);
  for (const value of [0, .2, .44, .6, .99999]) {
    const pose = nextWorkGaze(0, () => value);
    assert(Math.abs(pose.x) <= 5 && Math.abs(pose.y) <= 4);
    assert(pose.scaleX >= 1 && pose.scaleX <= 1.4);
    assert(pose.scaleY >= 1 && pose.scaleY <= 1.55);
    assert(pose.transitionMs >= 650 && pose.transitionMs <= 1100);
    assert(workGazeDelay(() => value) >= 6500 && workGazeDelay(() => value) < 14500);
  }
  assert.notEqual(workGazeDelay(() => 0), workGazeDelay(() => .8));
});
