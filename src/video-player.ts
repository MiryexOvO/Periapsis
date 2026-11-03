import * as Moq from "@moq/net";
import * as Loc from "@moq/loc";
import * as Msf from "./media-catalog.ts";
import { connect } from "./transport.ts";
import { resolveBroadcast } from "./resolve.ts";

const canvas = document.querySelector<HTMLCanvasElement>("#screen")!;
const context = canvas.getContext("2d")!;
const status = document.querySelector<HTMLElement>("#status")!;
const metrics = document.querySelector<HTMLElement>("#metrics")!;
const start = document.querySelector<HTMLButtonElement>("#connect")!;
const recover = document.querySelector<HTMLButtonElement>("#recover")!;
const stop = document.querySelector<HTMLButtonElement>("#stop")!;
let cleanup: (() => void) | undefined;

start.onclick = async () => {
  start.disabled = true;
  let alive = true, needKey = true, rendered = 0, keys = 0;
  let peer: Awaited<ReturnType<typeof connect>> | undefined;
  let decoder: VideoDecoder | undefined;
  const handles: (() => void)[] = [];
  const close = () => { if (!alive) return; alive = false; for (const release of [...handles].reverse()) release(); if (decoder?.state !== "closed") decoder?.close(); peer?.close(); start.disabled = false; stop.disabled = true; recover.disabled = true; };
  cleanup = close;
  stop.disabled = false;
  try {
    if (!globalThis.isSecureContext || typeof VideoDecoder === "undefined") throw new Error("需要 HTTPS 或 localhost，以及支持 WebCodecs 的浏览器");
    const config = await fetch("/config").then(r => r.json()) as { url: string; path: string };
    if (!alive) return;
    peer = await connect(new URL(config.url), "subscribe");
    if (!alive) { peer.close(); return; }
    const request = peer.origin.request(Moq.Path.from(config.path)); handles.push(() => request.close());
    const broadcast = await resolveBroadcast(request, 5000);
    const catalogTrack = broadcast.track("catalog").subscribe(); handles.push(() => catalogTrack.close());
    const catalogGroup = await catalogTrack.recvGroup(); if (!catalogGroup) throw new Error("缺少媒体描述");
    const catalogFrame = await catalogGroup.readFrame(); catalogGroup.close();
    if (!catalogFrame || catalogFrame.payload.length > 16 * 1024) throw new Error("无效媒体描述");
    const info = Msf.decode(catalogFrame.payload).tracks.find(t => t.role === "video");
    if (!info || info.codec !== "vp8" || info.packaging !== "loc") throw new Error("不支持的媒体格式");
    const codec: VideoDecoderConfig = { codec: "vp8", codedWidth: info.width, codedHeight: info.height, optimizeForLatency: true };
    if (!(await VideoDecoder.isConfigSupported(codec)).supported) throw new Error("浏览器不支持 VP8 解码");
    decoder = new VideoDecoder({ output(frame) {
      try { if (alive) { context.drawImage(frame, 0, 0, canvas.width, canvas.height); rendered++; metrics.textContent = `已显示 ${rendered} 帧 · 已解码 ${keys} 个关键帧`; metrics.dataset.frames = String(rendered); metrics.dataset.keys = String(keys); } }
      finally { frame.close(); }
    }, error(error) { if (alive) { status.textContent = error.message; close(); } } });
    decoder.configure(codec);
    recover.disabled = false;
    recover.onclick = () => { decoder!.reset(); decoder!.configure(codec); needKey = true; status.textContent = "等待下一个关键帧"; };
    const video = broadcast.track(info.name).subscribe({ maxAge: Moq.Time.Milli(0) }); handles.push(() => video.close());
    const loc = new Loc.Format("video");
    while (alive) {
      const group = await video.recvGroup(); if (!group) throw new Error("视频已结束");
      try { for (;;) {
        const object = await group.readFrame(); if (!object || !alive) break;
        if (object.payload.length > 1024 * 1024) throw new Error("视频帧过大");
        for (const frame of loc.decode(object.payload)) {
          // LOC 元数据不携带 VP8 帧类型，直接读取 VP8 帧头；恢复前丢弃依赖帧。
          if (!frame.payload.length) continue;
          const key = (frame.payload[0]! & 1) === 0;
          if (decoder.decodeQueueSize > 8) { decoder.reset(); decoder.configure(codec); needKey = true; }
          if (needKey && !key) continue;
          if (key) { keys++; needKey = false; }
          decoder.decode(new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp: frame.timestamp, data: frame.payload as Uint8Array<ArrayBuffer> }));
          status.textContent = "播放中";
        }
      } } finally { group.close(); }
    }
  } catch (error) { if (alive) status.textContent = error instanceof Error ? error.message : String(error); close(); }
};
stop.onclick = () => { cleanup?.(); status.textContent = "已停止"; };
window.addEventListener("pagehide", () => cleanup?.());
