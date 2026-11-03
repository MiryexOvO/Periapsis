import { chromium } from "playwright-core";
import { startVideoDemo } from "./media-runtime.ts";
import { mkdir } from "node:fs/promises";
const quic = process.argv.includes("--quic");
const demo = await startVideoDemo(0, quic ? "webtransport" : "websocket");
const browser = await chromium.launch({ channel: "chrome", headless: true });
try {
  const first = await browser.newPage();
  await first.goto(demo.url); await first.click("#connect");
  await first.waitForFunction(() => Number(document.querySelector<HTMLElement>("#metrics")?.dataset.frames) >= 30, undefined, { timeout: 15000 });
  const late = await browser.newPage(); await late.goto(demo.url); await late.click("#connect");
  await late.waitForFunction(() => Number(document.querySelector<HTMLElement>("#metrics")?.dataset.frames) >= 15, undefined, { timeout: 15000 });
  const wire = await first.locator("#metrics").getAttribute("data-transport");
  if (wire !== (quic ? "webtransport" : "websocket")) throw new Error(`Unexpected transport: ${wire}`);
  console.log(`Verified browser transport: ${wire}`);
  for (const page of [first, late]) {
    await page.waitForFunction(() => {
      const stats = document.querySelector<HTMLElement>("#metrics")!.dataset;
      return Number(stats.audioPackets) > 30 && Number(stats.audioEnergy) > 0.001 && stats.audioState === "running";
    }, undefined, { timeout: 10000 });
    const stats = await page.locator("#metrics").evaluate(element => ({ ...((element as HTMLElement).dataset) }));
    if (Number(stats.audioBufferMs) > 360) throw new Error("unbounded audio buffer");
    console.log(`Verified Opus playback: packets=${stats.audioPackets}, buffer=${stats.audioBufferMs}ms, scheduled video skew=${stats.skewMs}ms`);
  }
  await first.click("#mute");
  if (await first.locator("#mute").textContent() !== "取消静音") throw new Error("mute failed");
  if (quic) {
    const rejected = await first.evaluate(async () => {
      const config = await fetch("/config").then(r => r.json());
      const attempt = async (badHash: boolean) => {
        const url = new URL(config.url);
        if (!badHash) url.searchParams.set("token", "invalid");
        const hash: string = badHash ? "00".repeat(32) : config.certificateHash;
        const options: WebTransportOptions & { protocols: string[] } = { protocols: ["moqt-22"], serverCertificateHashes: [{ algorithm: "sha-256", value: Uint8Array.from(hash.match(/../g)!, hex => parseInt(hex, 16)).buffer }] };
        const connection = new WebTransport(url, options);
        connection.closed.catch(() => {});
        const timer = setTimeout(() => connection.close(), 3000);
        try { await connection.ready; return false; }
        catch { return true; }
        finally { clearTimeout(timer); connection.close(); }
      };
      return { invalidCertificate: await attempt(true), invalidToken: await attempt(false) };
    });
    if (!rejected.invalidCertificate || !rejected.invalidToken) throw new Error("WebTransport access validation failed");
    console.log("Verified certificate and token rejection");
  }
  const visible = await first.locator("canvas").evaluate((canvas: HTMLCanvasElement) => {
    const rgba = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, canvas.height).data;
    return rgba.some((value, i) => i % 4 !== 3 && value > 30);
  });
  if (!visible) throw new Error("decoded output is blank");
  const before = await first.locator("#metrics").getAttribute("data-keys");
  await first.click("#recover");
  await first.waitForFunction(previous => Number(document.querySelector<HTMLElement>("#metrics")?.dataset.keys) > Number(previous), before, { timeout: 5000 });
  await mkdir("reports", { recursive: true }); await first.screenshot({ path: "reports/video.png" });
  console.log(JSON.stringify({ first: await first.locator("#metrics").textContent(), lateJoin: await late.locator("#metrics").textContent(), status: await first.locator("#status").textContent() }));
  await first.click("#stop");
  if (await first.locator("#status").textContent() !== "已停止") throw new Error("stop failed");
} finally { await browser.close(); await demo.close(); }
