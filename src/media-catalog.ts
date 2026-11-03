import * as upstream from "@moq/msf";

/** Typed facade for the upstream MSF catalog codec under NodeNext resolution.
 * 适配上游声明文件的 NodeNext 导出解析问题，编解码仍由原包执行。
 */
export interface Catalog {
  tracks: { name: string; packaging: string; role?: string; codec?: string; initData?: string; width?: number; height?: number; framerate?: number; samplerate?: number; channelConfig?: string; isLive?: boolean }[];
}
const codec = upstream as unknown as { encode(catalog: Catalog): Uint8Array; decode(bytes: Uint8Array): Catalog };
export const encode = codec.encode;
export const decode = codec.decode;
