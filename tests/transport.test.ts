import test from "node:test";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { startRelay } from "../src/relay.ts";
import { connect, publishTrack, subscribeTrack, MAX_OBJECT_BYTES } from "../src/transport.ts";

test("real MOQT-22 sessions forward an opaque object to two subscribers over loopback WebSocket", { timeout: 12_000 }, async t => {
  const publishToken = randomBytes(24).toString("hex"), subscribeToken = randomBytes(24).toString("hex");
  const relay = await startRelay({ publishToken, subscribeToken });
  t.after(() => relay.close());
  const endpoint = (role: string, token: string) => { const u = new URL(relay.url); u.searchParams.set("role", role); u.searchParams.set("token", token); return u; };
  const publisher = await connect(endpoint("publish", publishToken), "publish"); t.after(() => publisher.close());
  const subscribers = await Promise.all([1, 2].map(() => connect(endpoint("subscribe", subscribeToken), "subscribe")));
  for (const sub of subscribers) t.after(() => sub.close());
  assert.equal(publisher.connection.version, "moq-transport-22");
  assert.equal(publisher.connection.transport, "websocket");
  const track = publishTrack(publisher.origin, "test/objects", "probe"); t.after(() => track.close());
  assert.throws(() => track.write(new Uint8Array(MAX_OBJECT_BYTES + 1)), /size/);
  const payload = randomBytes(64 * 1024); track.write(payload);
  const receivers = await Promise.all(subscribers.map(sub => subscribeTrack(sub.origin, "test/objects", "probe")));
  for (const receiver of receivers) t.after(() => receiver.close());
  const received = await Promise.all(receivers.map(receiver => receiver.read()));
  for (const bytes of received) assert.deepEqual(bytes, new Uint8Array(payload));
  const health = await fetch(relay.healthUrl).then(r => r.json()) as { sessions: number };
  assert.equal(health.sessions, 3);
});
test("relay rejects missing or wrong role credentials", { timeout: 5000 }, async t => {
  const relay = await startRelay({ publishToken: "publisher-secret-123", subscribeToken: "subscriber-secret-456" });
  t.after(() => relay.close());
  await assert.rejects(connect(relay.url, "publish", 500));
});
