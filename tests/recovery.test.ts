import test from "node:test";
import assert from "node:assert/strict";
import { recoverPlayback, RetryablePlaybackError } from "../src/recovery.ts";
test("transient failure retries and succeeds without overlapping attempts", async () => {
  let calls = 0, active = 0, peak = 0;
  await recoverPlayback(async () => { peak = Math.max(peak, ++active); active--; if (++calls < 3) throw new RetryablePlaybackError("offline"); }, new AbortController().signal, () => {}, 1);
  assert.equal(calls, 3); assert.equal(peak, 1);
});
test("permanent failure is not retried", async () => {
  let calls = 0;
  await assert.rejects(recoverPlayback(async () => { calls++; throw new Error("unsupported codec"); }, new AbortController().signal, () => {}, 1));
  assert.equal(calls, 1);
});
test("cancelling during backoff prevents subsequent connections", async () => {
  const controller = new AbortController(); let calls = 0;
  await recoverPlayback(async () => { calls++; throw new RetryablePlaybackError("offline"); }, controller.signal, () => controller.abort(), 1000);
  assert.equal(calls, 1);
});
test("retry budget is finite and backoff increases", async () => {
  const delays: number[] = []; let calls = 0;
  await assert.rejects(recoverPlayback(async () => { calls++; throw new RetryablePlaybackError("offline"); }, new AbortController().signal, (_, delay) => delays.push(delay), 1, 3));
  assert.equal(calls, 4); assert.deepEqual(delays, [1, 2, 4]);
});
