import test from "node:test";
import assert from "node:assert/strict";
import * as Moq from "@moq/net";
import { resolveBroadcast } from "../src/resolve.ts";

test("waiting request resolves when a publisher announces", async () => {
  const origin = new Moq.Origin.Producer();
  const pending = origin.request(Moq.Path.from("later"), { announced: true });
  try {
    const resolved = resolveBroadcast(pending, 1000);
    const broadcast = origin.createBroadcast(Moq.Path.from("later"));
    broadcast.createTrack("probe"); broadcast.announce();
    assert.equal((await resolved).path, "later");
  } finally { pending.close(); origin.close(); }
});
test("waiting request times out and can be closed", async () => {
  const origin = new Moq.Origin.Producer();
  const pending = origin.request(Moq.Path.from("missing"), { announced: true });
  try { await assert.rejects(resolveBroadcast(pending, 20), /timed out/); }
  finally { pending.close(); origin.close(); }
});
test("closing a pending request cancels its waiter", async () => {
  const origin = new Moq.Origin.Producer();
  const pending = origin.request(Moq.Path.from("missing"), { announced: true });
  const resolved = resolveBroadcast(pending, 1000);
  pending.close();
  try { await assert.rejects(resolved, /closed/); }
  finally { origin.close(); }
});
