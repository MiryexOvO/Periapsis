import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { spawnSync, spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { once } from "node:events";
import { resolve } from "node:path";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import * as Moq from "@moq/net";
import * as Loc from "@moq/loc";
import * as Msf from "./media-catalog.ts";
import { parseIvf } from "./ivf.ts";
import { connect } from "./transport.ts";
import { startRelay } from "./relay.ts";

/** Start a loopback-only test-video publisher and browser viewer.
 * 启动仅本机可访问的测试视频发布端与浏览器播放器。
 */
export async function startVideoDemo(port = 8080, transport: "websocket" | "webtransport" = "websocket") {
  const root = fileURLToPath(new URL("../", import.meta.url));
  await mkdir(new URL("../dist/", import.meta.url), { recursive: true });
  const input = fileURLToPath(new URL("../dist/test.ivf", import.meta.url));
  const ffmpeg = spawnSync(process.env.PERI_FFMPEG ?? "ffmpeg", ["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30", "-t", "3", "-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8", "-g", "30", "-auto-alt-ref", "0", "-b:v", "700k", "-f", "ivf", input], { cwd: root, windowsHide: true, encoding: "utf8" });
  if (ffmpeg.error || ffmpeg.status !== 0) throw new Error(`FFmpeg VP8 generation failed: ${ffmpeg.error?.message ?? ffmpeg.stderr}`);
  const video = parseIvf(await readFile(input));
  if (transport === "webtransport") {
    const result = spawnSync("cargo", ["build", "--locked", "-p", "peri-transport", "--example", "webtransport_video"], { cwd: root, stdio: "inherit", windowsHide: true });
    if (result.error || result.status !== 0) throw new Error("Rust WebTransport gateway build failed");
  }
  await build({ entryPoints: [fileURLToPath(new URL("./video-player.ts", import.meta.url))], bundle: true, platform: "browser", format: "esm", outfile: fileURLToPath(new URL("../dist/player.js", import.meta.url)) });
  const html = await readFile(new URL("../web/video.html", import.meta.url));
  const script = await readFile(new URL("../dist/player.js", import.meta.url));
  let config = {};
  const server = createServer((req, res) => {
    res.setHeader("Cache-Control", "no-store");
    res.setHeader("Referrer-Policy", "no-referrer");
    if (req.url === "/") { res.setHeader("Content-Type", "text/html; charset=utf-8"); res.end(html); }
    else if (req.url === "/player.js") { res.setHeader("Content-Type", "text/javascript"); res.end(script); }
    else if (req.url === "/config") { res.setHeader("Content-Type", "application/json"); res.end(JSON.stringify(config)); }
    else { res.writeHead(404); res.end(); }
  });
  await new Promise<void>((resolve, reject) => { server.once("error", reject); server.listen(port, "127.0.0.1", resolve); });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("invalid viewer address");
  const url = `http://127.0.0.1:${address.port}`;
  const publishToken = randomBytes(24).toString("hex"), subscribeToken = randomBytes(24).toString("hex");
  const relay = await startRelay({ publishToken, subscribeToken, allowedOrigins: [url] });
  const publishUrl = new URL(relay.url); publishUrl.searchParams.set("role", "publish"); publishUrl.searchParams.set("token", publishToken);
  const watchUrl = new URL(relay.url); watchUrl.searchParams.set("role", "subscribe"); watchUrl.searchParams.set("token", subscribeToken);
  config = { url: watchUrl.toString(), path: "peri/test-video" };
  const publisher = await connect(publishUrl, "publish");
  const broadcast = publisher.origin.createBroadcast(Moq.Path.from("peri/test-video"));
  const catalog = broadcast.createTrack("catalog", { maxAge: Moq.Time.Milli(2000) });
  const track = broadcast.createTrack("video", { maxAge: Moq.Time.Milli(2000) });
  const encoded = new Loc.Producer(track);
  const group = catalog.appendGroup();
  const catalogBytes = Msf.encode({ tracks: [{ name: "video", packaging: "loc", role: "video", codec: "vp8", width: video.width, height: video.height, framerate: video.framerate, isLive: true }] });
  group.writeFrame({ payload: catalogBytes, timestamp: Moq.Time.Timestamp.now() }); group.close();
  broadcast.announce();
  let index = 0, cycle = 0, stopped = false;
  let gateway: ReturnType<typeof spawn> | undefined;
  if (transport === "webtransport") {
    gateway = spawn(resolve(process.env.CARGO_TARGET_DIR ?? resolve(root, "target"), "debug/examples", process.platform === "win32" ? "webtransport_video.exe" : "webtransport_video"), [], { cwd: root, stdio: ["pipe", "pipe", "inherit"], windowsHide: true });
    const child = gateway;
    const lines = createInterface({ input: child.stdout! });
    const ready = await new Promise<{ url: string; hash: string }>((resolve, reject) => {
      const timer = setTimeout(() => { child.kill(); reject(new Error("WebTransport gateway startup timed out")); }, 10000);
      child.once("error", error => { clearTimeout(timer); reject(error); });
      child.once("exit", () => { clearTimeout(timer); reject(new Error("WebTransport gateway exited")); });
      lines.once("line", line => { clearTimeout(timer); try { resolve(JSON.parse(line)); } catch (error) { reject(error); } });
      child.stdin!.write(JSON.stringify({ origin: url, token: subscribeToken, catalog: Buffer.from(catalogBytes).toString("base64") }) + "\n");
    });
    const endpoint = new URL(ready.url); endpoint.searchParams.set("token", subscribeToken);
    config = { url: endpoint.toString(), path: "peri/test-video", certificateHash: ready.hash };
    const subscription = track.subscribe();
    void (async () => {
      const loc = new Loc.Format("video");
      while (!stopped) {
        const group = await subscription.recvGroup(); if (!group) break;
        try { for (;;) {
          const frame = await group.readFrame(); if (!frame || stopped) break;
          const media = loc.decode(frame.payload)[0]!;
          const message = JSON.stringify({ payload: Buffer.from(frame.payload).toString("base64"), timestamp: media.timestamp, keyframe: (media.payload[0]! & 1) === 0 }) + "\n";
          if (!child.stdin!.write(message)) await once(child.stdin!, "drain");
        } } finally { group.close(); }
      }
    })().catch(error => { if (!stopped) console.error("WebTransport media input failed:", error.message); }).finally(() => subscription.close());
  }
  const started = performance.now();
  let timer: ReturnType<typeof setTimeout>;
  const tick = () => {
    if (stopped) return;
    const frame = video.frames[index]!;
    encoded.encode(frame.payload, Moq.Time.Micro(frame.timestamp + cycle * video.duration), frame.keyframe);
    if (++index === video.frames.length) { index = 0; cycle++; }
    const next = cycle * video.duration + video.frames[index]!.timestamp;
    timer = setTimeout(tick, Math.max(0, started + next / 1000 - performance.now()));
  };
  tick();
  return { url, async close() { stopped = true; clearTimeout(timer); encoded.close(); catalog.close(); broadcast.close(); publisher.close(); gateway?.kill(); await relay.close(); await new Promise<void>(resolve => server.close(() => resolve())); } };
}
