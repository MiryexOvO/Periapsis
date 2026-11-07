import {test} from 'node:test';
import assert from 'node:assert/strict';
import {assessNetwork} from '../scripts/browser-network-acceptance.mjs';
function report(){
  const d={renderFps:30,freezeRatio:0,pixelP95Ms:400,pixelSamples:60,missingPixelSamples:0,transports:['webcodecs']};
  return {status:'measured',sourceExit:0,quality:'source',source:{fps:30},viewers:[0,1].map(index=>
    ({index,baseline:{...d},damage:{...d},recovery:{...d},recoveredUnder1sAfterMs:20,errors:[]}))};
}
test('gate uses declared frame rate and demands both viewers',()=>{
  assert.equal(assessNetwork(report()).passed,true);
  const r=report();r.viewers.pop();assert.equal(assessNetwork(r).passed,false);
});
test('completed experiments cannot hide stalls, fallback or missing instrumentation',()=>{
  for(const patch of [{renderFps:24},{freezeRatio:.06},{pixelP95Ms:1000},{pixelP95Ms:null},
    {missingPixelSamples:60},{transports:['http-webm']}]){
    const r=report();Object.assign(r.viewers[1].damage,patch);assert.equal(assessNetwork(r).passed,false);
  }
  const r=report();r.viewers[0].recoveredUnder1sAfterMs=null;assert.equal(assessNetwork(r).passed,false);
});
test('a duplicated OBS frame-rate divisor cannot pass cadence validation',()=>{
  const r=report();r.viewers[0].frameDurations={'66666':100};assert.equal(assessNetwork(r).passed,false);
  r.viewers[0].frameDurations={'33333':100};assert.equal(assessNetwork(r).passed,true);
});
