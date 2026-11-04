/** Extract bounded Opus packets from an Ogg test fixture.
 * 从有界 Ogg 测试文件中重组 Opus 包，处理跨页分段。
 */
export function parseOggOpus(bytes: Uint8Array): Uint8Array[] {
  if (bytes.length > 4 * 1024 * 1024) throw new Error("Ogg input too large");
  const packets: Uint8Array[] = []; let parts: Uint8Array[] = [], length = 0;
  for (let offset = 0; offset < bytes.length;) {
    if (offset + 27 > bytes.length || new TextDecoder().decode(bytes.subarray(offset, offset + 4)) !== "OggS" || bytes[offset + 4] !== 0) throw new Error("invalid Ogg page");
    const count = bytes[offset + 26]!;
    if (Boolean(bytes[offset + 5]! & 1) !== Boolean(parts.length)) throw new Error("invalid Ogg continuation");
    if (offset + 27 + count > bytes.length) throw new Error("truncated Ogg table");
    const table = bytes.subarray(offset + 27, offset + 27 + count); offset += 27 + count;
    for (const size of table) {
      if (offset + size > bytes.length) throw new Error("truncated Ogg packet");
      parts.push(bytes.subarray(offset, offset + size)); offset += size; length += size;
      if (length > 65536) throw new Error("Opus packet too large");
      if (size < 255) {
        if (!length || packets.length >= 5000) throw new Error("invalid Opus packet count or size");
        const packet = new Uint8Array(length); let start = 0;
        for (const part of parts) { packet.set(part, start); start += part.length; }
        packets.push(packet); parts = []; length = 0;
      }
    }
  }
  if (parts.length || packets.length < 3 || new TextDecoder().decode(packets[0]!.subarray(0, 8)) !== "OpusHead" || new TextDecoder().decode(packets[1]!.subarray(0, 8)) !== "OpusTags") throw new Error("invalid Opus stream");
  return packets.slice(2);
}
