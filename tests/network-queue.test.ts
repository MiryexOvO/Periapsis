import { test } from "node:test";
import assert from "node:assert/strict";
import { reserveDatagram } from "../scripts/network-queue.ts";

test("overflow does not create phantom serialization debt; traffic resumes within the bound", () => {
  let next = 0;
  for (let i = 0; i < 4; i++) {
    const slot = reserveDatagram(0, next, 1200, i, true)!;
    assert.equal(slot.delay, (i + 1) * 50 + 45);
    next = slot.next;
  }
  for (let i = 0; i < 100; i++) assert.equal(reserveDatagram(0, next, 1200, 4, true), undefined);
  assert.equal(next, 200);
  assert.deepEqual(reserveDatagram(50, next, 1200, 4, true), { next: 250, delay: 245 });
  assert.deepEqual(reserveDatagram(300, next, 1200, 0, true), { next: 350, delay: 95 });
});

test("packet count limit applies even after impairment ends", () => {
  assert.equal(reserveDatagram(1000, 1000, 1200, 64, false), undefined);
  assert.deepEqual(reserveDatagram(1000, 1000, 1200, 63, false), { next: 1000, delay: 0 });
});

test("existing admitted backlog drains when impairment ends", () => {
  assert.deepEqual(reserveDatagram(1000, 1100, 1200, 1, false), { next: 1100, delay: 100 });
});
