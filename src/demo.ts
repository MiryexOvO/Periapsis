import { randomBytes, createHash } from "node:crypto";
import { startRelay } from "./relay.ts";
import { connect, publishTrack, subscribeTrack } from "./transport.ts";
import { defaultHello, negotiate, parseHello } from "./session.ts";

const publishToken = randomBytes(24).toString("hex");
const subscribeToken = randomBytes(24).toString("hex");
const relay = await startRelay({ publishToken, subscribeToken });
const endpoint = (role: string, token: string) => {
  const url = new URL(relay.url);
  url.searchParams.set("role", role); url.searchParams.set("token", token); return url;
};
const cleanups: (() => void)[] = [];
const deadline = setTimeout(() => { console.error("demo timed out"); process.exit(1); }, 15_000);
try {
  const pub = await connect(endpoint("publish", publishToken), "publish"); cleanups.push(() => pub.close());
  const sub = await connect(endpoint("subscribe", subscribeToken), "subscribe"); cleanups.push(() => sub.close());
  const control = publishTrack(pub.origin, "nsp-demo/control", "hello"); cleanups.push(() => control.close());
  control.write(new TextEncoder().encode(JSON.stringify(defaultHello())));
  const receiver = await subscribeTrack(sub.origin, "nsp-demo/control", "hello"); cleanups.push(() => receiver.close());
  const hello = await receiver.read();
  if (!hello) throw new Error("missing hello");
  const agreement = negotiate(defaultHello(), parseHello(hello));
  // 通过任意字节对象验证传输完整性，尚未采用 LOC/MSF 媒体封装。
  const sender = publishTrack(pub.origin, "nsp-demo/probe", "bytes"); cleanups.push(() => sender.close());
  const payload = randomBytes(64 * 1024);
  const digest = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");
  sender.write(payload);
  const reader = await subscribeTrack(sub.origin, "nsp-demo/probe", "bytes"); cleanups.push(() => reader.close());
  const received = await reader.read();
  if (!received || digest(payload) !== digest(received)) throw new Error("object integrity mismatch");
  console.log(JSON.stringify({ protocol: pub.connection.version, transport: pub.connection.transport,
    agreement, sentBytes: payload.length, receivedBytes: received.length,
    sha256: digest(received), mediaInterop: "not yet validated" }, null, 2));
} finally {
  clearTimeout(deadline);
  for (const close of cleanups.reverse()) close();
  await relay.close();
}
