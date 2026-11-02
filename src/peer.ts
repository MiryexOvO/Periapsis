import { defaultHello, type Hello, type Agreement } from "./session.ts";
import { Handshake, encodeControl, decodeControl, type Role } from "./handshake.ts";
import { connect, publishTrack, subscribeTrack } from "./transport.ts";

/** Negotiate over ordinary MOQT control tracks using a trusted session invitation.
 * 根据可信邀请中的会话标识，通过普通 MOQT 控制轨道完成双向协商。
 */
export async function openPeer(url: URL, role: Role, sessionId: string, hello: Hello = defaultHello(), timeoutMs = 5000) {
  const handshake = new Handshake(role, sessionId, hello);
  const peer = await connect(url, "duplex", timeoutMs);
  const outgoingPath = `nsp/${sessionId}/${role}`;
  const incomingPath = `nsp/${sessionId}/${role === "initiator" ? "responder" : "initiator"}`;
  let output: ReturnType<typeof publishTrack> | undefined;
  let input: Awaited<ReturnType<typeof subscribeTrack>> | undefined;
  void peer.connection.closed.then(() => { handshake.close(); input?.close(); output?.close(); });
  try {
    handshake.transportReady(peer.connection.version);
    output = publishTrack(peer.origin, outgoingPath, "nsp.control");
    const offer = handshake.start();
    if (offer) output.write(encodeControl(offer));
    input = await subscribeTrack(peer.origin, incomingPath, "nsp.control", timeoutMs);
    while (handshake.state !== "ACTIVE") {
      const payload = await input.read();
      if (!payload) throw new Error("control track ended before activation");
      const message = decodeControl(payload);
      const reply = handshake.receive(message);
      if (reply) {
        output.write(encodeControl(reply));
        if (reply.type === "nsp.ack") handshake.ackSent();
      }
    }
    return { origin: peer.origin, connection: peer.connection,
      get agreement(): Agreement {
        const agreement = handshake.agreement;
        if (!agreement) throw new Error("session is not active");
        return agreement;
      },
      get state() { return handshake.state; },
      close() { handshake.close(); input?.close(); output?.close(); peer.close(); } };
  } catch (error) {
    if (output) {
      try { output.write(encodeControl({ type: "nsp.reject", sessionId })); } catch { /* 控制轨道可能已随传输关闭。 */ }
    }
    handshake.close(); input?.close(); output?.close(); peer.close();
    throw error;
  }
}
