import * as Moq from "@moq/net";
import QMux from "@moq/qmux";
import { resolveBroadcast } from "./resolve.ts";

export const MOQT_PROTOCOL = "moqt-22";
export const QMUX_PROTOCOL = "qmux-01.moqt-22";
export const MAX_OBJECT_BYTES = 1024 * 1024;

/** Pin draft-22 and reject silent moq-lite negotiation. QMux is a compatibility transport. */
/** 固定 draft-22，拒绝静默切换到 moq-lite；QMux 提供兼容传输。 */
export async function connect(url: URL, role: "publish" | "subscribe" | "duplex", timeoutMs = 5000, certificateHash?: string, signal?: AbortSignal) {
  signal?.throwIfAborted();
  const origin = new Moq.Origin.Producer();
  let transport: WebTransport | undefined;
  const abort = () => transport?.close();
  signal?.addEventListener("abort", abort, { once: true });
  try {
    if (url.protocol === "ws:" || url.protocol === "wss:") {
      transport = new QMux(url, { protocols: [QMUX_PROTOCOL], requireProtocol: true });
    } else if (url.protocol === "https:") {
      if (typeof WebTransport === "undefined") throw new Error("WebTransport unavailable in this runtime; use ws:/wss: compatibility or a native QUIC client");
      if (certificateHash && !/^[a-f0-9]{64}$/.test(certificateHash)) throw new Error("invalid certificate hash");
      const options: WebTransportOptions & { protocols: string[] } = { protocols: [MOQT_PROTOCOL],
        ...(certificateHash ? { serverCertificateHashes: [{ algorithm: "sha-256", value: Uint8Array.from(certificateHash.match(/../g)!, hex => parseInt(hex, 16)).buffer }] } : {}) };
      transport = new WebTransport(url, options);
    } else {
      throw new Error("expected https: for WebTransport or ws:/wss: for QMux compatibility");
    }
    transport.closed.catch(() => {});
    // 外部传输须先完成子协议协商，MOQT 才能读取协商结果。
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([transport.ready, new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error("transport handshake timed out")), timeoutMs);
      })]);
    } finally { clearTimeout(timer); }
    const connection = await Moq.Connection.connect({
      url, transport, signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(timeoutMs)]) : AbortSignal.timeout(timeoutMs),
      ...(role !== "subscribe" ? { publish: origin.consume() } : {}),
      ...(role !== "publish" ? { consume: origin } : {}),
    });
    if (connection.version !== "moq-transport-22") {
      connection.close();
      throw new Error(`unexpected MOQT version: ${connection.version}`);
    }
    return { origin, connection, close() { connection.close(); origin.close(); } };
  } catch (error) {
    transport?.close();
    origin.close();
    throw error;
  } finally { signal?.removeEventListener("abort", abort); }
}
export function publishTrack(origin: Moq.Origin.Producer, path: string, trackName: string) {
  const broadcast = origin.createBroadcast(Moq.Path.from(path));
  const track = broadcast.createTrack(trackName, { maxAge: Moq.Time.Milli(3000) });
  broadcast.announce();
  return {
    write(payload: Uint8Array) {
      if (!payload.byteLength || payload.byteLength > MAX_OBJECT_BYTES) throw new Error("object size out of bounds");
      const group = track.appendGroup();
      group.writeFrame({ payload, timestamp: Moq.Time.Timestamp.now() });
      group.close();
    },
    close() { track.close(); broadcast.close(); },
  };
}
export async function subscribeTrack(origin: Moq.Origin.Producer, path: string, trackName: string, timeoutMs = 5000) {
  const requested = origin.request(Moq.Path.from(path));
  let broadcast: Moq.Broadcast.Consumer;
  try { broadcast = await resolveBroadcast(requested, timeoutMs); }
  catch (error) { requested.close(); throw error; }
  const track = broadcast.track(trackName).subscribe({ maxAge: Moq.Time.Milli(3000) });
  return {
    async read(): Promise<Uint8Array | undefined> {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const expiry = new Promise<never>((_, reject) => {
        timer = setTimeout(() => { track.close(); reject(new Error("object receive timed out")); }, timeoutMs);
      });
      let group: Moq.Group.Consumer | undefined;
      try {
        group = await Promise.race([track.recvGroup(), expiry]);
        if (!group) return undefined;
        const frame = await Promise.race([group.readFrame(), expiry]);
        if (frame && frame.payload.byteLength > MAX_OBJECT_BYTES) throw new Error("received object too large");
        return frame?.payload;
      } finally { clearTimeout(timer); group?.close(); }
    },
    close() { track.close(); requested.close(); },
  };
}
