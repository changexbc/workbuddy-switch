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
  assert.equal(plain.scaleY, .76);
  const neutral = nextWorkGaze(4, () => 0);
  assert.equal(neutral.x, 0);
  assert.equal(neutral.y, 0);
  assert.equal(neutral.scaleX, 1);
  assert.equal(neutral.scaleY, 1);
  assert.equal(neutral.tiltDeg, 0);
  for (const value of [0, .2, .44, .6, .99999]) {
    const pose = nextWorkGaze(0, () => value);
    assert(Math.abs(pose.x) <= 5 && Math.abs(pose.y) <= 7);
    assert(pose.scaleX >= 1 && pose.scaleX <= 1.75);
    assert(pose.scaleY >= .76 && pose.scaleY <= 1.643);
    assert(pose.tiltDeg >= -5 && pose.tiltDeg <= 5);
    assert(pose.transitionMs >= 650 && pose.transitionMs <= 1100);
    assert(workGazeDelay(() => value) >= 6500 && workGazeDelay(() => value) < 14500);
  }
  assert.notEqual(workGazeDelay(() => 0), workGazeDelay(() => .8));
});
test('left and right glances tilt the eyes in opposite directions', () => {
  const poses = [0, .25, .5, .75].map(value => nextWorkGaze(0, () => value));
  for (const pose of poses) {
    assert.equal(pose.tiltDeg, pose.x < 0 ? pose.y < 0 ? 5 : 1 : pose.y < 0 ? -5 : -1);
    assert.equal(Math.sign(pose.y), pose.direction <= 2 ? -1 : 1);
  }
  assert(poses.some(pose => pose.tiltDeg > 0));
  assert(poses.some(pose => pose.tiltDeg < 0));
  assert(poses.some(pose => pose.tiltDeg === 5));
  assert(poses.some(pose => pose.tiltDeg === -5));
  assert(poses.some(pose => pose.tiltDeg === 1));
  assert(poses.some(pose => pose.tiltDeg === -1));
});
test('enlargement can happen on every upward and downward glance', () => {
  for (const choice of [0, .25, .5, .75]) {
    const gaze = enlarge => {
      const values = [choice, enlarge ? 0 : .9, .5, .5, .5, .5];
      return nextWorkGaze(0, () => values.shift() ?? .5);
    };
    const plain = gaze(false);
    const enlarged = gaze(true);
    assert.equal(enlarged.direction, plain.direction);
    assert(enlarged.scaleX > plain.scaleX);
    assert(enlarged.scaleY > plain.scaleY);
  }
});
