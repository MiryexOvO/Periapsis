import { randomBytes } from "node:crypto";
import { startRelay } from "./relay.ts";
import { openPeer } from "./peer.ts";

const peerToken = randomBytes(24).toString("hex");
const relay = await startRelay({ publishToken: randomBytes(24).toString("hex"),
  subscribeToken: randomBytes(24).toString("hex"), peerToken });
const url = new URL(relay.url); url.searchParams.set("role", "peer"); url.searchParams.set("token", peerToken);
const sessionId = randomBytes(16).toString("hex");
const peers: Awaited<ReturnType<typeof openPeer>>[] = [];
try {
  const results = await Promise.allSettled(["initiator", "responder"].map(async role => {
    const peer = await openPeer(url, role as "initiator" | "responder", sessionId);
    peers.push(peer); return peer;
  }));
  for (const result of results) if (result.status === "rejected") throw result.reason;
  console.log(JSON.stringify({ protocol: peers[0]!.connection.version, transport: peers[0]!.connection.transport,
    states: peers.map(peer => peer.state), agreement: peers[0]!.agreement }, null, 2));
} finally {
  for (const peer of peers) peer.close();
  await relay.close();
}
