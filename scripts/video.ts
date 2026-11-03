import { startVideoDemo } from "./media-runtime.ts";
const demo = await startVideoDemo(Number(process.env.PERI_VIDEO_PORT ?? 8080));
console.log(`Periapsis video: ${demo.url}`);
for (const signal of ["SIGINT", "SIGTERM"] as const) process.once(signal, () => { void demo.close(); });
