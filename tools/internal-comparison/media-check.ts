import { spawn } from "node:child_process";
import { mkdir,mkdtemp,writeFile,readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { createHash } from "node:crypto";
// 同一编码夹具经过两种真实QUIC传输后，交给同一个FFmpeg解码校验像素；不计性能胜负。
await mkdir("dist",{recursive:true});
const directory=await mkdtemp(resolve("dist/media-interop-"));
const fixture=resolve(directory,"source.ivf"),probe=resolve(directory,"probe.json");
const native=resolve(directory,"native.ivf"),moq=resolve(directory,"moq.ivf");
const environment={...process.env,PERI_AV1_FIXTURE:fixture,PERI_AV1_PROBE:probe,PERI_NATIVE_OUTPUT:native,PERI_MOQ_OUTPUT:moq,PERI_NATIVE_RGB:resolve(directory,"native.rgb"),PERI_MOQ_RGB:resolve(directory,"moq.rgb"),PERI_MOQ_LIVE_RGB:resolve(directory,"moq-live.rgb")};
async function run(command:string,args:string[]) {
 const child=spawn(command,args,{env:environment,windowsHide:true,stdio:["ignore","pipe","pipe"]});
 let stdout="",stderr="";
 child.stdout.on("data",data=>stdout+=String(data)); child.stderr.on("data",data=>stderr+=String(data));
 const timer=setTimeout(()=>child.kill(),120000);
 try {
  const code=await new Promise<number|null>((resolve,reject)=>{child.on("error",reject);child.on("close",resolve);});
  if(code!==0) throw new Error(`${command} exited ${code}: ${stderr} ${stdout}`);
  return stdout;
 } finally {clearTimeout(timer);}
}
try {
 await run(process.env.PERI_PYTHON ?? "python",["tests/fixtures/live-av1-worker.py","--check",fixture]);
 await writeFile(probe,await run(process.env.PERI_FFPROBE ?? "ffprobe",["-v","error","-f","ivf","-select_streams","v:0","-show_frames","-show_entries","frame=key_frame,pts,pkt_size,width,height","-of","json",fixture]));
 await run("cargo",["test","-p","peri-native","--test","native","shared_av1_fixture","--","--ignored","--nocapture"]);
 await run("cargo",["test","--manifest-path","tools/internal-comparison/Cargo.toml","-p","peri-internal-comparison","--features","media-interop","--test","native_quic","--","--nocapture"]);
 const decoded:string[][]=[];
 for (const path of [fixture,native,moq]) {
  const output=await run(process.env.PERI_FFMPEG ?? "ffmpeg",["-v","error","-i",path,"-map","0:v:0","-fps_mode","passthrough","-f","framemd5","-"]);
  const hashes=output.split(/\r?\n/).filter(line=>line.trim()&&!line.startsWith("#")).map(line=>line.split(",").at(-1)!.trim());
  if(hashes.length!==30) throw new Error("decoded frame count mismatch");
  decoded.push(hashes);
 }
 if(decoded.some(hashes=>JSON.stringify(hashes)!==JSON.stringify(decoded[0]))) throw new Error("decoded pixels differ across transports");
 // 共享库解码器与单调时钟已经实际驱动两条路径；RGB也必须与源片一致。
 const reference=resolve(directory,"source.rgb");
 await run(process.env.PERI_FFMPEG ?? "ffmpeg",["-v","error","-i",fixture,"-fps_mode","passthrough","-pix_fmt","rgb24","-f","rawvideo",reference]);
 const rgbHashes=[];
 for(const name of ["source.rgb","native.rgb","moq.rgb"]) {
  const bytes=await readFile(resolve(directory,name));
  if(bytes.length!==30*320*180*3) throw new Error("RGB frame count mismatch");
  rgbHashes.push(createHash("sha256").update(bytes).digest("hex"));
 }
 if(new Set(rgbHashes).size!==1) throw new Error("shared decoder output pixels differ");
 const liveRgb=await readFile(resolve(directory,"moq-live.rgb"));
 const sourceRgb=await readFile(reference);
 if(!liveRgb.equals(sourceRgb.subarray(0,320*180*3))) throw new Error("live recovery keyframe pixels differ");
 await writeFile(resolve(directory,"report.json"),JSON.stringify({scope:"loopback-media-integrity-only",ffmpegCommand:process.env.PERI_FFMPEG ?? "ffmpeg",moqVersion:"moq-transport-22",backend:"moq-net-0.3.18",frames:30,sourceSha256:createHash("sha256").update(await readFile(fixture)).digest("hex"),decodedPixelsEqual:true,sharedDecoderClock:true,liveGroupRecovery:true,rgbSha256:rgbHashes[0],performanceComparison:false},null,2));
 console.log(`Shared AV1 media accepted: ${directory}`);
} catch(error) {
 await writeFile(resolve(directory,"failure.txt"),String(error)); throw error;
}
