import type * as Moq from "@moq/net";

/** Resolve a routed broadcast within a deadline and release signal subscriptions. */
/** 在限定时间内解析广播路由，并在结束时释放信号订阅。 */
export async function resolveBroadcast(request: Moq.Origin.Requesting, timeoutMs: number): Promise<Moq.Broadcast.Consumer> {
  if (request.closed.peek() !== undefined) throw new Error("broadcast request closed");
  const current = request.active.peek();
  if (current) return current;
  return new Promise((resolve, reject) => {
    let settled = false;
    const subscriptions: (() => void)[] = [];
    const finish = (broadcast?: Moq.Broadcast.Consumer, error?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      for (const unsubscribe of subscriptions) unsubscribe();
      if (broadcast) resolve(broadcast); else reject(error ?? new Error("broadcast request closed"));
    };
    const timer = setTimeout(() => finish(undefined, new Error("broadcast resolution timed out")), timeoutMs);
    subscriptions.push(request.active.subscribe((value: Moq.Broadcast.Consumer | undefined) => { if (value) finish(value); }));
    subscriptions.push(request.closed.subscribe((value: Error | null | undefined) => { if (value !== undefined) finish(undefined, value ?? undefined); }));
    // 注册信号订阅后再次检查，避免错过注册期间的状态变化。
    const active = request.active.peek();
    if (active) finish(active);
    if (settled) for (const unsubscribe of subscriptions) unsubscribe();
  });
}
