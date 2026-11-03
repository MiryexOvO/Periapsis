/** A transient transport failure eligible for bounded reconnect attempts.
 * 可触发有限次重连的临时传输错误。
 */
export class RetryablePlaybackError extends Error {}

/** Retry failed connections with backoff; cancellation prevents all later attempts.
 * 以退避重试连接；取消后不得继续发起请求。
 */
export async function recoverPlayback(attempt: (signal: AbortSignal) => Promise<void>, signal: AbortSignal,
  notify: (count: number, delay: number) => void, baseDelay = 500, maxRetries = 8) {
  for (let count = 0;; count++) {
    if (signal.aborted) return;
    const started = performance.now();
    try { await attempt(signal); return; }
    catch (error) {
      if (signal.aborted) return;
      if (!(error instanceof RetryablePlaybackError)) throw error;
      // 连续稳定播放十秒后重置失败预算，短时反复断连不会无限重试。
      if (performance.now() - started >= 10000) count = 0;
      if (count >= maxRetries) throw error;
      const delay = Math.min(5000, baseDelay * 2 ** count);
      notify(count + 1, delay);
      await new Promise<void>(resolve => {
        const done = () => { clearTimeout(timer); signal.removeEventListener("abort", done); resolve(); };
        const timer = setTimeout(done, delay);
        signal.addEventListener("abort", done, { once: true });
        if (signal.aborted) done();
      });
    }
  }
}
