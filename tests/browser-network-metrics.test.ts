import {test} from 'node:test';
import assert from 'node:assert/strict';
import {summarizeDisplay} from '../scripts/browser-network-metrics.ts';
import {reserveDatagram} from '../scripts/network-queue.ts';
test('no output and a stalled final frame remain visible in freeze accounting',()=>{
  const empty=summarizeDisplay([],[],0,1000);
  assert.ok(empty.freezeMs>980);assert.equal(empty.renderedFrames,0);assert.equal(empty.pixelP95Ms,null);
  const tail=summarizeDisplay([{at:0,pts:0,transport:'webcodecs'}],[],0,1000);
  assert.ok(tail.freezeMs>980);assert.equal(tail.maxGapMs,1000);
});
test('window clips an existing stall and keeps missing/fallback pixel samples',()=>{
  const result=summarizeDisplay([{at:0,pts:0,transport:'webcodecs'},{at:800,pts:1,transport:'http-webm'}],
    [{at:550,lagMs:600,transport:'webcodecs'},{at:700,lagMs:null,transport:'http-webm'}],500,1000);
  assert.equal(result.freezeMs,300);assert.equal(result.pixelSamples,1);assert.equal(result.missingPixelSamples,1);
  assert.deepEqual(result.transports,['webcodecs','http-webm']);
});
test('high-rate delay pipe admits more than the legacy 64-packet cap but remains bounded',()=>{
  let next=0;
  for(let n=0;n<100;n++){
    const r=reserveDatagram(0,next,1200,n,true,2500000,45,{packets:512,delayMs:250});
    assert.ok(r);next=r.next;
  }
  assert.equal(reserveDatagram(0,next,1200,512,true,2500000,45,{packets:512,delayMs:250}),undefined);
});
