import * as Moq from "@moq/net";
import * as Loc from "@moq/loc";
import * as Msf from "./media-catalog.ts";
import { connect } from "./transport.ts";
import { resolveBroadcast } from "./resolve.ts";
import { AudioPlayback } from "./audio-playback.ts";

const canvas = document.querySelector<HTMLCanvasElement>("#screen")!;
const context = canvas.getContext("2d")!;
const status = document.querySelector<HTMLElement>("#status")!;
const metrics = document.querySelector<HTMLElement>("#metrics")!;
const start = document.querySelector<HTMLButtonElement>("#connect")!;
const recover = document.querySelector<HTMLButtonElement>("#recover")!;
const stop = document.querySelector<HTMLButtonElement>("#stop")!;
const mute = document.querySelector<HTMLButtonElement>("#mute")!;
let cleanup: (() => void) | undefined;

start.onclick = async () => {
  start.disabled = true;
  let alive = true, needKey = true, rendered = 0, keys = 0;
  let peer: Awaited<ReturnType<typeof connect>> | undefined;
  let decoder: VideoDecoder | undefined;
  let audioDecoder: AudioDecoder | undefined;
  const audio = new AudioPlayback();
  let muted = false;
  mute.disabled = false; mute.textContent = "静音";
  mute.onclick = () => { muted = !muted; audio.muted = muted; mute.textContent = muted ? "取消静音" : "静音"; };
  const pictures: VideoFrame[] = [];
  let animation = 0, dropped = 0;
  const handles: (() => void)[] = [];
  const close = () => { if (!alive) return; alive = false; cancelAnimationFrame(animation); for (const frame of pictures) frame.close(); pictures.length = 0; audio.close(); if (audioDecoder?.state !== "closed") audioDecoder?.close(); for (const release of [...handles].reverse()) release(); if (decoder?.state !== "closed") decoder?.close(); peer?.close(); start.disabled = false; stop.disabled = true; recover.disabled = true; mute.disabled = true; };
  cleanup = close;
  stop.disabled = false;
  try {
    if (!globalThis.isSecureContext || typeof VideoDecoder === "undefined" || typeof AudioDecoder === "undefined") throw new Error("需要 HTTPS 或 localhost，以及支持音视频 WebCodecs 的浏览器");
    const config = await fetch("/config").then(r => r.json()) as { url: string; path: string; certificateHash?: string };
    if (!alive) return;
    peer = await connect(new URL(config.url), "subscribe", 5000, config.certificateHash);
    metrics.dataset.transport = peer.connection.transport;
    if (!alive) { peer.close(); return; }
    const request = peer.origin.request(Moq.Path.from(config.path)); handles.push(() => request.close());
    const broadcast = await resolveBroadcast(request, 5000);
    const catalogTrack = broadcast.track("catalog").subscribe(); handles.push(() => catalogTrack.close());
    const catalogGroup = await catalogTrack.recvGroup(); if (!catalogGroup) throw new Error("缺少媒体描述");
    const catalogFrame = await catalogGroup.readFrame(); catalogGroup.close();
    if (!catalogFrame || catalogFrame.payload.length > 16 * 1024) throw new Error("无效媒体描述");
    const catalog = Msf.decode(catalogFrame.payload);
    const info = catalog.tracks.find(t => t.role === "video");
    if (!info || info.codec !== "vp8" || info.packaging !== "loc") throw new Error("不支持的媒体格式");
    const codec: VideoDecoderConfig = { codec: "vp8", codedWidth: info.width, codedHeight: info.height, optimizeForLatency: true };
    if (!(await VideoDecoder.isConfigSupported(codec)).supported) throw new Error("浏览器不支持 VP8 解码");
    const audioInfo = catalog.tracks.find(t => t.role === "audio");
    if (!audioInfo || audioInfo.codec !== "opus") throw new Error("缺少 Opus 音频轨道");
    const audioConfig: AudioDecoderConfig = { codec: "opus", sampleRate: 48000, numberOfChannels: 1 };
    if (!(await AudioDecoder.isConfigSupported(audioConfig)).supported) throw new Error("浏览器不支持 Opus");
    audioDecoder = new AudioDecoder({ output: data => audio.schedule(data), error: error => { if (alive) { status.textContent = error.message; close(); } } });
    audioDecoder.configure(audioConfig);
    const audioTrack = broadcast.track(audioInfo.name).subscribe({ maxAge: Moq.Time.Milli(0) }); handles.push(() => audioTrack.close());
    void (async () => {
      const loc = new Loc.Format("audio");
      while (alive) {
        const group = await audioTrack.recvGroup(); if (!group) throw new Error("音频已结束");
        try { for (;;) {
          const object = await group.readFrame(); if (!object || !alive) break;
          if (object.payload.length > 65536) throw new Error("音频帧过大");
          for (const frame of loc.decode(object.payload)) {
            if (audioDecoder!.decodeQueueSize > 24) { audioDecoder!.reset(); audioDecoder!.configure(audioConfig); }
            audioDecoder!.decode(new EncodedAudioChunk({ type: "key", timestamp: frame.timestamp, data: frame.payload as Uint8Array<ArrayBuffer> }));
          }
        } } finally { group.close(); }
      }
    })().catch(error => { if (alive) { status.textContent = error.message; close(); } });
    decoder = new VideoDecoder({ output(frame) {
      if (!alive) { frame.close(); return; }
      pictures.push(frame);
      while (pictures.length > 12) { pictures.shift()!.close(); dropped++; }
    }, error(error) { if (alive) { status.textContent = error.message; close(); } } });
    const render = () => {
      if (!alive) return;
      const clock = audio.mediaTime;
      if (clock !== undefined) {
        while (pictures.length && pictures[0]!.timestamp <= clock + 15000) {
          const frame = pictures.shift()!;
          const skew = frame.timestamp - clock;
          if (skew >= -100000) {
            context.drawImage(frame, 0, 0, canvas.width, canvas.height); rendered++;
            metrics.dataset.skewMs = String(Math.round(skew / 1000));
          } else dropped++;
          frame.close();
        }
      }
      metrics.textContent = `已显示 ${rendered} 帧 · 已解码 ${keys} 个关键帧 · 音频 ${audio.packets} 包 · 缓冲 ${audio.bufferedMs} ms · 丢帧 ${dropped}`;
      Object.assign(metrics.dataset, { frames: String(rendered), keys: String(keys), audioPackets: String(audio.packets), audioEnergy: String(audio.energy), audioState: audio.context.state, audioBufferMs: String(audio.bufferedMs) });
      animation = requestAnimationFrame(render);
    };
    render();
    decoder.configure(codec);
    recover.disabled = false;
    recover.onclick = () => { decoder!.reset(); decoder!.configure(codec); for (const frame of pictures) frame.close(); pictures.length = 0; needKey = true; status.textContent = "等待下一个关键帧"; };
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
