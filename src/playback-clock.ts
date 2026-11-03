/** Map media microseconds onto the audio device timeline with bounded lead.
 * 将媒体微秒映射到音频设备时间轴，并限制预排队时长。
 */
export class PlaybackClock {
  private media?: number;
  private device = 0;
  readonly target: number;
  readonly maximum: number;
  constructor(target = 0.12, maximum = 0.35) {
    if (!Number.isFinite(target) || !Number.isFinite(maximum) || target <= 0 || maximum < target) throw new Error("invalid buffer policy");
    this.target = target; this.maximum = maximum;
  }
  reset() { this.media = undefined; }
  schedule(timestamp: number, now: number): number {
    if (!Number.isFinite(timestamp) || !Number.isFinite(now)) throw new Error("invalid playback time");
    if (this.media === undefined) { this.media = timestamp; this.device = now + this.target; }
    let at = this.device + (timestamp - this.media) / 1e6;
    if (at < now - 0.06 || at > now + this.maximum) {
      this.media = timestamp; this.device = now + this.target; at = this.device;
    }
    return at;
  }
  mediaTime(now: number): number | undefined { return this.media === undefined ? undefined : this.media + (now - this.device) * 1e6; }
}
