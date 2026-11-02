/** NSP application conventions preserve MOQT framing and SETUP semantics. */
/** NSP 应用层约定，沿用 MOQT 自身的帧格式和 SETUP 机制。 */
export const NSP_VERSION = 1;
export const MAX_CONTROL_BYTES = 16 * 1024;
export const PROFILES = {
  interactive: { targetBufferMs: 35, maxBufferMs: 100 },
  live: { targetBufferMs: 100, maxBufferMs: 300 },
  quality: { targetBufferMs: 300, maxBufferMs: 1000 },
} as const;
export type Profile = keyof typeof PROFILES;
export interface Hello {
  type: "nsp.hello";
  version: number;
  profiles: Profile[];
  codecs: string[];
  extensions: { id: string; version: number; required: boolean }[];
}
export interface Agreement {
  type: "nsp.accept";
  version: 1;
  profile: Profile;
  codecs: string[];
  extensions: { id: string; version: number }[];
  targetBufferMs: number;
  maxBufferMs: number;
}
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function strings(value: unknown): value is string[] {
  return Array.isArray(value) && value.length > 0 && value.length <= 32 &&
    value.every(x => typeof x === "string" && x.length > 0 && x.length <= 128) && new Set(value).size === value.length;
}
export function parseHello(bytes: Uint8Array): Hello {
  if (bytes.byteLength > MAX_CONTROL_BYTES) throw new Error("control message too large");
  const value: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  if (!object(value) || value.type !== "nsp.hello" || !Number.isSafeInteger(value.version) ||
      !strings(value.profiles) || !value.profiles.every(p => Object.hasOwn(PROFILES, p)) ||
      !strings(value.codecs) || !Array.isArray(value.extensions) || value.extensions.length > 32) {
    throw new Error("invalid NSP hello");
  }
  const ids = new Set<string>();
  for (const extension of value.extensions) {
    if (!object(extension) || typeof extension.id !== "string" ||
        !/^[a-z][a-z0-9.-]{0,63}$/.test(extension.id) || !Number.isSafeInteger(extension.version) ||
        (extension.version as number) < 1 || typeof extension.required !== "boolean" || ids.has(extension.id)) {
      throw new Error("invalid or duplicate extension");
    }
    ids.add(extension.id);
  }
  return value as unknown as Hello;
}
export function negotiate(local: Hello, remote: Hello): Agreement {
  // 即使调用方绕过网络解析器，也要校验双方的能力声明。
  for (const hello of [local, remote]) parseHello(new TextEncoder().encode(JSON.stringify(hello)));
  if (local.version !== NSP_VERSION || remote.version !== NSP_VERSION) throw new Error("unsupported NSP version");
  const profile = local.profiles.find(p => remote.profiles.includes(p));
  if (!profile) throw new Error("no common profile");
  const codecs = local.codecs.filter(c => remote.codecs.includes(c));
  if (!codecs.length) throw new Error("no common codec");
  for (const [a, b] of [[local, remote], [remote, local]]) {
    for (const extension of a.extensions) {
      if (extension.required && !b.extensions.some(e => e.id === extension.id && e.version === extension.version)) {
        throw new Error(`unsupported required extension: ${extension.id}@${extension.version}`);
      }
    }
  }
  const extensions = local.extensions.filter(e => remote.extensions.some(r => r.id === e.id && r.version === e.version))
    .map(({ id, version }) => ({ id, version }));
  return { type: "nsp.accept", version: NSP_VERSION, profile, codecs, extensions, ...PROFILES[profile] };
}
export function defaultHello(profile: Profile = "live"): Hello {
  return { type: "nsp.hello", version: 1, profiles: [profile], codecs: ["application/octet-stream"], extensions: [] };
}
