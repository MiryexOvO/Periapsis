import test from "node:test";
import assert from "node:assert/strict";
import { DeadlineQueue, type MediaUnit } from "../src/scheduler.ts";
const unit = (id: string, kind: MediaUnit["kind"] = "reference", deadlineMs = 100): MediaUnit =>
  ({ id, kind, deadlineMs, track: "video", bytes: 100, dependencies: [] });
test("bounded queue refuses overflow without changing accounting", () => {
  const queue = new DeadlineQueue(100, 1); queue.push(unit("a"));
  assert.throws(() => queue.push(unit("b")), /capacity/);
  assert.equal(queue.bytes, 100); assert.equal(queue.size, 1);
  queue.next(0, new Set()); assert.equal(queue.bytes, 0);
});
test("late reference data requires recovery; disposable data can be dropped", () => {
  const queue = new DeadlineQueue(); queue.push(unit("ref")); queue.push(unit("delta", "disposable"));
  assert.equal(queue.next(101, new Set())?.action, "recover");
  assert.equal(queue.next(101, new Set())?.action, "drop");
});
test("does not send a frame before dependencies are available", () => {
  const queue = new DeadlineQueue(); const dependent = unit("p", "reference", 50); dependent.dependencies = ["key"];
  queue.push(dependent); queue.push(unit("key", "key", 60));
  assert.equal(queue.next(0, new Set())?.unit.id, "key");
  assert.equal(queue.next(0, new Set()), undefined);
  assert.equal(queue.next(0, new Set(["key"]))?.unit.id, "p");
});
