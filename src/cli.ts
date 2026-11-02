import { readFile, stat, writeFile } from "node:fs/promises";
import { once } from "node:events";
import { connect, publishTrack, subscribeTrack, MAX_OBJECT_BYTES } from "./transport.ts";

const [command, file, path = "nsp/probe"] = process.argv.slice(2);
if (!file || !["publish", "subscribe"].includes(command ?? "")) {
  console.error("Usage: npm run object -- publish|subscribe FILE [BROADCAST_PATH]");
  process.exit(1);
}
const publisher = command === "publish";
const token = publisher ? process.env.NSP_PUBLISH_TOKEN : process.env.NSP_SUBSCRIBE_TOKEN;
if (!token) throw new Error(`missing NSP_${publisher ? "PUBLISH" : "SUBSCRIBE"}_TOKEN`);
const url = new URL(process.env.NSP_RELAY_URL ?? "ws://127.0.0.1:4443/moq");
url.searchParams.set("role", publisher ? "publish" : "subscribe");
url.searchParams.set("token", token);
const peer = await connect(url, publisher ? "publish" : "subscribe");
try {
  if (publisher) {
    const info = await stat(file);
    if (!info.isFile() || info.size < 1 || info.size > MAX_OBJECT_BYTES) throw new Error("file must be 1 byte to 1 MiB");
    const track = publishTrack(peer.origin, path, "probe");
    try {
      track.write(await readFile(file));
      console.log(`Published ${info.size} bytes at ${path}; waiting for viewers. Ctrl+C to stop.`);
      await once(process, "SIGINT");
    } finally { track.close(); }
  } else {
    const track = await subscribeTrack(peer.origin, path, "probe");
    try {
      const bytes = await track.read();
      if (!bytes) throw new Error("track ended without an object");
      await writeFile(file, bytes, { flag: "wx" });
      console.log(`Received ${bytes.length} bytes into ${file}`);
    } finally { track.close(); }
  }
} finally { peer.close(); }
