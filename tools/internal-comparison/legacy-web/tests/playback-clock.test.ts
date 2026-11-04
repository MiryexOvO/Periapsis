import test from "node:test";
import assert from "node:assert/strict";
import { PlaybackClock } from "../src/playback-clock.ts";
test("media timestamps follow the audio clock despite arrival jitter", () => {
  const clock = new PlaybackClock();
  assert.equal(clock.schedule(1_000_000, 10), 10.12);
  assert.ok(Math.abs(clock.schedule(1_020_000, 10.025) - 10.14) < 1e-8);
  assert.ok(Math.abs(clock.mediaTime(10.16)! - 1_040_000) < 1e-6);
});
test("late audio and excess lead rebase without retaining an old time origin", () => {
  const clock = new PlaybackClock(); clock.schedule(0, 1);
  assert.ok(Math.abs(clock.schedule(20_000, 2) - 2.12) < 1e-8);
  assert.ok(Math.abs(clock.schedule(4_000_000, 2.02) - 2.14) < 1e-8);
});
test("reset removes the old mapping and invalid clocks are rejected", () => {
  const clock = new PlaybackClock(); clock.schedule(0, 0); clock.reset();
  assert.equal(clock.mediaTime(1), undefined);
  assert.throws(() => clock.schedule(NaN, 0));
});
