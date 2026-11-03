import * as Moq from "@moq/net";
import * as Loc from "@moq/loc";
import * as Msf from "./media-catalog.ts";
import { connect } from "./transport.ts";
import { resolveBroadcast } from "./resolve.ts";
import { isAvcKeyframe } from "./avc.ts";
import { AudioPlayback } from "./audio-playback.ts";
import { recoverPlayback, RetryablePlaybackError } from "./recovery.ts";

const canvas = document.querySelector<HTMLCanvasElement>("#screen")!;
const context = canvas.getContext("2d")!;
const status = document.querySelector<HTMLElement>("#status")!;
const metrics = document.querySelector<HTMLElement>("#metrics")!;
const start = document.querySelector<HTMLButtonElement>("#connect")!;
const recover = document.querySelector<HTMLButtonElement>("#recover")!;
const stop = document.querySelector<HTMLButtonElement>("#stop")!;
const mute = document.querySelector<HTMLButtonElement>("#mute")!;
let cleanup: (() => void) | undefined;

let lifetime: AbortController | undefined;
let generation = 0;
start.onclick = async () => {
  if (lifetime) return;
  const controller = new AbortController(); lifetime = controller;
  const audio = new AudioPlayback();
  start.disabled = true; stop.disabled = false;
  cleanup = () => controller.abort();
  try {
    await recoverPlayback(signal => playAttempt(audio, signal), controller.signal, (count, delay) => {
      recover.disabled = true; mute.disabled = true;
      status.textContent = `连接中断，${delay} ms 后重连（${count}/8）`;
    });
  } catch (error) { if (!controller.signal.aborted) status.textContent = error instanceof Error ? error.message : String(error); }
  finally { audio.close(); if (lifetime === controller) { lifetime = undefined; start.disabled = false; stop.disabled = true; recover.disabled = true; mute.disabled = true; } }
};

async function playAttempt(audio: AudioPlayback, signal: AbortSignal) {
  const attempt = new AbortController();
  metrics.dataset.generation = String(++generation);
  metrics.dataset.frames = "0"; metrics.dataset.audioPackets = "0"; metrics.dataset.keys = "0";
  status.textContent = "连接中";
  let failure: Error | undefined;
  let connecting = false;
  let watchdog: ReturnType<typeof setInterval> | undefined;
  let startupTimer: ReturnType<typeof setTimeout> | undefined;
  let lastAudio = performance.now(), lastVideo = performance.now();
  let alive = true, needKey = true, rendered = 0, keys = 0;
  let peer: Awaited<ReturnType<typeof connect>> | undefined;
  let decoder: VideoDecoder | undefined;
  let audioDecoder: AudioDecoder | undefined;
  let muted = audio.muted;
  mute.disabled = false; mute.textContent = muted ? "取消静音" : "静音";
  mute.onclick = () => { muted = !muted; audio.muted = muted; mute.textContent = muted ? "取消静音" : "静音"; };
  const pictures: VideoFrame[] = [];
  let animation = 0, dropped = 0;
  const handles: (() => void)[] = [];
  const close = () => { if (!alive) return; alive = false; attempt.abort(); clearTimeout(startupTimer); clearInterval(watchdog); cancelAnimationFrame(animation); for (const frame of pictures) frame.close(); pictures.length = 0; audio.reset(); if (audioDecoder?.state !== "closed") audioDecoder?.close(); for (const release of [...handles].reverse()) release(); if (decoder?.state !== "closed") decoder?.close(); peer?.close(); signal.removeEventListener("abort", close); };
  const fail = (error: Error) => { if (alive) { failure = error; close(); } };
  signal.addEventListener("abort", close, { once: true });
  if (signal.aborted) { close(); return; }
  startupTimer = setTimeout(() => fail(new RetryablePlaybackError("media startup timed out")), 10000);
  try {
    if (!globalThis.isSecureContext || typeof VideoDecoder === "undefined" || typeof AudioDecoder === "undefined") throw new Error("需要 HTTPS 或 localhost，以及支持音视频 WebCodecs 的浏览器");
    connecting = true;
    const config = await fetch("/config", { signal: attempt.signal }).then(r => { if (!r.ok) throw new Error("media configuration unavailable"); return r.json(); }) as { url: string; path: string; certificateHash?: string };
    if (!alive) { if (failure) throw failure; return; }
    peer = await connect(new URL(config.url), "subscribe", 5000, config.certificateHash, attempt.signal);
    connecting = false;
    metrics.dataset.transport = peer.connection.transport;
    void peer.connection.closed.then(() => fail(new RetryablePlaybackError("media connection closed")));
    if (!alive) { peer.close(); if (failure) throw failure; return; }
    const request = peer.origin.request(Moq.Path.from(config.path)); handles.push(() => request.close());
    const broadcast = await resolveBroadcast(request, 5000);
    if (!alive) { if (failure) throw failure; return; }
    const catalogTrack = broadcast.track("catalog").subscribe(); handles.push(() => catalogTrack.close());
    const catalogGroup = await catalogTrack.recvGroup(); if (!catalogGroup) throw new Error("缺少媒体描述");
    const catalogFrame = await catalogGroup.readFrame(); catalogGroup.close();
    if (!alive) { if (failure) throw failure; return; }
    if (!catalogFrame || catalogFrame.payload.length > 16 * 1024) throw new Error("无效媒体描述");
    const catalog = Msf.decode(catalogFrame.payload);
    const info = catalog.tracks.find(t => t.role === "video");
    if (!info || !(info.codec === "vp8" || info.codec?.startsWith("avc1.")) || info.packaging !== "loc") throw new Error("不支持的媒体格式");
    const description = (value?: string) => value ? Uint8Array.from(atob(value), ch => ch.charCodeAt(0)) : undefined;
    const videoInit = description(info.initData);
    const codec: VideoDecoderConfig = { codec: info.codec!, description: videoInit, codedWidth: info.width, codedHeight: info.height, optimizeForLatency: true };
    if (!(await VideoDecoder.isConfigSupported(codec)).supported) throw new Error("浏览器不支持该视频编码");
    if (!alive) { if (failure) throw failure; return; }
    const audioInfo = catalog.tracks.find(t => t.role === "audio");
    if (!audioInfo || !(audioInfo.codec === "opus" || audioInfo.codec === "mp4a.40.2")) throw new Error("缺少支持的音频轨道");
    const audioConfig: AudioDecoderConfig = { codec: audioInfo.codec!, description: description(audioInfo.initData), sampleRate: audioInfo.samplerate ?? 48000, numberOfChannels: Number(audioInfo.channelConfig ?? 1) };
    if (!(await AudioDecoder.isConfigSupported(audioConfig)).supported) throw new Error("浏览器不支持该音频编码");
    if (!alive) { if (failure) throw failure; return; }
    connecting = false;
    audioDecoder = new AudioDecoder({ output: data => { if (alive) audio.schedule(data); else data.close(); }, error: error => fail(error) });
    audioDecoder.configure(audioConfig);
    clearTimeout(startupTimer);
    lastAudio = lastVideo = performance.now();
    watchdog = setInterval(() => {
      if (performance.now() - Math.min(lastAudio, lastVideo) > 3000) fail(new RetryablePlaybackError("media stalled"));
    }, 500);
    const audioTrack = broadcast.track(audioInfo.name).subscribe({ maxAge: Moq.Time.Milli(0) }); handles.push(() => audioTrack.close());
    void (async () => {
      const loc = new Loc.Format("audio");
      while (alive) {
        const group = await audioTrack.recvGroup(); if (!group) throw new RetryablePlaybackError("audio track ended");
        try { for (;;) {
          const object = await group.readFrame(); if (!object || !alive) break;
          lastAudio = performance.now();
          if (object.payload.length > 65536) throw new Error("音频帧过大");
          for (const frame of loc.decode(object.payload)) {
            if (audioDecoder!.decodeQueueSize > 24) { audioDecoder!.reset(); audioDecoder!.configure(audioConfig); }
            audioDecoder!.decode(new EncodedAudioChunk({ type: "key", timestamp: frame.timestamp, data: frame.payload as Uint8Array<ArrayBuffer> }));
          }
        } } finally { group.close(); }
      }
    })().catch(error => fail(error instanceof Error ? error : new Error(String(error))));
    decoder = new VideoDecoder({ output(frame) {
      if (!alive) { frame.close(); return; }
      if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) { canvas.width = frame.displayWidth; canvas.height = frame.displayHeight; }
      pictures.push(frame);
      while (pictures.length > 12) { pictures.shift()!.close(); dropped++; }
    }, error(error) { fail(error); } });
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
      const group = await video.recvGroup(); if (!group) throw new RetryablePlaybackError("video track ended");
      try { for (;;) {
        const object = await group.readFrame(); if (!object || !alive) break;
        lastVideo = performance.now();
        if (object.payload.length > 1024 * 1024) throw new Error("视频帧过大");
        for (const frame of loc.decode(object.payload)) {
          // 从编码数据识别独立帧；恢复前丢弃依赖帧。
          if (!frame.payload.length) continue;
          const key = info.codec === "vp8" ? (frame.payload[0]! & 1) === 0 : isAvcKeyframe(frame.payload, ((videoInit?.[4] ?? 3) & 3) + 1);
          if (decoder.decodeQueueSize > 8) { decoder.reset(); decoder.configure(codec); needKey = true; }
          if (needKey && !key) continue;
          if (key) { keys++; needKey = false; }
          decoder.decode(new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp: frame.timestamp, data: frame.payload as Uint8Array<ArrayBuffer> }));
          status.textContent = "播放中";
        }
      } } finally { group.close(); }
    }
  } catch (error) {
    if (!failure && !signal.aborted) {
      const cause = error instanceof Error ? error : new Error(String(error));
      failure = connecting ? new RetryablePlaybackError(cause.message) : cause;
    }
  } finally { close(); }
  if (failure && !signal.aborted) throw failure;
}
stop.onclick = () => { cleanup?.(); status.textContent = "已停止"; };
window.addEventListener("pagehide", () => cleanup?.());
