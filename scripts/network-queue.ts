/** Bounded downstream serializer. Rejected packets consume no queue capacity or time. */
export function reserveDatagram(now: number, next: number, bytes: number, queued: number, impaired: boolean): { next: number; delay: number } | undefined {
  const candidate = Math.max(now, next) + (impaired ? bytes / 24 : 0);
  const delay = candidate - now + (impaired ? 45 : 0);
  if (queued >= 64 || delay > 250) return undefined;
  return { next: candidate, delay };
}
