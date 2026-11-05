import { test } from "node:test";
import assert from "node:assert/strict";
import { analyzeFreezes, analyzeObjectFailures, type TraceEvent } from "../scripts/freeze-analysis.ts";
const event = (event:string, wall_ms:number, data:Record<string,unknown>={},process=1):TraceEvent=>({event,wall_ms,data,process});
test("object failure candidates require known identity and preceding time",()=>{
  const report=analyzeObjectFailures([
    event("object_send_cancel",50,{sequence:7,stream:3,reason:"ack_deadline"},2),
    event("object_send_cancel",150,{sequence:7,stream:3,reason:"peer_stop"},2),
    event("object_send_cancel",50,{sequence:8,stream:3,reason:"ack_deadline"},2),
    event("object_read_end",100,{sequence:7,stream:3,phase:"payload",reason:"peer_reset_1"}),
    event("object_read_end",100,{sequence:null,stream:3,phase:"header",reason:"residence_timeout"}),
    event("object_read_end",2000,{sequence:7,stream:3,reason:"cancelled"}),
  ]);
  assert.equal(report.reads[0]!.sendCandidates.length,1);
  assert.equal(report.reads[1]!.sendCandidates.length,0);
  assert.equal(report.reads[2]!.sendCandidates.length,0);
  assert.equal(report.readReasons.residence_timeout,1);
});
test("freeze evidence isolates players and preserves unknown causes",()=>{
  const report=analyzeFreezes([
    event("frame_output",0,{sequence:1}),
    event("frame_output",100,{sequence:2}),
    event("player_recovery",200,{reason:"decode_timeout"},2),
    event("relay_drop",220,{viewer:8,reason:"expired"},3),
    event("frame_output",500,{sequence:3}),
    event("player_receive",600,{elapsed_ms:150}),
    event("player_recovery",610,{reason:"receive_abandoned"}),
    event("player_decode",790,{outcome:"ok",elapsed_ms:60}),
    event("frame_output",800,{sequence:5}),
  ]);
  assert.equal(report.freezeCount,2);
  assert.equal(report.freezeMs,(400-67)+(300-67));
  assert.equal(report.unattributed,1);
  assert.deepEqual(report.gaps[0]!.recovery,[]);
  assert.equal(report.gaps[0]!.relayEvidence.length,1);
  assert.equal(report.gaps[1]!.maxReceiveMs,150);
  assert.equal(report.gaps[1]!.maxDecodeMs,60);
});
test("threshold, empty logs and reversed output clocks remain explicit",()=>{
  assert.equal(analyzeFreezes([]).freezeCount,0);
  assert.equal(analyzeFreezes([event("frame_output",0),event("frame_output",250)]).freezeCount,0);
  assert.throws(()=>analyzeFreezes([event("frame_output",100),event("frame_output",90)]),/reversed/);
});
