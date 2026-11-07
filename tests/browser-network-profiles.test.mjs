import {test} from 'node:test';
import assert from 'node:assert/strict';
import {browserProfile} from '../scripts/browser-network-profiles.mjs';
test('single-factor uplink profiles do not silently inject the other impairments',()=>{
  const delay=browserProfile('uplink-delay','source'),loss=browserProfile('uplink-loss','source');
  assert.deepEqual([delay.delayMs,delay.loss,delay.blackoutMs],[45,0,0]);
  assert.deepEqual([loss.delayMs,loss.loss,loss.blackoutMs],[0,.01,0]);
  const cap=browserProfile('uplink-capacity','720'),blackout=browserProfile('uplink-blackout','source');
  assert.deepEqual([cap.bytesPerSecond,cap.delayMs,cap.loss,cap.blackoutMs],[1e6,0,0,0]);
  assert.deepEqual([blackout.delayMs,blackout.loss,blackout.blackoutMs],[0,0,180]);
  assert.equal(loss.bytesPerSecond,Number.MAX_SAFE_INTEGER);
  assert.equal(blackout.direction,'up');
  assert.equal(browserProfile('uplink','source').bytesPerSecond,25e6/8);
});
