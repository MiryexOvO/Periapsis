import test from "node:test";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { startRelay } from "../src/relay.ts";
import { openPeer } from "../src/peer.ts";
import { defaultHello } from "../src/session.ts";
import { publishTrack, subscribeTrack } from "../src/transport.ts";

async function setup() {
  const token = randomBytes(24).toString("hex");
  const relay = await startRelay({ publishToken: randomBytes(24).toString("hex"),
    subscribeToken: randomBytes(24).toString("hex"), peerToken: token });
  const url = new URL(relay.url); url.searchParams.set("role", "peer"); url.searchParams.set("token", token);
  return { relay, url, id: randomBytes(16).toString("hex") };
}

test("real MoQ exchange activates two peers before opaque media-object transfer", { timeout: 12_000 }, async () => {
  const { relay, url, id } = await setup();
  const peers: Awaited<ReturnType<typeof openPeer>>[] = [];
  try {
    const results = await Promise.allSettled(["initiator", "responder"].map(async role => {
      const peer = await openPeer(url, role as "initiator" | "responder", id);
      peers.push(peer); return peer;
    }));
    for (const result of results) if (result.status === "rejected") throw result.reason;
    assert.deepEqual(peers.map(p => p.state), ["ACTIVE", "ACTIVE"]);
    assert.deepEqual(peers[0]!.agreement, peers[1]!.agreement);
    const sender = publishTrack(peers[0]!.origin, `peri/${id}/data`, "probe");
    const payload = randomBytes(32 * 1024); sender.write(payload);
    const reader = await subscribeTrack(peers[1]!.origin, `peri/${id}/data`, "probe");
    try { assert.deepEqual(await reader.read(), new Uint8Array(payload)); }
    finally { reader.close(); sender.close(); }
    peers[0]!.close(); assert.equal(peers[0]!.state, "CLOSED");
    assert.throws(() => peers[0]!.agreement, /not active/);
  } finally { for (const peer of peers) peer.close(); await relay.close(); }
});

test("incompatible capabilities never produce an active peer", { timeout: 8000 }, async () => {
  const { relay, url, id } = await setup(); const peers: Awaited<ReturnType<typeof openPeer>>[] = [];
  try {
    const incompatible = defaultHello(); incompatible.extensions.push({ id: "future.required", version: 1, required: true });
    const results = await Promise.allSettled([
      openPeer(url, "initiator", id, incompatible, 500), openPeer(url, "responder", id, defaultHello(), 500),
    ].map(async promise => { const peer = await promise; peers.push(peer); return peer; }));
    assert.ok(results.every(result => result.status === "rejected"));
    assert.equal(peers.length, 0);
  } finally { for (const peer of peers) peer.close(); await relay.close(); }
});

test("a missing peer times out without leaving a live relay session", { timeout: 5000 }, async () => {
  const { relay, url, id } = await setup();
  try { await assert.rejects(openPeer(url, "initiator", id, defaultHello(), 200), /timed out|ended|closed|unavailable/i); }
  finally { await relay.close(); }
});
