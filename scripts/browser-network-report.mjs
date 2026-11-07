// Summarize retained diagnostics without treating a completed run as a quality pass.
// 汇总已保存的诊断，不把实验完成视为播放质量达标。
import fs from 'node:fs';
import path from 'node:path';
const dir=process.argv[2];
if(!dir)throw Error('usage: browser-network-report.mjs result-directory');
const rows=JSON.parse(fs.readFileSync(path.join(dir,'summary.json'),'utf8'));
const number=x=>x===null||x===undefined?'—':Math.round(x*10)/10;
console.log('| Case | Quality | Viewer | P95 ms | Freeze % | Max gap ms | FPS | Transports | Recovery <1s ms |');
console.log('|---|---|---:|---:|---:|---:|---:|---|---:|');
for(const row of rows){
  if(row.productAcceptance)console.log(`${row.name}/${row.quality} product ${row.productAcceptance.tier}: ${row.productAcceptance.status} ${[...row.productAcceptance.failures,...row.productAcceptance.missing].join('; ')}`);
  if(row.acceptance)console.log(`${row.name}/${row.quality} acceptance: ${row.acceptance.passed?'PASS':'FAIL'} ${row.acceptance.failures.join('; ')}`);
  if(row.status!=='measured')console.log(`${row.name}/${row.quality}: ${row.status}: ${row.error??''}`);
  for(const v of row.viewers){const d=v.damage;
    console.log(`| ${row.name} | ${row.quality} | ${v.index} | ${number(d.pixelP95Ms)} | ${number(d.freezeRatio*100)} | ${number(d.maxGapMs)} | ${number(d.renderFps)} | ${d.transports.join(',')} | ${number(v.recoveredUnder1sAfterMs)} |`);
  }
  const p=row.network?.publisher?.up?.damage;
  if(p)console.log(`Source offered UDP Mbps=${number(p.bytes*8/row.durationMs/1000)}; drops(random/blackout/queue)=${p.randomDrop}/${p.blackoutDrop}/${p.queueDrop}`);
  const processes=new Map();
  for(const sample of row.resources??[])for(const proc of sample.items??[]){
    if(proc.ProcessName==='conhost')continue;
    const previous=processes.get(proc.Id);
    processes.set(proc.Id,{name:proc.ProcessName,peakWorkingMiB:Math.max(previous?.peakWorkingMiB??0,proc.WorkingSet64/1048576),
      peakPrivateMiB:Math.max(previous?.peakPrivateMiB??0,proc.PrivateMemorySize64/1048576),
      cpuFirst:previous?.cpuFirst??proc.CPU,cpuLast:proc.CPU,first:previous?.first??sample.at,last:sample.at});
  }
  console.log('Sampled processes '+JSON.stringify([...processes.values()].map(p=>({name:p.name,peakWorkingMiB:number(p.peakWorkingMiB),
    peakPrivateMiB:number(p.peakPrivateMiB),meanCpuCores:number((p.cpuLast-p.cpuFirst)/((p.last-p.first)/1000))}))));
}
