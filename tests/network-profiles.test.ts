import { test } from "node:test";
import assert from "node:assert/strict";
import { seededRandom, profiles } from "../scripts/network-profiles.ts";
import { reserveDatagram } from "../scripts/network-queue.ts";
// 相同种子的丢包决策必须可复验，不能依赖进程启动时钟。
test("seeded loss is reproducible and independent seeds differ",()=>{
 const a=seededRandom(42),b=seededRandom(42),c=seededRandom(43);
 const first=Array.from({length:1000},()=>a());
 assert.deepEqual(first,Array.from({length:1000},()=>b()));
 assert.notDeepEqual(first,Array.from({length:1000},()=>c()));
 assert.ok(first.every(x=>x>=0 && x<1));
});
test("profile serialization uses actual capacity and delay without changing queue bounds",()=>{
 const p=profiles.delay;
 assert.deepEqual(reserveDatagram(0,0,1280,0,true,p.bytesPerSecond,p.delayMs),{next:20,delay:120});
 assert.equal(reserveDatagram(0,200,1280,0,true,p.bytesPerSecond,p.delayMs),undefined);
});
