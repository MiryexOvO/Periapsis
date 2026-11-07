import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

// Drive the actual shipped player with deterministic stream/decode/clock boundaries.
// 直接运行交付播放器，用确定性的流、解码输出和时钟重现晚加入缓存。
const player=fs.readFileSync(new URL('../crates/peri-webtransport/web/player.js',import.meta.url),'utf8');
const flush=()=>new Promise(resolve=>setImmediate(resolve));
function fixture(duration=16667){
  let now=0,serial=0,controllerVideo,controllerAudio,liveFrames=0,videoDecoder;
  const callbacks=new Map(),timers=new Map(),drawn=[],audioNodes=[],created=[];
  const encoder=new TextEncoder();
  function stream(track,save){return new ReadableStream({start(c){
    save(c);const json=encoder.encode(JSON.stringify(track)),bytes=new Uint8Array(json.length+4);
    new DataView(bytes.buffer).setUint32(0,json.length);bytes.set(json,4);c.enqueue(bytes);
  }})}
  const videoStream=stream({kind:'video',codec:'avc1.64001f',width:1280,height:720},c=>controllerVideo=c);
  const audioStream=stream({kind:'audio',codec:'mp4a.40.2',sample_rate:48000,channels:2},c=>controllerAudio=c);
  class Transport {
    ready=Promise.resolve();closed=new Promise(()=>{});
    incomingUnidirectionalStreams=new ReadableStream({start(c){c.enqueue(videoStream);c.enqueue(audioStream)}});
    close(){}
  }
  class VideoDecoder {
    static async isConfigSupported(){return {supported:true}}
    state='unconfigured';decodeQueueSize=0;
    resets=0;
    constructor(init){this.init=init;if(new.target===VideoDecoder)videoDecoder=this;}
    configure(){this.state='configured'}
    reset(){this.state='unconfigured';this.resets++;this.decodeQueueSize=0;}
    close(){this.state='closed'}
    decode(chunk){
      let closed=false;liveFrames++;
      const frame={timestamp:chunk.timestamp,duration:chunk.duration,displayWidth:1280,displayHeight:720,
        close(){assert.equal(closed,false,'frame closed twice');closed=true;liveFrames--}};
      created.push(frame);this.init.output(frame);
    }
  }
  class AudioDecoder extends VideoDecoder {
    decode(chunk){this.init.output({timestamp:chunk.timestamp,numberOfChannels:2,numberOfFrames:800,sampleRate:48000,
      copyTo(){},close(){}})}
  }
  class AudioContext {
    state='running';destination={};
    get currentTime(){return now/1000}
    async close(){this.state='closed'}
    async resume(){this.state='running'}
    async suspend(){this.state='suspended'}
    createBuffer(){return {getChannelData:()=>new Float32Array(800)}}
    createBufferSource(){const node={stopped:false,connect(){},disconnect(){},start(at){this.at=at},stop(){this.stopped=true}};audioNodes.push(node);return node}
  }
  const canvas={hidden:true,width:0,height:0,dataset:{},getContext:()=>({drawImage(frame){drawn.push({at:now,pts:frame.timestamp})}})};
  const sound={},status={},video={};
  const context={URL,Uint8Array,DataView,TextDecoder,ReadableStream,console,AudioContext,VideoDecoder,AudioDecoder,
    WebTransport:Transport,EncodedVideoChunk:class {constructor(init){Object.assign(this,init)}},
    EncodedAudioChunk:class {constructor(init){Object.assign(this,init)}},
    performance:{now:()=>now},document:{querySelector:s=>s==='canvas'?canvas:s==='#sound'?sound:status},
    requestAnimationFrame:f=>{const id=++serial;callbacks.set(id,f);return id},cancelAnimationFrame:id=>callbacks.delete(id),
    setTimeout:(f,delay)=>{const id=++serial;timers.set(id,{f,at:now+delay});return id},clearTimeout:id=>timers.delete(id)};
  context.window={isSecureContext:true,WebTransport:Transport,VideoDecoder,AudioDecoder};
  vm.createContext(context);vm.runInContext(player,context);
  let fallbacks=0;
  const stop=context.periWebTransport(video,{url:'https://localhost:4444'},true,()=>fallbacks++);
  function push(controller,pts,key){
    const bytes=new Uint8Array(18),h=new DataView(bytes.buffer);
    h.setUint32(0,1);h.setUint8(4,key?1:0);h.setBigUint64(5,BigInt(Math.round(pts)));h.setUint32(13,duration);bytes[17]=42;
    controller.enqueue(bytes);
  }
  return {drawn,audioNodes,created,stop,
    get decoder(){return videoDecoder},
    video:(pts,key=false)=>push(controllerVideo,pts,key),audio:pts=>push(controllerAudio,pts,true),
    get liveFrames(){return liveFrames},get fallbacks(){return fallbacks},
    render(at){now=at;const pending=[...callbacks.values()];callbacks.clear();for(const f of pending)f(at);
      for(const [id,t] of [...timers])if(t.at<=now){timers.delete(id);t.f()}},
  };
}

test('late join does not evict every decoded frame before its presentation time',async()=>{
  const f=fixture();await flush();
  // 150 ms of cached media fits the server hot GOP but exceeds the six-frame display queue.
  // 服务端 150 ms 热 GOP 合法，却超过客户端六帧显示队列。
  for(let i=0;i<10;i++)f.video(1_000_000+i*16667,i===0);
  await flush();assert.ok(f.liveFrames<=6);
  for(let i=1;i<=120;i++){
    f.render(i*1000/60);
    f.video(1_000_000+(9+i)*16667,(9+i)%15===0);
    await flush();assert.ok(f.liveFrames<=6);
  }
  const rendered=f.drawn.length;f.stop();
  assert.equal(f.liveFrames,0);assert.equal(f.fallbacks,0);
  assert.ok(rendered>=110,`decoded 130 frames but rendered only ${rendered}`);
});

test('ordinary live pacing remains bounded and releases decoded frames on stop',async()=>{
  const f=fixture();await flush();
  for(let i=0;i<120;i++){
    f.render(i*1000/60);f.video(1_000_000+i*16667,i%15===0);
    await flush();assert.ok(f.liveFrames<=6);
  }
  assert.ok(f.drawn.length>=110);f.stop();assert.equal(f.liveFrames,0);
});

test('catching up a cached GOP cancels audio scheduled against the previous clock',async()=>{
  const f=fixture();await flush();f.video(1_000_000,true);await flush();
  f.audio(1_000_000);await flush();assert.equal(f.audioNodes.length,1);
  const oldAudio=f.audioNodes[0];assert.equal(oldAudio.stopped,false);
  for(let i=1;i<10;i++)f.video(1_000_000+i*16667);
  await flush();assert.equal(oldAudio.stopped,true);
  f.audio(1_150_003);await flush();assert.equal(f.audioNodes.length,2);
  const newAudio=f.audioNodes[1];assert.equal(newAudio.stopped,false);
  f.stop();assert.equal(newAudio.stopped,true);assert.equal(f.liveFrames,0);
});

test('a discontinuous recovery key releases old decoded frames and scheduled audio',async()=>{
  const f=fixture();await flush();f.video(1_000_000,true);await flush();
  f.audio(1_000_000);await flush();
  f.render(1000); // Advance past the old timeline. / 推进到旧时间轴之外。
  f.video(1_016_667);await flush();
  f.video(1_033_334,true);await flush();
  assert.equal(f.liveFrames,1);assert.equal(f.audioNodes[0].stopped,true);
  f.render(1060);assert.equal(f.drawn.at(-1).pts,1_033_334);
  f.stop();assert.equal(f.liveFrames,0);
});

test('repeated 150 ms arrival gaps grow a bounded playout reserve instead of repeatedly dropping bursts',async()=>{
  const f=fixture();await flush();
  let held=[];
  for(let i=0;i<240;i++){
    f.render(i*1000/60);
    held.push(i);
    // Nine delayed frames followed by their catch-up batch, once per second.
    if(i%60<30 || i%60>=39){
      for(const n of held)f.video(1_000_000+n*16667,n===0);
      held=[];
    }
    await flush();assert.ok(f.liveFrames<=20);
  }
  for(let i=240;i<260;i++)f.render(i*1000/60);
  assert.ok(f.drawn.length>=220,`rendered ${f.drawn.length} of 240 frames`);
  assert.ok(f.drawn.every(x=>x.at-(x.pts-1_000_000)/1000<300),'playout reserve accumulated beyond its bound');
  assert.equal(f.fallbacks,0);f.stop();assert.equal(f.liveFrames,0);
});

test('an overstated frame duration cannot starve a full adaptive queue',async()=>{
  const f=fixture(66666);await flush();let held=[];
  for(let i=0;i<180;i++){
    f.render(i*1000/30);held.push(i);
    if(i%30<10 || i%30>=16){for(const n of held)f.video(1_000_000+n*33333,n===0);held=[];}
    await flush();assert.ok(f.liveFrames<=20);
  }
  for(let i=180;i<195;i++)f.render(i*1000/30);
  assert.ok(f.drawn.length>=150,`rendered only ${f.drawn.length}`);
  assert.equal(f.fallbacks,0);f.stop();assert.equal(f.liveFrames,0);
});

test('a temporary native decode backlog drains without destroying its reference chain',async()=>{
  const f=fixture();await flush();f.video(1_000_000,true);await flush();
  f.decoder.decodeQueueSize=8;f.video(1_016_667);await flush();
  assert.equal(f.created.length,1);assert.equal(f.decoder.resets,0);
  f.decoder.decodeQueueSize=0;f.render(10);await flush();
  assert.equal(f.created.length,2);assert.equal(f.decoder.resets,0);f.stop();assert.equal(f.liveFrames,0);
});

test('a decoder that stays blocked is reset after the bounded drain wait',async()=>{
  const f=fixture();await flush();f.video(1_000_000,true);await flush();
  f.decoder.decodeQueueSize=8;f.video(1_016_667);await flush();f.render(110);await flush();
  assert.equal(f.decoder.resets,1);assert.equal(f.created.length,1);
  f.video(1_033_334);await flush();assert.equal(f.created.length,1);
  f.video(1_050_001,true);await flush();assert.equal(f.created.length,2);f.stop();assert.equal(f.liveFrames,0);
});
