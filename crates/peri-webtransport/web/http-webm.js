// Push HTTP media into the demuxer as bytes arrive instead of relying on the
// native progressive-download buffering heuristic when scene bitrate collapses.
function periHttpWebm(video,url,onError){
  const mime='video/webm; codecs="av01.0.12M.08,opus"';
  if(!window.MediaSource || !MediaSource.isTypeSupported(mime))return null;
  video.dataset.httpDelivery='mse';
  const media=new MediaSource(),controller=new AbortController();
  const objectUrl=URL.createObjectURL(media);
  let stopped=false,buffer;
  function stop(){
    if(stopped)return;stopped=true;controller.abort();
    if(buffer?.updating && media.readyState==='open')try{buffer.abort()}catch{}
    URL.revokeObjectURL(objectUrl);
  }
  function wait(target,event,action){
    return new Promise((resolve,reject)=>{
      const cleanup=()=>{target.removeEventListener(event,done);target.removeEventListener('error',fail);controller.signal.removeEventListener('abort',cancel)};
      const done=()=>{cleanup();resolve()},fail=()=>{cleanup();reject(Error('HTTP media append failed'))},cancel=()=>{cleanup();reject(Error('HTTP media stopped'))};
      target.addEventListener(event,done,{once:true});target.addEventListener('error',fail,{once:true});controller.signal.addEventListener('abort',cancel,{once:true});
      if(controller.signal.aborted){cancel();return;}
      try{action?.()}catch(e){cleanup();reject(e)}
    });
  }
  (async()=>{
    await wait(media,'sourceopen',()=>{video.src=objectUrl});
    buffer=media.addSourceBuffer(mime);
    const response=await fetch(url,{cache:'no-store',signal:controller.signal});
    if(!response.ok || !response.body)throw Error('Live HTTP source unavailable');
    const reader=response.body.getReader();
    try{
      while(!stopped){
        // Do not accumulate unlimited future media when the viewer pauses.
        if(buffer.buffered.length && buffer.buffered.end(buffer.buffered.length-1)-video.currentTime>5){
          await new Promise(resolve=>setTimeout(resolve,100));continue;
        }
        const {value,done}=await reader.read();
        if(stopped)return;
        if(done)throw Error('Live HTTP source ended');
        await wait(buffer,'updateend',()=>buffer.appendBuffer(value));
        const keepFrom=video.currentTime-5;
        if(keepFrom>0 && buffer.buffered.length && buffer.buffered.start(0)<keepFrom)
          await wait(buffer,'updateend',()=>buffer.remove(0,keepFrom));
      }
    }finally{await reader.cancel().catch(()=>{});reader.releaseLock();}
  })().catch(error=>{if(!stopped){stop();onError(error)}});
  return stop;
}
