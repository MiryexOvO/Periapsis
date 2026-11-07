// Measured encoded bytes, grouped by explicit fixture steps; targets are not measurements.
// 按夹具明确的阶跃分组实际编码字节，目标值不能替代测量。
import fs from 'node:fs';
import path from 'node:path';
for(const dir of process.argv.slice(2)){
  const report=JSON.parse(fs.readFileSync(path.join(dir,'clean-source/report.json')));
  const groups=[{target:report.encoderStepTest.initialKbps,samples:[]}];
  for(const line of fs.readFileSync(path.join(dir,'clean-source/source.log'),'utf8').split(/\r?\n/)){
    const step=line.match(/RATE_STEP.*requested=(\d+) settings=(\d+)/);
    if(step){groups.push({target:Number(step[1]),settings:Number(step[2]),samples:[]});continue;}
    const meter=line.match(/Encoded video.*kbps=(\d+)/);
    if(meter)groups.at(-1).samples.push(Number(meter[1]));
  }
  console.log(JSON.stringify({directory:dir,texture:report.source.texture,groups:groups.map(g=>{
    const stable=g.samples.slice(2,-1).sort((a,b)=>a-b);
    return {...g,stableSamples:stable.length,medianKbps:stable.length?stable[Math.floor(stable.length/2)]:null};
  })}));
}
