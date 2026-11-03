import { PlaybackClock } from "./playback-clock.ts";

/** Schedule decoded audio and provide the master media clock.
 * 调度解码后的音频，以音频设备时钟作为媒体播放基准。
 */
export class AudioPlayback {
  readonly context = new AudioContext({ sampleRate: 48000, latencyHint: "interactive" });
  readonly clock = new PlaybackClock();
  private readonly gain = this.context.createGain();
  private sources = new Set<AudioBufferSourceNode>();
  packets = 0;
  energy = 0;
  closed = false;
  constructor() { this.gain.gain.value = 0.2; this.gain.connect(this.context.destination); void this.context.resume(); }
  set muted(value: boolean) { this.gain.gain.value = value ? 0 : 0.2; }
  schedule(data: AudioData) {
    try {
      if (this.closed) return;
      const now = this.context.currentTime;
      const before = this.clock.mediaTime(now);
      const at = this.clock.schedule(data.timestamp, now);
      // 重建时间映射后取消旧时间轴上已排队的音频，避免叠音或持续积压。
      if (before !== undefined && Math.abs((data.timestamp - before) / 1e6 - (at - now)) > 0.001) this.clear();
      const buffer = this.context.createBuffer(data.numberOfChannels, data.numberOfFrames, data.sampleRate);
      for (let channel = 0; channel < data.numberOfChannels; channel++) {
        const plane = new Float32Array(data.numberOfFrames);
        data.copyTo(plane, { planeIndex: channel, format: "f32-planar" });
        buffer.copyToChannel(plane, channel);
        this.energy = Math.max(this.energy, ...plane.subarray(0, 256).map(Math.abs));
      }
      if (at < now - 0.02 || this.sources.size >= 24) return;
      const source = this.context.createBufferSource(); source.buffer = buffer;
      source.connect(this.gain); this.sources.add(source);
      source.onended = () => { this.sources.delete(source); source.disconnect(); };
      source.start(Math.max(now, at)); this.packets++;
    } finally { data.close(); }
  }
  get mediaTime() { return this.clock.mediaTime(this.context.currentTime); }
  get bufferedMs() { return this.sources.size * 20; }
  private clear() { for (const source of this.sources) { source.stop(); source.disconnect(); } this.sources.clear(); }
  close() { if (this.closed) return; this.closed = true; this.clear(); void this.context.close(); }
}
