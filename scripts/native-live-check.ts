import dgram from "node:dgram";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, access, writeFile, stat } from "node:fs/promises";
import { resolve } from "node:path";
import { randomBytes } from "node:crypto";

const root = process.cwd();
await mkdir(resolve(root, "dist"), { recursive: true });
const directory = await mkdtemp(resolve(root, "dist/live-check-"));
const binary = resolve(process.env.CARGO_TARGET_DIR ?? resolve(root, "target"), "debug", process.platform === "win32" ? "peri.exe" : "peri");
const publishToken = randomBytes(24).toString("hex"), subscribeToken = randomBytes(24).toString("hex");
const environment = { ...process.env, PERI_PUBLISH_TOKEN: publishToken, PERI_SUBSCRIBE_TOKEN: subscribeToken };
const children: ReturnType<typeof start>[] = [];
function start(command: string, args: string[], env: NodeJS.ProcessEnv = environment) {
  const child = spawn(command, args, { cwd: root, env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  const state = { output: "" };
  for (const stream of [child.stdout!, child.stderr!]) stream.on("data", bytes => { state.output = (state.output + String(bytes)).slice(-128 * 1024); });
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
  for (const target of [300000,100000]) {
    const path=resolve(directory,`quality-${target}.ivf`);
    await run(process.env.PERI_PYTHON ?? "python",[resolve(root,"scripts/live-av1-worker.py"),"--check",path],{...environment,PERI_TEST_BITRATE:String(target)});
    const psnr=await run(ffmpeg,["-hide_banner","-loglevel","info","-i",path,"-f","rawvideo","-pixel_format","yuv420p","-video_size","320x180","-framerate","15","-i",path+".yuv","-lavfi","psnr","-f","null","-"],environment,true,60000);
    const value=psnr.match(/average:([\d.]+|inf)/)?.[1]; if(!value) throw new Error("missing quality measurement");
    quality.push({target,psnr:value,encodedBytes:(await stat(path)).size});
  }
  await writeFile(resolve(directory,"quality-report.json"),JSON.stringify(quality,null,2));
  console.log(JSON.stringify({quality}));
  for (const mode of [[], ["--fifo"]]) {
    const output = await run(binary, ["play", address, "localhost", cert, resource, "30", "--headless", ...mode], environment, true, 30000);
    console.log(output.split(/\r?\n/).filter(line => line.startsWith("PERI_PLAY_DONE")).join("\n"));
  }
  // Actual UDP proxy: per-direction queues are bounded; packets exceed neither
  // 64 queued datagrams nor a 250ms scheduled delay. Only the viewer path is impaired.
  const proxyReports = [];
  const repeats = Number(process.env.PERI_NETWORK_REPEATS ?? "3");
  if (!Number.isInteger(repeats) || repeats < 1 || repeats > 5) throw new Error("PERI_NETWORK_REPEATS must be 1..5");
  for (let round = 1; round <= repeats; round++) for (const mode of Array.from({length:3},(_,i)=>["fixed","legacy","coalesced"][(i+round-1)%3]!)) {
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
    const timers = new Set<ReturnType<typeof setTimeout>>();
    let peer: dgram.RemoteInfo | undefined, started = 0, next = 0, packets = 0, lost = 0, queued = 0, peak = 0, wireBytes=0;
    downstream.on("message", (bytes, remote) => { peer = remote; upstream.send(bytes, Number(address.split(":")[1]), "127.0.0.1"); });
    upstream.on("message", bytes => {
      if (!peer) return;
      const elapsed = performance.now() - started;
      const impaired = started > 0 && elapsed < 2000;
      packets++; wireBytes+=bytes.length;
      if (impaired && ((elapsed >= 300 && elapsed < 480) || packets % 11 === 0)) { lost++; return; }
      const now = performance.now();
      next = Math.max(now, next) + (impaired ? bytes.length / 24 : 0); // 24 kB/s
      const delay = Math.max(0, next - now) + (impaired ? 45 : 0);
      if (queued >= 64 || delay > 250) { lost++; return; }
      queued++; peak = Math.max(peak, queued);
      const timer = setTimeout(() => { timers.delete(timer); queued--; if (peer) downstream.send(bytes, peer.port, peer.address); }, delay);
      timers.add(timer);
    });
    try {
      const viewer = start(binary, ["play", `127.0.0.1:${(downstream.address() as import("node:net").AddressInfo).port}`, "localhost", cert, resource, "60", "--headless"]); children.push(viewer);
      await waitFor(viewer, "PERI_FRAME", 10); started = performance.now();
      if (await finish(viewer, 30000) !== 0) throw new Error(`Impaired playback failed: ${viewer.state.output}`);
      const summary = JSON.parse(viewer.state.output.match(/PERI_PLAY_DONE (.+)/)![1]!);
      if (summary.decoded !== 60 || summary.reconnects !== 0 || lost === 0) throw new Error(`Live recovery regression: ${JSON.stringify(summary)}, dropped UDP packets: ${lost}`);
      const rates=[...publisher.state.output.matchAll(/PERI_ENCODED (\d+) (\d+) (\d+) (\d+)/g)].map(match=>({sequence:Number(match[1]),bytes:Number(match[2]),target:Number(match[3]),encodeUs:Number(match[4])}));
      const phases=publisher.state.output.split(/\r?\n/).filter(line=>line.startsWith("PERI_TRACE"));
      const traces=[...phases,...viewer.state.output.split(/\r?\n/),...server.state.output.split(/\r?\n/)]
        .filter(line=>line.startsWith("PERI_TRACE ")).map(line=>JSON.parse(line.slice(11))).filter(event=>event.wall_ms>=traceStart).sort((a,b)=>a.wall_ms-b.wall_ms);
      await writeFile(resolve(directory,`trace-${round}-${mode}.jsonl`),traces.map(event=>JSON.stringify(event)).join("\n")+"\n");
      if (!phases.some(line=>line.includes('source_report'))) throw new Error("receiver telemetry did not reach encoder");
      proxyReports.push({ round, mode, targets:[...new Set(rates.map(rate=>rate.target))], rateSwitches:publisher.state.output.split("PERI_RATE_SWITCH").length-1, encodeMeanUs:Math.round(rates.reduce((total,rate)=>total+rate.encodeUs,0)/rates.length),encodeP95Us:rates.map(rate=>rate.encodeUs).sort((a,b)=>a-b)[Math.floor((rates.length-1)*0.95)], encodedBytes:rates.reduce((total,rate)=>total+rate.bytes,0), profile: { downstreamBytesPerSecond: 24000, delayMs: 45, durationMs: 2000, dropEvery: 11, blackoutStartMs: 300, blackoutDurationMs: 180 }, packets, lost, wireBytes, trialMs:Math.round(performance.now()-started), peakQueuedDatagrams: peak, ...summary });
    } finally { for (const timer of timers) clearTimeout(timer); downstream.close(); upstream.close(); }
  }
  await writeFile(resolve(directory, "network-report.json"), JSON.stringify(proxyReports, null, 2));
  console.log(JSON.stringify(proxyReports));
  const responses = [...publisher.state.output.matchAll(/PERI_KEYFRAME_RESPONSE (\d+) (\d+)/g)].map(match => Number(match[1]));
  if (responses.some((value, index) => index > 0 && value - responses[index - 1]! < 15)) throw new Error("missing or excessive encoder keyframe responses");
  console.log(`Verified ${responses.length} actual encoder keyframe responses with >=15-frame spacing`);
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
} finally {
  for (const process of children) if (process.child.exitCode === null) process.child.kill();
  await Promise.allSettled(children.map(process => finish(process, 5000)));
}
