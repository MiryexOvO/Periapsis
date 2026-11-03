import { chromium } from "playwright-core";
import { startVideoDemo } from "../src/video-demo.ts";
import { mkdir } from "node:fs/promises";
const demo = await startVideoDemo(0);
const browser = await chromium.launch({ channel: "chrome", headless: true });
try {
  const first = await browser.newPage();
  await first.goto(demo.url); await first.click("#connect");
  await first.waitForFunction(() => Number(document.querySelector<HTMLElement>("#metrics")?.dataset.frames) >= 30, { timeout: 15000 });
  const late = await browser.newPage(); await late.goto(demo.url); await late.click("#connect");
  await late.waitForFunction(() => Number(document.querySelector<HTMLElement>("#metrics")?.dataset.frames) >= 15, { timeout: 15000 });
  const before = await first.locator("#metrics").getAttribute("data-keys");
  await first.click("#recover");
  await first.waitForFunction(previous => Number(document.querySelector<HTMLElement>("#metrics")?.dataset.keys) > Number(previous), before, { timeout: 5000 });
  await mkdir("reports", { recursive: true }); await first.screenshot({ path: "reports/video.png" });
  console.log(JSON.stringify({ first: await first.locator("#metrics").textContent(), lateJoin: await late.locator("#metrics").textContent(), status: await first.locator("#status").textContent() }));
  await first.click("#stop");
  if (await first.locator("#status").textContent() !== "已停止") throw new Error("stop failed");
} finally { await browser.close(); await demo.close(); }
