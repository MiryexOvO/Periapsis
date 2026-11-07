// Transport receipts, decoder output and drawing are distinct observations.
// 传输确认、解码输出和绘制分别记录，不把缺失阶段推断为成功。
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
export function correlateKeys(events,viewers){
  const starts=events.filter(e=>e.event==='source_key_start');
  return starts.map(start=>{
    const pts=start.data.pts_us;
    const matching=name=>events.filter(e=>e.event===name&&e.data.pts_us===pts);
    return {pts,sequence:start.data.sequence,bytes:start.data.bytes,budgetMs:start.data.budget_ms,
      sourceStart:start.wall_ms,sourceReceipts:matching('source_key_receipt'),
      encoded:matching('encoder_key_observed'),relayReceived:matching('relay_key_received'),
      gatewayReceived:matching('gateway_key_received'),gatewayWrites:matching('gateway_key_write'),
      viewers:viewers.map(v=>({keyInput:v.decoder?.keyInputs?.find(f=>f.pts===pts)??null,
        keyOutput:v.decoder?.keyOutputs?.find(f=>f.pts===pts)??null,
        exactKeyDraw:v.frames?.find(f=>f.pts===pts&&f.transport==='webcodecs')??null}))};
  });
}
export function correlateProducerKeys(events,keys){
  return events.filter(e=>e.event==='key_encoded').map(encoded=>{
    const matching=name=>events.find(e=>e.event===name&&e.data.key_id===encoded.data.key_id)??null;
    const admitted=matching('key_admitted'),rejected=matching('key_rejected'),superseded=matching('key_superseded');
    const sendBegin=matching('key_send_begin'),sendEnd=matching('key_send_end');
    // A rejected key can share PTS zero with a later successful join. Require its own send interval.
    const transport=sendBegin&&sendEnd?keys.find(k=>k.pts===sendBegin.data.pts_us&&
      k.sourceStart>=sendBegin.wall_ms&&k.sourceStart<=sendEnd.wall_ms)??null:null;
    return {keyId:encoded.data.key_id,encoded,admitted,rejected,superseded,sendBegin,sendEnd,transport};
  });
}
export function writeTimeline(dir){
  const report=JSON.parse(fs.readFileSync(path.join(dir,'report.json')));
  const events=[];
  for(const name of ['source','relay','gateway'])for(const line of fs.readFileSync(path.join(dir,name+'.log'),'utf8').split(/\r?\n/)){
    const trace=line.match(/PERI_TRACE (\{.*\})/);if(trace)try{events.push({...JSON.parse(trace[1]),component:name})}catch{}
    const observed=line.match(/Recovery key observed.*origin=(\d+) id=(\d+) elapsed_ms=(\d+) pts_us=(\d+) wall_ms=(\d+)/);
    if(observed)events.push({event:'encoder_key_observed',wall_ms:Number(observed[5]),data:{origin:Number(observed[1]),id:observed[2],pts_us:Number(observed[4]),elapsed_ms:Number(observed[3])}});
    const submitted=line.match(/Recovery submitted.*origin=(\d+) id=(\d+) viewer=(\d+) after=(\d+) wall_ms=(\d+)/);
    if(submitted)events.push({event:'encoder_request',wall_ms:Number(submitted[5]),data:{origin:Number(submitted[1]),id:submitted[2],viewer:Number(submitted[3]),after_sequence:Number(submitted[4])}});
  }
  const viewers=report.viewers.map((_,i)=>JSON.parse(fs.readFileSync(path.join(dir,`viewer-${i}.json`))));
  // Transcoding introduces a different PTS domain; never fabricate the cross-codec join.
  // 转码有独立时间轴，不伪造跨编码器逐帧对应关系。
  const keys=correlateKeys(events,report.quality==='source'?viewers:[]);
  const producerKeys=correlateProducerKeys(events,keys);
  const result={quality:report.quality,scope:report.quality==='source'?'source PTS correlation':'source stages; transcoded browser stages separate',
    keys,producerKeys,queueAdvances:events.filter(e=>e.event==='queue_advance_to_key'),requests:events.filter(e=>['encoder_request','relay_recovery','recovery_demand'].includes(e.event)),
    producerTraceLost:fs.readFileSync(path.join(dir,'source.log'),'utf8').split(/\r?\n/).reduce((n,l)=>n+Number(l.match(/Key trace overflow: lost=(\d+)/)?.[1]??0),0),
    browserKeys:viewers.map(v=>({inputs:v.decoder?.keyInputs,outputs:v.decoder?.keyOutputs})),
    counts:{encoded:producerKeys.length,admitted:producerKeys.filter(k=>k.admitted).length,rejected:producerKeys.filter(k=>k.rejected).length,
      started:keys.length,relay:keys.filter(k=>k.relayReceived.length).length,
      gateway:keys.filter(k=>k.gatewayReceived.length).length,decoded:keys.filter(k=>k.viewers.some(v=>v.keyOutput)).length,
      exactDraw:keys.filter(k=>k.viewers.some(v=>v.exactKeyDraw)).length}};
  fs.writeFileSync(path.join(dir,'recovery-timeline.json'),JSON.stringify(result,null,2));return result.counts;
}
if(process.argv[1]&&import.meta.url===pathToFileURL(path.resolve(process.argv[1])).href)
  for(const dir of process.argv.slice(2))console.log(JSON.stringify({directory:dir,...writeTimeline(dir)}));
