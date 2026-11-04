import { spawn } from "node:child_process";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
// 串行执行，避免编码器争抢CPU污染延迟结果；单项失败仍保留结果并完成其他配方。
await mkdir("dist",{recursive:true});
const directory=await mkdtemp(resolve("dist/matrix-"));
const results=[];
const selectedProfiles=(process.env.PERI_MATRIX_PROFILES ?? "capacity,delay,loss,burst").split(",");
const selectedRates=(process.env.PERI_MATRIX_RATE_CONTROL ?? "vbr").split(",");
if(selectedRates.some(r=>!["vbr","cbr"].includes(r))) throw new Error("invalid rate control");
const selectedIntra=(process.env.PERI_MATRIX_INTRA ?? "0,100").split(",");
if(selectedProfiles.some(p=>!["capacity","delay","loss","burst"].includes(p)) || selectedIntra.some(p=>!["0","100"].includes(p))) throw new Error("invalid matrix selection");
for (const rateControl of selectedRates) for (const intra of selectedIntra) for (const profile of selectedProfiles) {
  const child=spawn(process.execPath,["--experimental-strip-types","scripts/native-live-check.ts"],{windowsHide:true,env:{...process.env,PERI_RATE_CONTROL:rateControl,PERI_NETWORK_PROFILE:profile,PERI_NETWORK_MODES:"fixed",PERI_NETWORK_REPEATS:"1",PERI_RECOVERY_MS:"333",PERI_MAX_INTRA_PCT:intra,PERI_RECOVERY_READ_MS:"300",PERI_REQUIRE_SUBSECOND:"0"},stdio:["ignore","pipe","pipe"]});
  let output="";
  for(const stream of [child.stdout,child.stderr]) stream.on("data",data=>{output+=String(data);process.stdout.write(data);});
  const code=await new Promise<number|null>((resolve,reject)=>{child.on("error",reject);child.on("close",resolve);});
  await writeFile(resolve(directory,`${profile}-${rateControl}-intra${intra}.log`),output);
  results.push({profile,rateControl,maxIntraPct:Number(intra),code,artifact:output.match(/Native service artifacts: (.+)/)?.[1] ?? null});
  await writeFile(resolve(directory,"matrix-report.json"),JSON.stringify(results,null,2));
}
console.log(`Matrix artifacts: ${directory}`);
if(results.some(result=>result.code!==0)) process.exitCode=1;
