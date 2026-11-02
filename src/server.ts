import { startRelay } from "./relay.ts";

const relay = await startRelay({ port: Number(process.env.NSP_PORT ?? 4443),
  publishToken: process.env.NSP_PUBLISH_TOKEN ?? "",
  subscribeToken: process.env.NSP_SUBSCRIBE_TOKEN ?? "" });
console.log(`NSP reference relay: ${relay.url} (MOQT-22 over QMux/WebSocket; local development)`);
console.log(`Health: ${relay.healthUrl}`);
let closing = false;
for (const signal of ["SIGINT", "SIGTERM"] as const) process.on(signal, async () => {
  if (closing) return;
  closing = true;
  await relay.close();
});
