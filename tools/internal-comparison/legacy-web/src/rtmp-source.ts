import { spawn } from "node:child_process";
import { readFlv } from "./flv.ts";
import { isAvcKeyframe } from "./avc.ts";
import type { Catalog } from "./media-catalog.ts";

export interface MediaFrame { track: "video" | "audio"; payload: Uint8Array; timestamp: number; keyframe: boolean }
export interface MediaSource { catalog: Catalog; frames: AsyncIterable<MediaFrame>; close(): void }

/** Accept one local RTMP publisher and preserve H.264/AAC encoded samples.
 * 接收单个本机 RTMP 发布端，保留原始 H.264/AAC 编码样本。
 */
export async function openRtmpSource(port = 1935, signal?: AbortSignal): Promise<MediaSource> {
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("invalid RTMP port");
  const child = spawn(process.env.PERI_FFMPEG ?? "ffmpeg", ["-hide_banner", "-loglevel", "error", "-listen", "1", "-i", `rtmp://127.0.0.1:${port}/live/stream`, "-map", "0:v:0", "-map", "0:a:0", "-c", "copy", "-flvflags", "no_duration_filesize", "-flush_packets", "1", "-f", "flv", "pipe:1"], { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
  let failure: Error | undefined, stderr = "";
  child.on("error", error => { failure = error; });
  child.stderr.on("data", chunk => { stderr = (stderr + String(chunk)).slice(-4096); });
  const close = () => { signal?.removeEventListener("abort", close); child.kill(); };
  signal?.addEventListener("abort", close, { once: true });
  if (signal?.aborted) close();
  const tags = readFlv(child.stdout)[Symbol.asyncIterator]();
  const catalog: Catalog = { tracks: [] };
  let video: Catalog["tracks"][number] | undefined, audio: Catalog["tracks"][number] | undefined;
  let videoInit = "", audioInit = "", lengthSize = 4, started = false, audioGroup = -1;
  async function* frames(): AsyncGenerator<MediaFrame> {
    for (;;) {
      const next = await tags.next();
      if (next.done) throw failure ?? new Error(`RTMP input ended: ${stderr || "publisher disconnected"}`);
      const { type, timestamp, data } = next.value;
      if (type !== 8 && type !== 9) continue;
      if (data.length < 2) throw new Error("invalid FLV media tag");
      if (type === 9) {
        if ((data[0]! & 15) !== 7 || data.length < 5) throw new Error("RTMP video must be H.264");
        if (data[1] === 0) {
          const init = data.subarray(5);
          if (init.length < 7 || init[0] !== 1) throw new Error("invalid AVC configuration");
          const value = init.toString("base64");
          if (videoInit && videoInit !== value) throw new Error("publisher codec changed; restart the gateway");
          videoInit = value; lengthSize = (init[4]! & 3) + 1;
          video = { name: "video", role: "video", packaging: "loc", codec: `avc1.${init.subarray(1, 4).toString("hex")}`, initData: value, isLive: true };
        } else if (data[1] === 1 && video && audio) {
          if (data.readIntBE(2, 3) !== 0) throw new Error("publisher B-frames are unsupported; set B-frames to 0");
          const payload = data.subarray(5), keyframe = isAvcKeyframe(payload, lengthSize);
          if (!started && !keyframe) continue;
          started = true; yield { track: "video", payload, timestamp, keyframe };
        }
      } else {
        if ((data[0]! >> 4) !== 10) throw new Error("RTMP audio must be AAC-LC");
        if (data[1] === 0) {
          const init = data.subarray(2);
          if (init.length < 2 || init.length > 64) throw new Error("invalid AAC configuration");
          const objectType = init[0]! >> 3, rateIndex = ((init[0]! & 7) << 1) | (init[1]! >> 7), channels = (init[1]! >> 3) & 15;
          const rate = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350][rateIndex];
          if (objectType !== 2 || !rate || channels < 1 || channels > 2) throw new Error("RTMP audio must be mono/stereo AAC-LC");
          const value = init.toString("base64");
          if (audioInit && audioInit !== value) throw new Error("publisher codec changed; restart the gateway");
          audioInit = value; audio = { name: "audio", role: "audio", packaging: "loc", codec: "mp4a.40.2", initData: value, samplerate: rate, channelConfig: String(channels), isLive: true };
        } else if (data[1] === 1 && started) {
          if (data.length > 65536) throw new Error("AAC frame too large");
          const group = Math.floor(timestamp / 1000000), keyframe = group !== audioGroup; audioGroup = group;
          yield { track: "audio", payload: data.subarray(2), timestamp, keyframe };
        }
      }
      if (video && audio && !catalog.tracks.length) catalog.tracks.push(video, audio);
    }
  }
  const iterator = frames();
  try {
    const first = await iterator.next();
    return { catalog, frames: (async function* () { if (!first.done) yield first.value; yield* iterator; })(), close };
  } catch (error) { close(); throw error; }
}
