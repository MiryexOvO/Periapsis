import { spawn } from "node:child_process";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
// 串行执行，避免编码器争抢CPU污染延迟结果；单项失败仍保留结果并完成其他配方。
await mkdir("dist",{recursive:true});
const directory=await mkdtemp(resolve("dist/matrix-"));
const results=[];
const selectedProfiles=(process.env.PERI_MATRIX_PROFILES ?? "capacity,delay,loss,burst").split(",");
const selectedRates=(process.env.PERI_MATRIX_RATE_CONTROL ?? "vbr").split(",");
const selectedFairness=(process.env.PERI_MATRIX_SEND_FAIRNESS ?? process.env.PERI_SEND_FAIRNESS ?? "1").split(",");
const selectedAckModes=(process.env.PERI_MATRIX_ACK_MODES ?? process.env.PERI_ACK_MODE ?? "default").split(",");
if(selectedAckModes.some(mode=>mode!=="default" && mode!=="frequent")) throw new Error("invalid ack mode");
if(selectedFairness.some(value=>value!=="0" && value!=="1")) throw new Error("invalid send fairness");
if(selectedRates.some(r=>!["vbr","cbr"].includes(r))) throw new Error("invalid rate control");
const selectedIntra=(process.env.PERI_MATRIX_INTRA ?? "0,100").split(",");
if(selectedProfiles.some(p=>!["capacity","delay","loss","burst"].includes(p)) || selectedIntra.some(p=>!["0","100"].includes(p))) throw new Error("invalid matrix selection");
for (const [ackOrder,ackMode] of selectedAckModes.entries()) for (const [fairOrder,fairness] of selectedFairness.entries()) for (const rateControl of selectedRates) for (const intra of selectedIntra) for (const profile of selectedProfiles) {
  const child=spawn(process.execPath,["--experimental-strip-types","scripts/native-live-check.ts"],{windowsHide:true,env:{...process.env,PERI_ACK_MODE:ackMode,PERI_SEND_FAIRNESS:fairness,PERI_RATE_CONTROL:rateControl,PERI_NETWORK_PROFILE:profile,PERI_NETWORK_MODES:"fixed",PERI_NETWORK_REPEATS:"1",PERI_RECOVERY_MS:"333",PERI_MAX_INTRA_PCT:intra,PERI_RECOVERY_READ_MS:"300",PERI_REQUIRE_SUBSECOND:"0"},stdio:["ignore","pipe","pipe"]});
  let output="";
  for(const stream of [child.stdout,child.stderr]) stream.on("data",data=>{output+=String(data);process.stdout.write(data);});
  const code=await new Promise<number|null>((resolve,reject)=>{child.on("error",reject);child.on("close",resolve);});
  // 序号防止ABBA里重复模式覆盖前一轮日志。
  await writeFile(resolve(directory,`run${ackOrder+1}-${fairOrder+1}-${profile}-${rateControl}-intra${intra}-fairness${fairness}-ack${ackMode}.log`),output);
  results.push({profile,rateControl,ackMode,ackOrder:ackOrder+1,sendFairness:Number(fairness),maxIntraPct:Number(intra),code,artifact:output.match(/Native service artifacts: (.+)/)?.[1]?.trim() ?? null});
  await writeFile(resolve(directory,"matrix-report.json"),JSON.stringify(results,null,2));
}
console.log(`Matrix artifacts: ${directory}`);
if(results.some(result=>result.code!==0)) process.exitCode=1;
