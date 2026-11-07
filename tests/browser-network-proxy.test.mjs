import {test} from 'node:test';
import assert from 'node:assert/strict';
import dgram from 'node:dgram';
import {udpProxy} from '../scripts/browser-network-proxy.mjs';
// Real UDP verifies impairment direction; feedback traffic must not share data loss.
// 真实 UDP 验证损伤方向，反馈方向不能误用媒体方向的丢包设置。
test('downlink loss does not drop uplink, and the same flow resumes after recovery',async()=>{
  const server=dgram.createSocket('udp4'),client=dgram.createSocket('udp4');
  await new Promise(r=>server.bind(0,'127.0.0.1',r));
  await new Promise(r=>client.bind(0,'127.0.0.1',r));
  const proxy=await udpProxy(server.address().port,123);
  let received=0,replies=0;
  server.on('message',(bytes,peer)=>{received++;server.send(bytes,peer.port,peer.address)});
  client.on('message',()=>replies++);
  const sleep=ms=>new Promise(r=>setTimeout(r,ms));
  try{
    proxy.setPhase('loss',{direction:'down',bytesPerSecond:1e6,delayMs:0,loss:1,blackoutMs:0});
    client.send(Buffer.from('request'),proxy.port,'127.0.0.1');
    for(let i=0;i<100 && !proxy.stats.down.loss?.randomDrop;i++)await sleep(5);
    assert.equal(received,1);assert.equal(replies,0);assert.equal(proxy.stats.down.loss.randomDrop,1);
    proxy.setPhase('recovered');
    client.send(Buffer.from('again'),proxy.port,'127.0.0.1');
    for(let i=0;i<100 && !replies;i++)await sleep(5);
    assert.equal(received,2);assert.equal(replies,1);
    proxy.setPhase('uplink',{direction:'up',bytesPerSecond:1e6,delayMs:0,loss:1,blackoutMs:0});
    client.send(Buffer.from('blocked'),proxy.port,'127.0.0.1');
    for(let i=0;i<100 && !proxy.stats.up.uplink?.randomDrop;i++)await sleep(5);
    assert.equal(received,2);assert.equal(proxy.stats.up.uplink.randomDrop,1);
  }finally{proxy.close();server.close();client.close();}
});
