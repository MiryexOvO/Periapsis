import {test} from 'node:test';
import assert from 'node:assert/strict';
import {assessProduct,productTierFor} from '../scripts/browser-product-acceptance.mjs';
import {assessNetwork} from '../scripts/browser-network-acceptance.mjs';
function sample(){
  const d={renderFps:60,freezeRatio:0,maxGapMs:30,pixelP95Ms:400,pixelSamples:60,missingPixelSamples:0,transports:['webcodecs']};
  return {status:'measured',sourceExit:0,quality:'source',source:{fps:60},viewers:[0,1].map(index=>
    ({index,baseline:{...d},damage:{...d},recovery:{...d},recoveredUnder1sAfterMs:50,errors:[]}))};
}
test('recoverable continuity can pass while original full-frame-rate stress still fails',()=>{
  const r=sample();for(const v of r.viewers)v.damage.renderFps=45;
  assert.equal(assessProduct(r,'recoverable').status,'pass');
  assert.equal(assessNetwork(r).passed,false);
  assert.equal(assessProduct(r,'normal').status,'fail');
});
test('weak network does not excuse long freezes, accumulated lag, bad recovery or fallback',()=>{
  for(const patch of [{renderFps:23},{freezeRatio:.06},{maxGapMs:1500},{pixelP95Ms:1200},{transports:['http-webm']}]){
    const r=sample();Object.assign(r.viewers[0].damage,patch);assert.equal(assessProduct(r,'recoverable').status,'fail');
  }
  const r=sample();r.viewers[1].recovery.renderFps=40;assert.equal(assessProduct(r,'recoverable').status,'fail');
});
test('a fixed low rendition cannot prove automatic severe-network degradation',()=>{
  const r=sample();r.source.fps=30;
  assert.equal(assessProduct(r,'severe').status,'not-evaluated');
  assert.equal(assessProduct(r,'severe').passed,false);
});
test('missing measurements and unclassified capacity never silently pass',()=>{
  const r=sample();r.viewers[0].damage.pixelP95Ms=null;
  assert.equal(assessProduct(r,'normal').status,'not-evaluated');
  assert.equal(productTierFor('uplink-capacity'),null);
  assert.equal(productTierFor('clean'),'normal');
  assert.equal(productTierFor('uplink'),'recoverable');
  assert.equal(productTierFor('slow-viewer'),'severe');
  assert.equal(productTierFor('uplink-capacity','recoverable'),'recoverable');
  assert.throws(()=>productTierFor('uplink','unknown'));
});
