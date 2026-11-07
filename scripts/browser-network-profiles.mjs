// Independent uplink factors keep loss, delay and serialization distinguishable.
// 独立上行因素分别测丢包、延迟、容量和断流，保留原有复合压力配方。
export const uplinkCases=['uplink','uplink-delay','uplink-loss','uplink-capacity','uplink-blackout'];
export function browserProfile(name,quality){
  const uplink=uplinkCases.includes(name);
  const p={direction:uplink?'up':'down',bytesPerSecond:(uplink||quality==='source'?25:5)*1e6/8,delayMs:45,loss:.01,blackoutMs:180};
  if(name==='slow-viewer')return {...p,bytesPerSecond:.4e6/8,loss:0,blackoutMs:0};
  if(uplink && name!=='uplink'){
    Object.assign(p,{bytesPerSecond:Number.MAX_SAFE_INTEGER,delayMs:0,loss:0,blackoutMs:0});
    if(name==='uplink-delay')p.delayMs=45;
    if(name==='uplink-loss')p.loss=.01;
    if(name==='uplink-capacity')p.bytesPerSecond=8e6/8;
    if(name==='uplink-blackout')p.blackoutMs=180;
  }
  return p;
}
