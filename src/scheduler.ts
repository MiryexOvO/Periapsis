/** Deadlines use local monotonic milliseconds, never unconverted remote wall time. */
/** 截止时间使用本地单调时钟的毫秒值，不能直接使用远端墙上时钟。 */
export interface MediaUnit {
  id: string;
  track: string;
  bytes: number;
  deadlineMs: number;
  kind: "audio" | "key" | "reference" | "disposable";
  dependencies: string[];
}
export type Decision = { action: "send" | "drop" | "recover"; unit: MediaUnit; reason: string };
/** Advisory media policy; transport retains congestion control and retransmission. */
/** 只提供媒体策略建议；拥塞控制和重传由传输层负责。 */
export class DeadlineQueue {
  private pending: MediaUnit[] = [];
  private usedBytes = 0;
  readonly maxBytes: number;
  readonly maxUnits: number;
  constructor(maxBytes = 4 * 1024 * 1024, maxUnits = 256) {
    if (!Number.isSafeInteger(maxBytes) || maxBytes < 1 || !Number.isSafeInteger(maxUnits) || maxUnits < 1) throw new Error("invalid queue bounds");
    this.maxBytes = maxBytes;
    this.maxUnits = maxUnits;
  }
  get bytes() { return this.usedBytes; }
  get size() { return this.pending.length; }
  push(unit: MediaUnit): void {
    if (!unit.id || !unit.track || !Number.isSafeInteger(unit.bytes) || unit.bytes < 1 ||
        !Number.isFinite(unit.deadlineMs) || unit.dependencies.includes(unit.id)) throw new Error("invalid media unit");
    if (this.pending.some(u => u.id === unit.id)) throw new Error("duplicate media unit");
    if (this.usedBytes + unit.bytes > this.maxBytes || this.size >= this.maxUnits) throw new Error("queue capacity exceeded");
    this.pending.push({ ...unit, dependencies: [...unit.dependencies] });
    this.usedBytes += unit.bytes;
  }
  next(nowMs: number, available: ReadonlySet<string>): Decision | undefined {
    if (!Number.isFinite(nowMs)) throw new Error("invalid monotonic time");
    // 参考数据过期时，由应用显式恢复并处理失效的解码依赖。
    const expired = this.pending.findIndex(u => u.deadlineMs <= nowMs);
    if (expired >= 0) {
      const unit = this.take(expired);
      return { action: unit.kind === "disposable" ? "drop" : "recover", unit, reason: "deadline-expired" };
    }
    const rank = { audio: 0, key: 1, reference: 2, disposable: 3 };
    const ready = this.pending.filter(u => u.dependencies.every(d => available.has(d)))
      .sort((a, b) => a.deadlineMs - b.deadlineMs || rank[a.kind] - rank[b.kind]);
    if (!ready.length) return undefined;
    return { action: "send", unit: this.take(this.pending.indexOf(ready[0]!)), reason: "ready" };
  }
  private take(index: number): MediaUnit {
    const [unit] = this.pending.splice(index, 1);
    this.usedBytes -= unit!.bytes;
    return unit!;
  }
}
