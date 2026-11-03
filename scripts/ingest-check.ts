import { startVideoDemo } from "./media-runtime.ts";
import { openRtmpSource } from "../src/rtmp-source.ts";
import { readFlv } from "../src/flv.ts";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:net";
import { createReadStream } from "node:fs";
import { mkdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { chromium } from "playwright-core";

await mkdir("dist", { recursive: true });
const ffmpeg = process.env.PERI_FFMPEG ?? "ffmpeg";
const fixture = "dist/ingest-fixture.flv";
const generated = spawnSync(ffmpeg, ["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=960x540:rate=30", "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000", "-t", "3", "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-pix_fmt", "yuv420p", "-g", "30", "-bf", "0", "-c:a", "aac", "-ac", "2", "-f", "flv", fixture], { windowsHide: true, encoding: "utf8" });
if (generated.status !== 0) throw new Error(generated.stderr || "fixture generation failed");
const hash = (data: Uint8Array) => createHash("sha256").update(data).digest("hex");
const expected = new Set<string>();
for await (const tag of readFlv(createReadStream(fixture))) {
  if (tag.type === 9 && tag.data[1] === 1) expected.add(hash(tag.data.subarray(5)));
  if (tag.type === 8 && tag.data[1] === 1) expected.add(hash(tag.data.subarray(2)));
}
const reservation = createServer();
await new Promise<void>(resolve => reservation.listen(0, "127.0.0.1", resolve));
const address = reservation.address(); if (!address || typeof address === "string") throw new Error("invalid port");
await new Promise<void>(resolve => reservation.close(() => resolve()));
const controller = new AbortController();
const pending = openRtmpSource(address.port, controller.signal);
// FFmpeg 的 RTMP 监听初始化没有 ready 消息；测试在短暂启动间隔后连接。
await new Promise(resolve => setTimeout(resolve, 600));
const publisher = spawn(ffmpeg, ["-hide_banner", "-loglevel", "error", "-re", "-stream_loop", "-1", "-i", fixture, "-c", "copy", "-f", "flv", `rtmp://127.0.0.1:${address.port}/live/stream`], { windowsHide: true, stdio: ["ignore", "ignore", "inherit"] });
let demo: Awaited<ReturnType<typeof startVideoDemo>> | undefined;
let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
const timeout = setTimeout(() => controller.abort(), 60000);
try {
  const source = await pending;
  let compared = 0;
  const original = source.frames;
  source.frames = (async function* () {
    for await (const frame of original) {
      if (!expected.has(hash(frame.payload))) throw new Error("encoded sample changed during RTMP ingest");
      compared++; yield frame;
    }
  })();
  demo = await startVideoDemo(0, process.argv.includes("--ws") ? "websocket" : "webtransport", source);
  browser = await chromium.launch({ channel: "chrome", headless: true });
  for (let viewer = 0; viewer < 2; viewer++) {
    const page = await browser.newPage(); await page.goto(demo.url); await page.click("#connect");
    try { await page.waitForFunction(() => {
      const stats = document.querySelector<HTMLElement>("#metrics")!.dataset;
      return Number(stats.frames) >= 25 && Number(stats.audioPackets) >= 30 && Number(stats.audioEnergy) > 0.001;
    }, undefined, { timeout: 20000 }); }
    catch (error) { console.error(await page.locator("#status").textContent()); throw error; }
    const stats = await page.locator("#metrics").evaluate(el => ({ ...(el as HTMLElement).dataset }));
    const size = await page.locator("canvas").evaluate((el: HTMLCanvasElement) => [el.width, el.height]);
    if (String(size) !== "960,540") throw new Error(`resolution changed: ${size}`);
    if (stats.transport !== (process.argv.includes("--ws") ? "websocket" : "webtransport")) throw new Error("wrong transport");
    console.log(`Verified H.264/AAC viewer ${viewer + 1}: ${JSON.stringify(stats)}, resolution=${size}`);
    await page.click("#recover");
    const keys = Number(stats.keys);
    await page.waitForFunction(previous => Number(document.querySelector<HTMLElement>("#metrics")!.dataset.keys) > previous, keys);
    await page.click("#stop");
  }
  if (compared < 60) throw new Error("insufficient samples verified");
  console.log(`Verified ${compared} encoded samples unchanged across RTMP ingest; late join and keyframe recovery passed`);
} finally { clearTimeout(timeout); controller.abort(); publisher.kill(); await browser?.close(); await demo?.close(); }
