import { startRelay } from "./relay.ts";

const relay = await startRelay({ port: Number(process.env.PERI_PORT ?? 4443),
  publishToken: process.env.PERI_PUBLISH_TOKEN ?? "",
  subscribeToken: process.env.PERI_SUBSCRIBE_TOKEN ?? "" });
console.log(`PERI reference relay: ${relay.url} (MOQT-22 over QMux/WebSocket; local development)`);
console.log(`Health: ${relay.healthUrl}`);
let closing = false;
for (const signal of ["SIGINT", "SIGTERM"] as const) process.on(signal, async () => {
  if (closing) return;
  closing = true;
  await relay.close();
});
