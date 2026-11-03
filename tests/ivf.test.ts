import test from "node:test";
import assert from "node:assert/strict";
import { parseIvf } from "../src/ivf.ts";

function fixture() {
  const bytes = Buffer.alloc(58); bytes.write("DKIF"); bytes.writeUInt16LE(32, 6); bytes.write("VP80", 8);
  bytes.writeUInt16LE(640, 12); bytes.writeUInt16LE(360, 14); bytes.writeUInt32LE(30, 16); bytes.writeUInt32LE(1, 20);
  bytes.writeUInt32LE(1, 32); bytes.writeBigUInt64LE(0n, 36); bytes[44] = 0;
  bytes.writeUInt32LE(1, 45); bytes.writeBigUInt64LE(1n, 49); bytes[57] = 1;
  return bytes;
}
test("IVF maps VP8 frame types and timebase to microseconds", () => {
  const video = parseIvf(fixture());
  assert.equal(video.width, 640); assert.equal(video.framerate, 30);
  assert.deepEqual(video.frames.map(f => [f.timestamp, f.keyframe]), [[0, true], [33333, false]]);
});
test("IVF rejects truncated and oversized frame declarations", () => {
  assert.throws(() => parseIvf(fixture().subarray(0, 50)), /truncated/);
  const bytes = fixture(); bytes.writeUInt32LE(2 ** 24, 32);
  assert.throws(() => parseIvf(bytes), /size/);
});
test("IVF rejects a non-random-access start and invalid clock", () => {
  const bytes = fixture(); bytes[44] = 1;
  assert.throws(() => parseIvf(bytes), /keyframe/);
  bytes.writeUInt32LE(0, 16); assert.throws(() => parseIvf(bytes), /parameters/);
});
test("IVF rejects duplicate timestamps and unsupported codec", () => {
  const bytes = fixture(); bytes.writeBigUInt64LE(0n, 49);
  assert.throws(() => parseIvf(bytes), /timestamp/);
  bytes.write("VP90", 8); assert.throws(() => parseIvf(bytes), /VP8/);
});
