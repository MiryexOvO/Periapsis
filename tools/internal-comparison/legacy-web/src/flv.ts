export interface FlvTag { type: number; timestamp: number; data: Buffer }

/** Read bounded FLV tags from a fragmented byte stream.
 * 从任意分片的字节流读取有界 FLV 标签。
 */
export async function* readFlv(input: AsyncIterable<Uint8Array>): AsyncGenerator<FlvTag> {
  let pending = Buffer.alloc(0), header = false;
  for await (const chunk of input) {
    if (chunk.length > 2 * 1024 * 1024) throw new Error("FLV input chunk too large");
    pending = Buffer.concat([pending, chunk]);
    if (!header) {
      if (pending.length < 9) continue;
      if (pending.toString("ascii", 0, 3) !== "FLV" || pending[3] !== 1 || pending.readUInt32BE(5) !== 9) throw new Error("invalid FLV header");
      if (pending.length < 13) continue;
      if (pending.readUInt32BE(9) !== 0) throw new Error("invalid FLV previous tag size");
      pending = pending.subarray(13); header = true;
    }
    while (pending.length >= 11) {
      const size = pending.readUIntBE(1, 3);
      if (size > 1024 * 1024) throw new Error("FLV tag too large");
      if (pending.length < size + 15) break;
      if (pending.readUIntBE(8, 3) !== 0 || pending.readUInt32BE(11 + size) !== size + 11) throw new Error("invalid FLV tag framing");
      const tag = { type: pending[0]!, timestamp: (pending[7]! * 0x1000000 + pending.readUIntBE(4, 3)) * 1000, data: pending.subarray(11, 11 + size) };
      pending = pending.subarray(size + 15);
      yield tag;
    }
  }
  if (!header || pending.length) throw new Error("truncated FLV stream");
}
