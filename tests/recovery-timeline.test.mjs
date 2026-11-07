import {test} from 'node:test';
import assert from 'node:assert/strict';
import {correlateKeys,correlateProducerKeys} from '../scripts/recovery-timeline.mjs';
test('a transport receipt cannot substitute for decoded or drawn key evidence',()=>{
  const rows=correlateKeys([
    {event:'source_key_start',wall_ms:1,data:{sequence:4,pts_us:100,bytes:200,budget_ms:750}},
    {event:'source_key_receipt',wall_ms:3,data:{pts_us:100,ok:true}},
    {event:'relay_key_received',wall_ms:2,data:{pts_us:101}},
  ],[{decoder:{keyInputs:[{pts:100,at:4}],keyOutputs:[]},frames:[{pts:101,at:5,transport:'webcodecs'}]}]);
  assert.equal(rows[0].sourceReceipts.length,1);assert.equal(rows[0].relayReceived.length,0);
  assert.equal(rows[0].viewers[0].keyInput.at,4);
  assert.equal(rows[0].viewers[0].keyOutput,null);assert.equal(rows[0].viewers[0].exactKeyDraw,null);
});
test('a generated but rejected key cannot inherit a later key transport success',()=>{
  const e=(event,key_id,wall_ms)=>({event,wall_ms,data:{key_id,pts_us:0}});
  const rows=correlateProducerKeys([e('key_encoded',1,1),e('key_rejected',1,2),
    e('key_encoded',2,3),e('key_admitted',2,4),e('key_send_begin',2,6),e('key_send_end',2,9)],
    [{pts:0,sourceStart:7,sourceReceipts:[{ok:true}]}]);
  assert.equal(rows.length,2);assert.equal(rows[0].transport,null);assert.ok(rows[0].rejected);
  assert.equal(rows[1].transport.sourceStart,7);
});
