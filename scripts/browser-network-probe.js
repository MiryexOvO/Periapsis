// Instrumentation injected only by the test runner; not included in the shipped page.
// 仅由测试脚本注入的观测探针，不包含在交付页面中。
(() => {
  const frames=[],ticks=[],events=[];
  const clock=()=>performance.timeOrigin+performance.now();
  const pixelDebug=[];
  const state=window.__periProbe={frames,ticks,events,pixelDebug};
  const raf=state.raf={count:0,maxGapMs:0};let lastRaf;
  function observeRaf(at){raf.count++;if(lastRaf!==undefined)raf.maxGapMs=Math.max(raf.maxGapMs,at-lastRaf);lastRaf=at;requestAnimationFrame(observeRaf)}
  requestAnimationFrame(observeRaf);
  const decoder=state.decoder={keys:0,deltas:0,outputs:0,maxQueue:0,resets:[],errors:[]};
  decoder.keyInputs=[];decoder.keyOutputs=[];const keyPts=new Set();
  decoder.durations={};
  if(window.VideoDecoder){
    const Native=window.VideoDecoder;
    window.VideoDecoder=class extends Native {
      constructor(init){super({...init,output:frame=>{decoder.outputs++;decoder.durations[String(frame.duration)]=(decoder.durations[String(frame.duration)]??0)+1;if(keyPts.delete(frame.timestamp)&&decoder.keyOutputs.length<2000)decoder.keyOutputs.push({at:clock(),pts:frame.timestamp});init.output(frame)},
        error:error=>{decoder.errors.push({at:clock(),message:error.message});init.error(error)}})}
      decode(chunk){decoder[chunk.type==='key'?'keys':'deltas']++;if(chunk.type==='key'&&decoder.keyInputs.length<2000){decoder.keyInputs.push({at:clock(),pts:chunk.timestamp});keyPts.add(chunk.timestamp)}decoder.maxQueue=Math.max(decoder.maxQueue,this.decodeQueueSize);return super.decode(chunk)}
      reset(){if(decoder.resets.length<500)decoder.resets.push({at:clock(),queue:this.decodeQueueSize});return super.reset()}
    };
  }
  const context=CanvasRenderingContext2D.prototype;
  const original=context.drawImage;
  let lastPts=-1,videoStarted=false;
  function record(pts,transport){
    if(pts===lastPts)return;lastPts=pts;
    if(frames.length<20000)frames.push({at:clock(),pts,transport});
  }
  context.drawImage=function(source,...args){
    const result=original.call(this,source,...args);
    if(this.canvas===document.querySelector('main>canvas') && !this.canvas.hidden)
      record(source.timestamp??Number(this.canvas.dataset.pts),'webcodecs');
    return result;
  };
  const scratch=document.createElement('canvas');scratch.width=40;scratch.height=1;
  const ctx=scratch.getContext('2d',{willReadFrequently:true});
  state.timer=setInterval(()=>{
    const video=document.querySelector('video'),canvas=document.querySelector('main>canvas');
    if(!video||!canvas)return;
    if(!videoStarted && video.requestVideoFrameCallback){
      videoStarted=true;
      const output=(_,metadata)=>{
        if(!video.hidden)record(metadata.mediaTime*1e6,video.dataset.transport??'unknown');
        video.requestVideoFrameCallback(output);
      };video.requestVideoFrameCallback(output);
    }
    const transport=video.dataset.transport??'unknown';
    const source=!canvas.hidden?canvas:video;
    const width=source===canvas?canvas.width:video.videoWidth;
    let lagMs=null;
    if(width && (source===canvas || video.readyState>=2))try{
      scratch.width=width;
      original.call(ctx,source,0,5,width,1,0,0,width,1);
      const pixels=ctx.getImageData(0,0,width,1).data;
      const bit=i=>pixels[4*Math.floor((i+.5)*width/40)]>128;
      if(pixelDebug.length<5)pixelDebug.push(Array.from({length:40},(_,i)=>pixels[4*Math.floor((i+.5)*width/40)]));
      if(Array.from({length:8},(_,i)=>bit(i)===(i%2===0)).every(Boolean)){
        let stamp=0,valid=true;
        for(let i=0;i<16;i++){if(bit(8+i)===bit(24+i))valid=false;stamp=stamp*2+Number(bit(8+i));}
        if(valid)lagMs=((Date.now()&65535)-stamp+65536)%65536;
      }
    }catch{}
    if(ticks.length<5000)ticks.push({at:clock(),lagMs,transport,rafCount:raf.count,decoded:decoder.outputs});
  },100);
  addEventListener('error',e=>events.push({at:clock(),error:e.message}));
})();
