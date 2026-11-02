import test from "node:test";
import assert from "node:assert/strict";
import { Handshake, encodeControl, decodeControl, type Control } from "../src/handshake.ts";
import { defaultHello, type Agreement } from "../src/session.ts";

const id = "0123456789abcdef0123456789abcdef";
function pair() {
  const a = new Handshake("initiator", id, defaultHello());
  const b = new Handshake("responder", id, defaultHello());
  a.transportReady("moq-transport-22"); b.transportReady("moq-transport-22");
  b.start(); return { a, b, offer: a.start()! };
}
const wire = (message: Control) => decodeControl(encodeControl(message));

test("both sides activate only after the offer/accept/ack exchange", () => {
  const { a, b, offer } = pair();
  const accept = b.receive(wire(offer))!;
  assert.equal(b.state, "AWAITING_ACK"); assert.equal(b.agreement, undefined);
  const ack = a.receive(wire(accept))!;
  assert.equal(a.state, "ACK_PENDING"); assert.equal(a.agreement, undefined);
  a.ackSent(); b.receive(wire(ack));
  assert.equal(a.state, "ACTIVE"); assert.equal(b.state, "ACTIVE");
  assert.deepEqual(a.agreement, b.agreement);
  const copy = a.agreement!; copy.maxBufferMs++;
  assert.equal(a.agreement!.maxBufferMs, 300);
});
test("a fabricated acceptance cannot relax the buffer budget", () => {
  const { a, b, offer } = pair(); const accept = b.receive(offer)!;
  assert.equal(accept.type, "nsp.accept");
  if (accept.type !== "nsp.accept") throw new Error("expected accept");
  (accept.agreement as Agreement).maxBufferMs++;
  assert.throws(() => a.receive(wire(accept)), /agreement mismatch/);
  assert.equal(a.state, "REJECTED"); assert.equal(a.agreement, undefined);
});
test("agreement property order is irrelevant", () => {
  const { a, b, offer } = pair(); const accept = b.receive(offer)!;
  if (accept.type !== "nsp.accept") throw new Error("expected accept");
  accept.agreement = Object.fromEntries(Object.entries(accept.agreement as Agreement).reverse());
  assert.equal(a.receive(wire(accept))!.type, "nsp.ack");
});
test("wrong sessions and premature acknowledgements fail closed", () => {
  const { b } = pair();
  assert.throws(() => b.receive({ type: "nsp.ack", sessionId: "ffffffffffffffffffffffffffffffff" }), /session mismatch/);
  assert.equal(b.state, "REJECTED");
  const second = pair().b;
  assert.throws(() => second.receive({ type: "nsp.ack", sessionId: id }), /unexpected/);
});
test("replayed ACK after activation revokes the active agreement", () => {
  const { a, b, offer } = pair(); const ack = a.receive(b.receive(offer)!)!;
  a.ackSent(); b.receive(ack);
  assert.throws(() => b.receive(ack), /unexpected/);
  assert.equal(b.state, "REJECTED"); assert.equal(b.agreement, undefined);
});
test("closing invalidates media permission and cannot reopen the session", () => {
  const { a, b, offer } = pair(); const ack = a.receive(b.receive(offer)!)!; a.ackSent(); b.receive(ack);
  a.close(); assert.equal(a.agreement, undefined);
  assert.throws(() => a.start(), /unexpected/); assert.equal(a.state, "CLOSED");
});
test("rejects malformed, oversized and unsupported control messages", () => {
  assert.throws(() => decodeControl(new Uint8Array(16 * 1024 + 1)), /too large/);
  assert.throws(() => decodeControl(new TextEncoder().encode(JSON.stringify({ type: "nsp.ack", sessionId: "bad" }))), /envelope/);
  assert.throws(() => decodeControl(new TextEncoder().encode(JSON.stringify({ type: "nsp.future", sessionId: id }))), /unsupported/);
  const endpoint = new Handshake("initiator", id, defaultHello());
  assert.throws(() => endpoint.transportReady("moq-lite-02"), /MOQT/);
  assert.equal(endpoint.state, "REJECTED");
});
