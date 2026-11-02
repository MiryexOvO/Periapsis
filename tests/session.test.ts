import test from "node:test";
import assert from "node:assert/strict";
import { defaultHello, negotiate, parseHello, MAX_CONTROL_BYTES } from "../src/session.ts";

test("negotiates a bounded media profile and ignores unknown optional extensions", () => {
  const remote = defaultHello(); remote.extensions.push({ id: "future.feature", version: 1, required: false });
  assert.deepEqual(negotiate(defaultHello(), remote), { type: "nsp.accept", version: 1, profile: "live",
    codecs: ["application/octet-stream"], extensions: [], targetBufferMs: 100, maxBufferMs: 300 });
});
test("critical requirements are enforced in both directions", () => {
  const remote = defaultHello(); remote.extensions.push({ id: "future.feature", version: 1, required: true });
  assert.throws(() => negotiate(defaultHello(), remote), /required extension/);
  assert.throws(() => negotiate(remote, defaultHello()), /required extension/);
});
test("rejects incompatible versions, codecs and profiles", () => {
  assert.throws(() => negotiate(defaultHello(), { ...defaultHello(), version: 2 }), /version/);
  assert.throws(() => negotiate(defaultHello(), { ...defaultHello(), codecs: ["video/av1"] }), /codec/);
  assert.throws(() => negotiate(defaultHello(), defaultHello("interactive")), /profile/);
});
test("rejects oversized, malformed and duplicate control data", () => {
  assert.throws(() => parseHello(new Uint8Array(MAX_CONTROL_BYTES + 1)), /too large/);
  assert.throws(() => parseHello(new Uint8Array([255])));
  const remote = defaultHello(); remote.extensions = Array(2).fill({ id: "nsp.clock", version: 1, required: false });
  assert.throws(() => negotiate(defaultHello(), remote), /duplicate/);
});
