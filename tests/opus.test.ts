import test from "node:test";
import assert from "node:assert/strict";
import { parseOggOpus } from "../src/opus.ts";
function page(packets: Uint8Array[]) {
  const header = Buffer.alloc(27 + packets.length); header.write("OggS"); header[26] = packets.length;
  packets.forEach((packet, i) => { header[27 + i] = packet.length; });
  return Buffer.concat([header, ...packets]);
}
test("extracts Opus payloads and strips container headers", () => {
  const bytes = page([Buffer.from("OpusHead"), Buffer.from("OpusTags"), Buffer.from([1, 2, 3])]);
  assert.deepEqual(parseOggOpus(bytes), [new Uint8Array([1, 2, 3])]);
});
test("rejects corrupt headers, truncated packets and missing Opus metadata", () => {
  assert.throws(() => parseOggOpus(Buffer.from("bad")));
  assert.throws(() => parseOggOpus(page([Buffer.from("OpusHead"), Buffer.from("OpusTags"), Buffer.from([1, 2, 3])]).subarray(0, -1)));
  assert.throws(() => parseOggOpus(page([Buffer.from("unknown"), Buffer.from("OpusTags"), Buffer.from([1])])));
});
test("reassembles continued packets and rejects a mismatched continuation flag", () => {
  const metadata = page([Buffer.from("OpusHead"), Buffer.from("OpusTags")]);
  const first = Buffer.alloc(28 + 255); first.write("OggS"); first[26] = 1; first[27] = 255; first.fill(7, 28);
  const last = page([Buffer.from([8, 9])]); last[5] = 1;
  const packets = parseOggOpus(Buffer.concat([metadata, first, last]));
  assert.equal(packets[0]!.length, 257); assert.equal(packets[0]![255], 8);
  last[5] = 0;
  assert.throws(() => parseOggOpus(Buffer.concat([metadata, first, last])), /continuation/);
});
