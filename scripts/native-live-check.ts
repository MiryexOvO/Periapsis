import { profiles, seededRandom } from "./network-profiles.ts";
import dgram from "node:dgram";
import { reserveDatagram } from "./network-queue.ts";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, access, writeFile, stat, readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { randomBytes, createHash } from "node:crypto";

const strictValue = process.env.PERI_REQUIRE_SUBSECOND ?? "0";
if (strictValue !== "0" && strictValue !== "1") throw new Error("PERI_REQUIRE_SUBSECOND must be 0 or 1");
const requireSubsecond = strictValue === "1";
const profileName=process.env.PERI_NETWORK_PROFILE ?? "short";
if (!Object.hasOwn(profiles, profileName)) throw new Error("unknown network profile");
const profile=profiles[profileName as keyof typeof profiles];
const frameCount=profileName==="short" ? 60 : 931;
const selectedModes=(process.env.PERI_NETWORK_MODES ?? "fixed,legacy,coalesced").split(",");
if (!selectedModes.length || selectedModes.some(mode=>!["fixed","legacy","coalesced"].includes(mode))) throw new Error("invalid modes");
const root = process.cwd();
await mkdir(resolve(root, "dist"), { recursive: true });
const directory = await mkdtemp(resolve(root, "dist/live-check-"));
const binary = resolve(process.env.CARGO_TARGET_DIR ?? resolve(root, "target"), "debug", process.platform === "win32" ? "peri.exe" : "peri");
const publishToken = randomBytes(24).toString("hex"), subscribeToken = randomBytes(24).toString("hex");
const environment: NodeJS.ProcessEnv = { ...process.env, PERI_PUBLISH_TOKEN: publishToken, PERI_SUBSCRIBE_TOKEN: subscribeToken };
const children: ReturnType<typeof start>[] = [];
function start(command: string, args: string[], env: NodeJS.ProcessEnv = environment) {
  const child = spawn(command, args, { cwd: root, env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  const state = { output: "" };
  for (const stream of [child.stdout!, child.stderr!]) stream.on("data", bytes => { state.output = (state.output + String(bytes)).slice(-8 * 1024 * 1024); });
  const done = new Promise<number | null>((resolve, reject) => { child.once("error", reject); child.once("close", resolve); });
  void done.catch(() => {});
  return { child, state, done };
}
async function finish(process: ReturnType<typeof start>, timeout = 15000) {
  let timer: ReturnType<typeof setTimeout>;
  try { return await Promise.race([process.done, new Promise<never>((_, reject) => { timer = setTimeout(() => { process.child.kill(); reject(new Error(`process timed out: ${process.state.output}`)); }, timeout); })]); }
  finally { clearTimeout(timer!); }
}
async function run(command: string, args: string[], env: NodeJS.ProcessEnv = environment, expectSuccess = true, timeout = 15000) {
  const process = start(command, args, env); children.push(process);
  const code = await finish(process, timeout);
  if ((code === 0) !== expectSuccess) throw new Error(`Unexpected exit ${code}: ${process.state.output}`);
  return process.state.output;
}
async function waitFor(process: ReturnType<typeof start>, text: string, count = 1) {
  const deadline = performance.now() + 15000;
  while (process.state.output.split(text).length - 1 < count) {
    if (process.child.exitCode !== null || performance.now() >= deadline) throw new Error(`Missing ${text}: ${process.state.output}`);
    await new Promise(resolve => setTimeout(resolve, 25));
  }
}
const ffmpeg = process.env.PERI_FFMPEG ?? "ffmpeg";
async function decoded(path: string) {
  const output = await run(ffmpeg, ["-hide_banner", "-loglevel", "error", "-i", path, "-map", "0:v:0", "-vsync", "0", "-f", "framemd5", "pipe:1"], environment, true, 60000);
  return output.split(/\r?\n/).filter(line => line.trim() && !line.startsWith("#")).map(line => line.split(",").at(-1)!.trim());
}
try {
  await run("cargo", ["build", "--locked", "-p", "peri-native", "--bin", "peri"], environment, true, 120000);
  const certDir = resolve(directory, "tls"), wrongDir = resolve(directory, "wrong-tls");
  await run(binary, ["init", certDir]); await run(binary, ["init", wrongDir]);
  const cert = resolve(certDir, "cert.der"), key = resolve(certDir, "key.key");
  const resource = "live/process-av1";
  const server = start(binary, ["relay", "127.0.0.1:0", cert, key, resource]); children.push(server);
  await waitFor(server, "PERI_RELAY_READY");
  const address = server.state.output.match(/PERI_RELAY_READY ([^\s]+)/)?.[1]; if (!address) throw new Error("missing server address");
  const publishArgs = ["live", address, "localhost", cert, resource, resolve(root, "scripts/live-av1-worker.py"), "13500"];
  let publisher = start(binary, publishArgs); children.push(publisher);
  await waitFor(server, "PERI_RESOURCE_READY"); await waitFor(publisher, "PERI_PUBLISH_READY");
  const denied = resolve(directory, "denied.ivf");
  const receiveArgs = ["receive", address, "localhost", cert, resource, denied, "1"];
  await run(binary, receiveArgs, { ...environment, PERI_SUBSCRIBE_TOKEN: publishToken }, false);
  await run(binary, publishArgs, { ...environment, PERI_PUBLISH_TOKEN: subscribeToken }, false);
  await run(binary, receiveArgs, { ...environment, PERI_SUBSCRIBE_TOKEN: "x".repeat(48) }, false);
  const badCert = [...receiveArgs]; badCert[3] = resolve(wrongDir, "cert.der"); await run(binary, badCert, environment, false);
  const badName = [...receiveArgs]; badName[2] = "wrong.invalid"; await run(binary, badName, environment, false);
  await run(binary, publishArgs, environment, false);
  let exists = true; try { await access(denied); } catch { exists = false; }
  if (exists) throw new Error("rejected viewer created output");
  console.log("Verified role isolation, wrong credentials/certificate/name rejection and exclusive publisher");
  const encoded = resolve(directory, "encoder.ivf");
  await run(process.env.PERI_PYTHON ?? "python", [resolve(root, "scripts/live-av1-worker.py"), "--check", encoded]);
  if ((await decoded(encoded)).length !== 30) throw new Error("live encoder decode mismatch");
  const pooled=resolve(directory,"pooled.ivf");
  await run(process.env.PERI_PYTHON ?? "python",[resolve(root,"scripts/live-av1-worker.py"),"--pool-check",pooled],{...environment,PERI_ENCODER_POOL:"1"});
  if((await decoded(pooled)).length!==90) throw new Error("pool switch decode failed");
  const quality=[];
  // 画质对照保持相同原始像素：同时测量默认编码与关键帧限幅，避免只展示低延迟。
  for (const intraPct of [0,100]) for (const target of [300000,100000]) {
    const path=resolve(directory,`quality-${target}-${intraPct}.ivf`);
    await run(process.env.PERI_PYTHON ?? "python",[resolve(root,"scripts/live-av1-worker.py"),"--check",path],{...environment,PERI_TEST_BITRATE:String(target),PERI_MAX_INTRA_PCT:String(intraPct)});
    const psnr=await run(ffmpeg,["-hide_banner","-loglevel","info","-i",path,"-f","rawvideo","-pixel_format","yuv420p","-video_size","320x180","-framerate","15","-i",path+".yuv","-lavfi","psnr","-f","null","-"],environment,true,60000);
    const value=psnr.match(/average:([\d.]+|inf)/)?.[1]; if(!value) throw new Error("missing quality measurement");
    const ssim=await run(ffmpeg,["-hide_banner","-loglevel","info","-i",path,"-f","rawvideo","-pixel_format","yuv420p","-video_size","320x180","-framerate","15","-i",path+".yuv","-lavfi","ssim","-f","null","-"],environment,true,60000);
    const similarity=ssim.match(/All:([\d.]+)/)?.[1]; if(!similarity) throw new Error("missing SSIM");
    quality.push({rateControl:environment.PERI_RATE_CONTROL ?? "vbr",inputSha256:createHash("sha256").update(await readFile(path+".yuv")).digest("hex"),target,maxIntraPct:intraPct,psnr:value,ssim:Number(similarity),scope:"offline-source-30frames",encodedBytes:(await stat(path)).size});
  }
  await writeFile(resolve(directory,"quality-report.json"),JSON.stringify(quality,null,2));
  console.log(JSON.stringify({quality}));
  const startupReports = [];
  for (const mode of [[], ["--fifo"]]) {
    const output = await run(binary, ["play", address, "localhost", cert, resource, "30", "--headless", ...mode], environment, true, 30000);
    console.log(output.split(/\r?\n/).filter(line => line.startsWith("PERI_PLAY_DONE")).join("\n"));
    const summary = JSON.parse(output.match(/PERI_PLAY_DONE (.+)/)![1]!);
    if (!Number.isFinite(summary.first_frame_ms) || summary.first_frame_ms < 0) throw new Error("missing first-frame timing");
    startupReports.push({ mode: mode.length ? "fifo" : "deadline", ...summary, subsecondJoin: summary.first_frame_ms < 1000 });
  }
  await writeFile(resolve(directory,"startup-report.json"),JSON.stringify(startupReports,null,2));
  // Actual UDP proxy: per-direction queues are bounded; packets exceed neither
  // 64 queued datagrams nor a 250ms scheduled delay. Only the viewer path is impaired.
  const proxyReports = [];
  const repeats = Number(process.env.PERI_NETWORK_REPEATS ?? "3");
  if (!Number.isInteger(repeats) || repeats < 1 || repeats > 5) throw new Error("PERI_NETWORK_REPEATS must be 1..5");
  for (let round = 1; round <= repeats; round++) for (const mode of Array.from({length:selectedModes.length},(_,i)=>selectedModes[(i+round-1)%selectedModes.length]!)) {
    const ended = server.state.output.split("PERI_RESOURCE_ENDED").length - 1;
    publisher.child.kill(); await finish(publisher);
    await waitFor(server,"PERI_RESOURCE_ENDED", ended+1);
    const ready = server.state.output.split("PERI_RESOURCE_READY").length - 1;
    // Resource teardown can follow process exit by one event-loop turn.
    await new Promise(resolve => setTimeout(resolve,100));
    publisher = start(binary,publishArgs,{...environment,PERI_ADAPTIVE:mode!=="fixed" ? "1":"0",PERI_SWITCH_POLICY:mode==="coalesced" ? "coalesced":"legacy"}); children.push(publisher);
    await waitFor(server,"PERI_RESOURCE_READY",ready+1).catch(error=>{throw new Error(`${publisher.state.output}\n${error}`)});
    const traceStart=Date.now();
    const downstream = dgram.createSocket("udp4"), upstream = dgram.createSocket("udp4");
    await new Promise<void>(resolve => downstream.bind(0, "127.0.0.1", resolve));
    await new Promise<void>(resolve => upstream.bind(0, "127.0.0.1", resolve));
    const random=seededRandom(20261104+round);
    const timers = new Set<ReturnType<typeof setTimeout>>();
    let peer: dgram.RemoteInfo | undefined, started = 0, next = 0, packets = 0, lost = 0, queued = 0, peak = 0, wireBytes=0;
    downstream.on("message", (bytes, remote) => { peer = remote; upstream.send(bytes, Number(address.split(":")[1]), "127.0.0.1"); });
    upstream.on("message", bytes => {
      if (!peer) return;
      const elapsed = performance.now() - started;
      const impaired = started > 0 && elapsed < profile.durationMs;
      packets++; wireBytes+=bytes.length;
      const phase=profile.blackoutEveryMs ? elapsed % profile.blackoutEveryMs : elapsed;
      if (impaired && ((profile.blackoutMs>0 && phase>=profile.blackoutStartMs && phase<profile.blackoutStartMs+profile.blackoutMs) || (profile.every>0 && packets%profile.every===0) || random()<profile.loss)) { lost++; return; }
      const now = performance.now();
      const reservation = reserveDatagram(now, next, bytes.length, queued, impaired, profile.bytesPerSecond, profile.delayMs);
      if (!reservation) { lost++; return; }
      const { delay } = reservation;
      next = reservation.next;
      queued++; peak = Math.max(peak, queued);
      const timer = setTimeout(() => { timers.delete(timer); queued--; if (peer) downstream.send(bytes, peer.port, peer.address); }, delay);
      timers.add(timer);
    });
    try {
      const viewer = start(binary, ["play", `127.0.0.1:${(downstream.address() as import("node:net").AddressInfo).port}`, "localhost", cert, resource, String(frameCount), "--headless"]); children.push(viewer);
      await waitFor(viewer, "PERI_FRAME", 10); started = performance.now();
      if (await finish(viewer, profileName==="short" ? 30000 : 240000) !== 0) throw new Error(`Impaired playback failed: ${viewer.state.output}`);
      const summary = JSON.parse(viewer.state.output.match(/PERI_PLAY_DONE (.+)/)![1]!);
      if (profileName!=="short" && summary.playback_ms<60000) throw new Error("long trial did not cover 60 seconds of output");
      if (summary.decoded !== frameCount || summary.reconnects !== 0 || (profileName==="short" && lost===0)) throw new Error(`Live recovery regression: ${JSON.stringify(summary)}, dropped UDP packets: ${lost}`);
      const rates=[...publisher.state.output.matchAll(/PERI_ENCODED (\d+) (\d+) (\d+) (\d+)/g)].map(match=>({sequence:Number(match[1]),bytes:Number(match[2]),target:Number(match[3]),encodeUs:Number(match[4])}));
      const phases=publisher.state.output.split(/\r?\n/).filter(line=>line.startsWith("PERI_TRACE"));
      const traces=[...phases,...viewer.state.output.split(/\r?\n/),...server.state.output.split(/\r?\n/)]
        .filter(line=>line.startsWith("PERI_TRACE ")).map(line=>JSON.parse(line.slice(11))).filter(event=>event.wall_ms>=traceStart).sort((a,b)=>a.wall_ms-b.wall_ms);
      await writeFile(resolve(directory,`trace-${round}-${mode}.jsonl`),traces.map(event=>JSON.stringify(event)).join("\n")+"\n");
      const inputs=new Map(traces.filter(event=>event.event==="source_input").map(event=>[event.data.sequence,event.wall_ms]));
      const deltas=traces.filter(event=>event.event==="frame_output" && inputs.has(event.data.sequence)).map(event=>event.wall_ms-inputs.get(event.data.sequence)!);
      const negativeClockSamples=deltas.filter(value=>value<0).length;
      const latencies=deltas.filter(value=>value>=0).sort((a,b)=>a-b);
      // 长测必须保留几乎全部输入/输出配对，不能用日志末尾少量样本代替整段测量。
      if(latencies.length<Math.max(30,frameCount-2)) throw new Error("too few matched software latency samples");
      const softwareLatency={samples:latencies.length,p50Ms:latencies[Math.floor((latencies.length-1)*.5)],p95Ms:latencies[Math.floor((latencies.length-1)*.95)],maxMs:latencies.at(-1)!,negativeClockSamples};
      if (!Number.isFinite(summary.first_frame_ms) || summary.first_frame_ms < 0) throw new Error("missing first-frame timing");
      const subsecond={targetMs:1000,joinPass:summary.first_frame_ms<1000,softwarePass:softwareLatency.maxMs<1000 && negativeClockSamples===0,continuityPass:summary.max_gap_ms<1000,pass:summary.first_frame_ms<1000 && softwareLatency.maxMs<1000 && summary.max_gap_ms<1000 && negativeClockSamples===0};
      if (!phases.some(line=>line.includes('source_report'))) throw new Error("receiver telemetry did not reach encoder");
      proxyReports.push({ round, mode, softwareLatency,subsecond, targets:[...new Set(rates.map(rate=>rate.target))], rateSwitches:publisher.state.output.split("PERI_RATE_SWITCH").length-1, encodeMeanUs:Math.round(rates.reduce((total,rate)=>total+rate.encodeUs,0)/rates.length),encodeP95Us:rates.map(rate=>rate.encodeUs).sort((a,b)=>a-b)[Math.floor((rates.length-1)*0.95)], encodedBytes:rates.reduce((total,rate)=>total+rate.bytes,0), profile: { rateControl:environment.PERI_RATE_CONTROL ?? "vbr", name:profileName, seed:20261104+round, config:profile, recoveryReadMs:Number(environment.PERI_RECOVERY_READ_MS ?? 300), recoveryMs:Number(environment.PERI_RECOVERY_MS ?? 1000), maxIntraPct:Number(environment.PERI_MAX_INTRA_PCT ?? 0), queueModel: "admitted-datagrams-v2", downstreamBytesPerSecond: profile.bytesPerSecond, delayMs: profile.delayMs, durationMs: profile.durationMs }, packets, lost, wireBytes, trialMs:Math.round(performance.now()-started), peakQueuedDatagrams: peak, ...summary });
      await writeFile(resolve(directory,"network-report.json"),JSON.stringify(proxyReports,null,2));
      console.log(`PERI_TRIAL_DONE ${profileName} ${mode} gap=${summary.max_gap_ms} freeze=${summary.freeze_ms}`);
    } finally { for (const timer of timers) clearTimeout(timer); downstream.close(); upstream.close(); }
  }
  await writeFile(resolve(directory, "network-report.json"), JSON.stringify(proxyReports, null, 2));
  console.log(JSON.stringify(proxyReports));
  // Check every trial's actual independent responses using monotonic spacing as
  // observed in same-process trace timestamps (wall-clock sanity check with tolerance).
  const responses = publisher.state.output.split(/\r?\n/).filter(line=>line.startsWith("PERI_TRACE ")).map(line=>JSON.parse(line.slice(11))).filter(event=>event.event==="encoder_response");
  const cooldown=Number(environment.PERI_RECOVERY_MS ?? 1000);
  if(responses.some((event,index)=>index>0 && event.wall_ms-responses[index-1]!.wall_ms < cooldown-20)) throw new Error("excessive encoder independent responses");
  console.log(`Verified independent response cooldown ${cooldown}ms`);
  // Exercise the visible-player output path with SDL's headless driver in CI.
  await run(binary, ["play", address, "localhost", cert, resource, "15"], { ...environment, SDL_VIDEODRIVER: "dummy", SDL_AUDIODRIVER: "dummy" }, true, 15000);
  const endedBefore = server.state.output.split("PERI_RESOURCE_ENDED").length - 1;
  publisher.child.kill(); await finish(publisher);
  await waitFor(server, "PERI_RESOURCE_ENDED", endedBefore+1);
  const readyCount = server.state.output.split("PERI_RESOURCE_READY").length - 1;
  const replacement = start(binary, publishArgs); children.push(replacement);
  await waitFor(server, "PERI_RESOURCE_READY", readyCount + 1);
  await waitFor(replacement, "PERI_PUBLISH_READY");
  console.log("Verified resource release after publisher exit and subsequent republish");
  for (const process of children) if (process.state.output.includes(publishToken) || process.state.output.includes(subscribeToken)) throw new Error("credential leaked to logs");
  console.log(`Native service artifacts: ${directory}`);
  if (requireSubsecond) {
    const failed = proxyReports.filter(report => !report.subsecond.pass);
    if (failed.length || startupReports.some(report => !report.subsecondJoin)) {
      throw new Error(`Subsecond acceptance failed: ${failed.length}/${proxyReports.length} impaired trials; see saved reports in ${directory}`);
    }
    console.log(`PERI_SUBSECOND_ACCEPTED ${proxyReports.length} impaired trials; software output only`);
  }
} catch (error) {
  await writeFile(resolve(directory,"failure.json"),JSON.stringify({profile:profileName,error:String(error)},null,2));
  console.error(`Native service artifacts: ${directory}`);
  throw error;
} finally {
  for (const process of children) if (process.child.exitCode === null) process.child.kill();
  await Promise.allSettled(children.map(process => finish(process, 5000)));
}
