/** Detect an IDR access unit in length-prefixed AVC data.
 * 检查长度前缀 AVC 访问单元是否包含 IDR，恢复时只从独立帧开始。
 */
export function isAvcKeyframe(data: Uint8Array, lengthSize: number): boolean {
  if (![1, 2, 4].includes(lengthSize)) throw new Error("invalid AVC NAL length size");
  let offset = 0, key = false;
  while (offset < data.length) {
    if (offset + lengthSize > data.length) throw new Error("truncated AVC NAL length");
    let size = 0;
    for (let i = 0; i < lengthSize; i++) size = size * 256 + data[offset++]!;
    if (!size || offset + size > data.length) throw new Error("invalid AVC NAL size");
    key ||= (data[offset]! & 31) === 5;
    offset += size;
  }
  return key;
}
