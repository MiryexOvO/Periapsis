// 下行发送队列按真实容量计时；拒绝的包不占用后续发送时间，容量和延迟均由配方传入。
/** Bounded downstream serializer. Rejected packets consume no queue capacity or time. */
export function reserveDatagram(now: number, next: number, bytes: number, queued: number, impaired: boolean, bytesPerSecond = 24000, delayMs = 45, limits = { packets: 64, delayMs: 250 }): { next: number; delay: number } | undefined {
  const candidate = Math.max(now, next) + (impaired ? bytes * 1000 / bytesPerSecond : 0);
  const delay = candidate - now + (impaired ? delayMs : 0);
  if (queued >= limits.packets || delay > limits.delayMs) return undefined;
  return { next: candidate, delay };
}
