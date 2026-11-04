/** Parse bounded VP8 IVF test inputs without decoding the codec bitstream.
 * 解析有大小限制的 VP8 IVF 测试输入，不执行视频解码。
 */
export function parseIvf(bytes: Uint8Array) {
  if (bytes.length < 32 || bytes.length > 32 * 1024 * 1024) throw new Error("invalid IVF size");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const text = (offset: number) => new TextDecoder().decode(bytes.subarray(offset, offset + 4));
  if (text(0) !== "DKIF" || text(8) !== "VP80" || view.getUint16(4, true) !== 0 || view.getUint16(6, true) !== 32) throw new Error("expected VP8 IVF");
  const width = view.getUint16(12, true), height = view.getUint16(14, true);
  const rate = view.getUint32(16, true), scale = view.getUint32(20, true);
  if (!width || !height || !rate || !scale || width > 4096 || height > 4096 || rate / scale > 120) throw new Error("unsupported video parameters");
  const frames: { payload: Uint8Array; timestamp: number; keyframe: boolean }[] = [];
  for (let offset = 32; offset < bytes.length;) {
    if (offset + 12 > bytes.length) throw new Error("truncated frame header");
    const size = view.getUint32(offset, true), ticks = view.getBigUint64(offset + 4, true);
    offset += 12;
    if (!size || size > 1024 * 1024 || offset + size > bytes.length || ticks > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("invalid frame size or timestamp");
    const payload = bytes.subarray(offset, offset + size);
    const timestamp = Math.round(Number(ticks) * scale / rate * 1_000_000);
    if (!Number.isSafeInteger(timestamp) || (frames.length && timestamp <= frames.at(-1)!.timestamp)) throw new Error("invalid timestamp sequence");
    frames.push({ payload, timestamp, keyframe: (payload[0]! & 1) === 0 }); offset += size;
    if (frames.length > 3600) throw new Error("too many frames");
  }
  if (!frames.length || !frames[0]!.keyframe) throw new Error("missing initial keyframe");
  return { width, height, framerate: rate / scale, frames, duration: frames.at(-1)!.timestamp + Math.round(1_000_000 * scale / rate) };
}
