import { test } from "node:test";
import assert from "node:assert/strict";
import { readFlv } from "../src/flv.ts";
import { isAvcKeyframe } from "../src/avc.ts";

function fixture() {
  const header = Buffer.from([70, 76, 86, 1, 5, 0, 0, 0, 9, 0, 0, 0, 0]);
  const tag = Buffer.alloc(18); tag[0] = 9; tag.writeUIntBE(3, 1, 3); tag.writeUIntBE(42, 4, 3); tag[7] = 1;
  tag.set([1, 2, 3], 11); tag.writeUInt32BE(14, 14);
  return Buffer.concat([header, tag]);
}
test("FLV supports every byte boundary and extended timestamps", async () => {
  const result = [];
  for await (const tag of readFlv((async function* () { for (const byte of fixture()) yield Uint8Array.of(byte); })())) result.push(tag);
  assert.equal(result.length, 1); assert.equal(result[0]!.timestamp, (0x1000000 + 42) * 1000);
  assert.deepEqual([...result[0]!.data], [1, 2, 3]);
});
test("FLV rejects oversized tags, framing damage and truncation", async () => {
  for (const kind of ["size", "framing", "truncated"]) {
    let data = fixture();
    if (kind === "size") data.writeUIntBE(1024 * 1024 + 1, 14, 3);
    if (kind === "framing") data[data.length - 1] = 0;
    if (kind === "truncated") data = data.subarray(0, -1);
    await assert.rejects(async () => { for await (const _ of readFlv((async function* () { yield data; })())) { /* 消费输入，触发校验。 */ } });
  }
});
test("AVC detects IDR among multiple NAL units and rejects corrupt lengths", () => {
  assert.equal(isAvcKeyframe(Uint8Array.of(0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65), 4), true);
  assert.equal(isAvcKeyframe(Uint8Array.of(1, 0x41), 1), false);
  assert.throws(() => isAvcKeyframe(Uint8Array.of(0, 0, 1), 4));
  assert.throws(() => isAvcKeyframe(Uint8Array.of(2, 0x65), 1));
});
