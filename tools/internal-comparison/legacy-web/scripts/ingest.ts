import { startVideoDemo } from "./media-runtime.ts";
import { openRtmpSource } from "../src/rtmp-source.ts";

const controller = new AbortController();
let demo: Awaited<ReturnType<typeof startVideoDemo>> | undefined;
for (const signal of ["SIGINT", "SIGTERM"] as const) process.once(signal, () => { controller.abort(); void demo?.close(); });
console.log(`RTMP server: rtmp://127.0.0.1:${process.env.PERI_RTMP_PORT ?? 1935}/live`);
console.log("RTMP stream key: stream (local development only; no authentication)");
console.log("Use H.264 + AAC, B-frames=0, keyframe interval=1s. Waiting for publisher...");
try {
  const source = await openRtmpSource(Number(process.env.PERI_RTMP_PORT ?? 1935), controller.signal);
  try { demo = await startVideoDemo(Number(process.env.PERI_VIDEO_PORT ?? 8080), process.argv.includes("--ws") ? "websocket" : "webtransport", source); }
  catch (error) { source.close(); throw error; }
  if (controller.signal.aborted) { await demo.close(); }
  else console.log(`Periapsis live: ${demo.url}`);
} catch (error) { if (!controller.signal.aborted) { console.error(error); process.exitCode = 1; } }
