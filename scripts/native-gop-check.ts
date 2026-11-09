import dgram from "node:dgram";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, access, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { randomBytes, createHash } from "node:crypto";

const root = process.cwd();
await mkdir(resolve(root, "dist"), { recursive: true });
const directory = await mkdtemp(resolve(root, "dist/gop-check-"));
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
function hashes(bytes: Buffer) {
  if (bytes.toString("ascii", 0, 4) !== "DKIF") throw new Error("missing IVF output");
  const values: string[] = [];
  for (let offset = 32; offset < bytes.length;) {
    if (offset + 12 > bytes.length) throw new Error("truncated IVF");
    const length = bytes.readUInt32LE(offset); offset += 12;
    if (!length || offset + length > bytes.length) throw new Error("invalid IVF frame");
    values.push(createHash("sha256").update(bytes.subarray(offset, offset + length)).digest("hex")); offset += length;
  }
  return values;
}
async function decoded(path: string) {
  const output = await run(ffmpeg, ["-hide_banner", "-loglevel", "error", "-i", path, "-map", "0:v:0", "-vsync", "0", "-f", "framemd5", "pipe:1"], environment, true, 60000);
  return output.split(/\r?\n/).filter(line => line.trim() && !line.startsWith("#")).map(line => line.split(",").at(-1)!.trim());
}
try {
  await run("cargo", ["build", "--locked", "-p", "peri-relay", "--bin", "peri"], environment, true, 120000);
  const source = resolve(directory, "source.ivf");
  await run(ffmpeg, ["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=15", "-t", "2", "-c:v", "libaom-av1", "-strict", "-2", "-cpu-used", "8", "-g", "15", "-usage", "realtime", "-lag-in-frames", "0", "-auto-alt-ref", "0", "-crf", "40", "-b:v", "0", "-pix_fmt", "yuv420p", "-f", "ivf", source], environment, true, 120000);
  const certDir = resolve(directory, "tls"), wrongDir = resolve(directory, "wrong-tls");
  await run(binary, ["init", certDir]); await run(binary, ["init", wrongDir]);
  const cert = resolve(certDir, "cert.der"), key = resolve(certDir, "key.key");
  const resource = "live/process-av1";
  const server = start(binary, ["relay", "127.0.0.1:0", cert, key, resource]); children.push(server);
  await waitFor(server, "PERI_RELAY_READY");
  const address = server.state.output.match(/PERI_RELAY_READY ([^\s]+)/)?.[1]; if (!address) throw new Error("missing server address");
  const publishArgs = ["publish", address, "localhost", cert, resource, source, "--gop", "--loop"];
  const publisher = start(binary, publishArgs); children.push(publisher);
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
  const captures = [resolve(directory, "viewer-1.ivf"), resolve(directory, "viewer-2.ivf")];
  const viewers = captures.map(path => { const process = start(binary, ["receive", address, "localhost", cert, resource, path, "30"]); children.push(process); return process; });
  const codes = await Promise.all(viewers.map(viewer => finish(viewer)));
  if (codes.some(code => code !== 0)) throw new Error(`viewer failed: ${viewers.map(viewer => viewer.state.output).join("\n")}`);
  const originalHashes = hashes(await readFile(source)), originalPixels = await decoded(source);
  for (const path of captures) {
    const encoded = hashes(await readFile(path)), pixels = await decoded(path);
    if (encoded.length !== 30 || pixels.length !== 30) throw new Error("missing received frames");
    for (let index = 0; index < encoded.length; index++) {
      const original = originalHashes.indexOf(encoded[index]!);
      if (original < 0 || pixels[index] !== originalPixels[original]) throw new Error("encoded or decoded video changed across processes");
    }
  }
  console.log("Verified separate relay, publisher and two recorder processes: 30 AV1 frames each, encoded and decoded checksums match");
  for (const mode of [[], ["--fifo"]]) {
    const output = await run(binary, ["play", address, "localhost", cert, resource, "30", "--headless", ...mode], environment, true, 30000);
    console.log(output.split(/\r?\n/).filter(line => line.startsWith("PERI_PLAY_DONE")).join("\n"));
  }
  // Actual UDP proxy: per-direction queues are bounded; packets exceed neither
  // 64 queued datagrams nor a 250ms scheduled delay. Only the viewer path is impaired.
  const proxyReports = [];
  const repeats = Number(process.env.PERI_NETWORK_REPEATS ?? "3");
  if (!Number.isInteger(repeats) || repeats < 1 || repeats > 5) throw new Error("PERI_NETWORK_REPEATS must be 1..5");
  for (let round = 1; round <= repeats; round++) for (const mode of round % 2 ? [[], ["--fifo"]] : [["--fifo"], []]) {
    const downstream = dgram.createSocket("udp4"), upstream = dgram.createSocket("udp4");
    await new Promise<void>(resolve => downstream.bind(0, "127.0.0.1", resolve));
    await new Promise<void>(resolve => upstream.bind(0, "127.0.0.1", resolve));
    const timers = new Set<ReturnType<typeof setTimeout>>();
    let peer: dgram.RemoteInfo | undefined, started = 0, next = 0, packets = 0, lost = 0, queued = 0, peak = 0;
    downstream.on("message", (bytes, remote) => { peer = remote; upstream.send(bytes, Number(address.split(":")[1]), "127.0.0.1"); });
    upstream.on("message", bytes => {
      if (!peer) return;
      const elapsed = performance.now() - started;
      const impaired = started > 0 && elapsed < 2000;
      packets++;
      if (impaired && packets % 97 === 0) { lost++; return; }
      const now = performance.now();
      next = Math.max(now, next) + (impaired ? bytes.length / 24 : 0); // 24 kB/s
      const delay = Math.max(0, next - now) + (impaired ? 45 : 0);
      if (queued >= 64 || delay > 250) { lost++; return; }
      queued++; peak = Math.max(peak, queued);
      const timer = setTimeout(() => { timers.delete(timer); queued--; if (peer) downstream.send(bytes, peer.port, peer.address); }, delay);
      timers.add(timer);
    });
    try {
      const viewer = start(binary, ["play", `127.0.0.1:${(downstream.address() as import("node:net").AddressInfo).port}`, "localhost", cert, resource, "60", "--headless", ...mode]); children.push(viewer);
      await waitFor(viewer, "PERI_FRAME", 10); started = performance.now();
      if (await finish(viewer, 30000) !== 0) throw new Error(`Impaired playback failed: ${viewer.state.output}`);
      const summary = JSON.parse(viewer.state.output.match(/PERI_PLAY_DONE (.+)/)![1]!);
      if (summary.decoded !== 60 || summary.reconnects !== 0 || lost === 0) throw new Error(`Live recovery regression: ${JSON.stringify(summary)}, dropped UDP packets: ${lost}`);
      proxyReports.push({ round, mode: mode.length ? "fifo" : "deadline", profile: { downstreamBytesPerSecond: 24000, delayMs: 45, durationMs: 2000, dropEvery: 97 }, packets, lost, peakQueuedDatagrams: peak, ...summary });
    } finally { for (const timer of timers) clearTimeout(timer); downstream.close(); upstream.close(); }
  }
  await writeFile(resolve(directory, "network-report.json"), JSON.stringify(proxyReports, null, 2));
  console.log(JSON.stringify(proxyReports));
  // Exercise the visible-player output path with SDL's headless driver in CI.
  await run(binary, ["play", address, "localhost", cert, resource, "15"], { ...environment, SDL_VIDEODRIVER: "dummy", SDL_AUDIODRIVER: "dummy" }, true, 15000);
  publisher.child.kill(); await finish(publisher);
  await waitFor(server, "PERI_RESOURCE_ENDED");
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
