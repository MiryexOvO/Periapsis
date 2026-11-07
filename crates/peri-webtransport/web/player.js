// Each uni-stream starts with one catalog track. The following 17-byte headers
// carry validated PERI timing, not container timestamps. / 每条单向流先声明轨道，包头直接使用 PERI 时间戳。
function periWebTransport(video,config,low,fallback) {
  if(!window.isSecureContext || !config.url || !window.WebTransport ||
     !window.VideoDecoder || !window.AudioDecoder) return null;
  const address=new URL(low?'/720':'/source',config.url);
  const options=config.hash?{serverCertificateHashes:[{algorithm:'sha-256',value:Uint8Array.from(atob(config.hash),c=>c.charCodeAt(0))}]}:{};
  const wt=new WebTransport(address,options);
  const canvas=document.querySelector('canvas'),sound=document.querySelector('#sound');
  const status=document.querySelector('[role=status]'),ctx=canvas.getContext('2d',{alpha:false});
  const audio=new AudioContext({latencyHint:'interactive'});
  const decoders=[],frames=[],audioSources=new Set();
  let closed=false,timer,raf,anchorPts,anchorMs;
  let playoutMs=55;
  video.hidden=true;canvas.hidden=false;sound.hidden=false;
  sound.textContent='开启声音';
  sound.onclick=async()=>{if(audio.state==='running'){await audio.suspend();sound.textContent='开启声音';}
    else{await audio.resume();sound.textContent='静音';}};
  function stop(){
    if(closed)return;closed=true;clearTimeout(timer);cancelAnimationFrame(raf);
    for(const d of decoders) if(d.state!=='closed')d.close();
    while(frames.length)frames.pop().close();
    clearAudio();
    wt.close();audio.close().catch(()=>{});canvas.hidden=sound.hidden=true;video.hidden=false;sound.onclick=null;
  }
  function fail(e){if(!closed){console.debug('PERI WebCodecs fallback',e?.message);stop();fallback();}}
  function deadline(){clearTimeout(timer);timer=setTimeout(()=>fail(Error('media timeout')),5000);}
  function clearAudio(){
    for(const source of audioSources){source.onended=null;source.stop();source.disconnect();}
    audioSources.clear();
  }
  function rebase(pts,now,discardFrames=false){
    // Scheduled audio belongs to the old clock; it must not play after a rebase.
    // 已排程音频属于旧时钟，重定位后取消，避免继续播放旧时间映射的声音。
    clearAudio();
    if(discardFrames)while(frames.length)frames.pop().close();
    anchorPts=pts;anchorMs=now+playoutMs;
  }
  function wall(pts){
    if(anchorPts===undefined){anchorPts=pts;anchorMs=performance.now()+playoutMs;}
    return anchorMs+(pts-anchorPts)/1000;
  }
  function render(){
    if(closed)return;
    let newest;
    while(frames.length && wall(frames[0].timestamp)<=performance.now()){
      newest?.close();newest=frames.shift();
    }
    if(newest){if(canvas.width!==newest.displayWidth || canvas.height!==newest.displayHeight){
        canvas.width=newest.displayWidth;canvas.height=newest.displayHeight;}
      ctx.drawImage(newest,0,0);canvas.dataset.pts=newest.timestamp;newest.close();deadline();
      status.textContent='直播中 · WebTransport / WebCodecs';
    }
    raf=requestAnimationFrame(render);
  }
  // Read exactly N bytes even when QUIC splits headers or coalesces frames.
  // QUIC 可拆分头部或合并多个帧，按长度增量读取。
  function reader(stream){
    const r=stream.getReader();let rest=new Uint8Array();
    return async n=>{const out=new Uint8Array(n);let pos=0;
      while(pos<n){if(!rest.length){const next=await r.read();if(next.done)throw Error('track ended');rest=next.value;}
        const length=Math.min(rest.length,n-pos);out.set(rest.subarray(0,length),pos);rest=rest.subarray(length);pos+=length;}
      return out;};
  }
  async function track(stream){
    const read=reader(stream);const length=new DataView((await read(4)).buffer).getUint32(0);
    if(!length || length>16384)throw Error('invalid catalog');
    const t=JSON.parse(new TextDecoder().decode(await read(length)));const isVideo=t.kind==='video';
    const codec=t.codec==='video/av1'?'av01.0.12M.08':t.codec==='audio/opus'?'opus':t.codec;
    const settings=isVideo?{codec,optimizeForLatency:true}:{codec,sampleRate:t.sample_rate,numberOfChannels:t.channels};
    if(isVideo && t.width){settings.codedWidth=t.width;settings.codedHeight=t.height;}
    if(t.codec!=='video/av1' && t.init?.length)settings.description=Uint8Array.from(t.init);
    const Decoder=isVideo?VideoDecoder:AudioDecoder;
    if(!(await Decoder.isConfigSupported(settings)).supported)throw Error('unsupported codec');
    if(closed)return;
    function output(frame){
      if(isVideo){
        const arrival=performance.now(),late=arrival-wall(frame.timestamp);
        // Absorb repeated arrival gaps with at most 250 ms of playout reserve.
        // 在显示队列耗尽后增加有限缓冲，避免每次网络补发都丢弃整个追赶批次。
        if(!frames.length && late>30 && late<=220){
          const reserve=Math.min(250,playoutMs+late+30);
          if(reserve>playoutMs){
            clearAudio();anchorMs+=reserve-playoutMs;playoutMs=reserve;
          }
        }
        const capacity=Math.min(20,Math.max(6,Math.ceil(playoutMs*1000/(frame.duration||16667))+2));
        if(frames.length>=capacity){
          const oldest=frames.shift(),now=performance.now();
          // Make the next retained frame due now. Re-anchoring the newest frame
          // with another reserve can starve rendering if duration overstates cadence.
          // 队列满时让下一保留帧立即到期，避免错误时长或高帧率导致持续未来排程。
          if(wall(oldest.timestamp)>now){
            clearAudio();anchorMs-=wall(frames[0]?.timestamp??frame.timestamp)-now;
          }
          oldest.close();
        }
        frames.push(frame);
      }else{
        try{
          if(audio.state!=='running')return;
          const delay=(wall(frame.timestamp)-performance.now())/1000;
          if(delay<-.08 || delay>.35)return;
          const b=audio.createBuffer(frame.numberOfChannels,frame.numberOfFrames,frame.sampleRate);
          for(let i=0;i<frame.numberOfChannels;i++)frame.copyTo(b.getChannelData(i),{planeIndex:i,format:'f32-planar'});
          const s=audio.createBufferSource();s.buffer=b;s.connect(audio.destination);
          s.onended=()=>{audioSources.delete(s);s.disconnect();};
          s.start(audio.currentTime+Math.max(0,delay));audioSources.add(s);
        }finally{frame.close();}
      }
    }
    const decoder=new Decoder({output,error:fail});decoders.push(decoder);decoder.configure(settings);
    let needsKey=isVideo;
    while(!closed){
      const h=new DataView((await read(17)).buffer),size=h.getUint32(0),key=h.getUint8(4)!==0;
      if(!size || size>1048576)throw Error('invalid media length');
      const timestamp=Number(h.getBigUint64(5)),duration=h.getUint32(13),data=await read(size);
      if(closed)return;
      if(isVideo && decoder.decodeQueueSize>=8){
        // Yield to native decoder output before declaring a burst an overload.
        // 补发批次先给原生解码回调调度机会；只有持续堵塞才丢弃依赖链。
        const until=performance.now()+100;
        while(!closed && decoder.decodeQueueSize>=8 && performance.now()<until)
          await new Promise(resolve=>setTimeout(resolve,2));
        if(closed)return;
        if(decoder.decodeQueueSize>=8){decoder.reset();decoder.configure(settings);needsKey=true;}
      }
      if(isVideo && needsKey && !key)continue;
      if(!isVideo && decoder.decodeQueueSize>12)continue;

      if(isVideo && key && (anchorPts===undefined || wall(timestamp)-performance.now() < -220 ||
          wall(timestamp)-performance.now()>playoutMs+220)){
        rebase(timestamp,performance.now(),true);
      }
      needsKey=false;
      decoder.decode(new (isVideo?EncodedVideoChunk:EncodedAudioChunk)({type:key?'key':'delta',timestamp,duration,data}));
    }
  }
  deadline();raf=requestAnimationFrame(render);wt.closed.then(()=>fail(Error('session closed')),fail);
  (async()=>{await wt.ready;const streams=wt.incomingUnidirectionalStreams.getReader();
    for(let i=0;i<2;i++){const s=await streams.read();if(s.done)throw Error('missing media track');track(s.value).catch(fail);}
  })().catch(fail);
  return stop;
}
