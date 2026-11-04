// 固定网络配方与随机种子；同一包序列可复现丢包决策，包时序仍由实际传输决定。
export const profiles = {
  short: { bytesPerSecond:24000, delayMs:45, durationMs:2000, loss:0, every:11, blackoutEveryMs:0, blackoutStartMs:300, blackoutMs:180 },
  capacity: { bytesPerSecond:48000, delayMs:20, durationMs:60000, loss:0, every:0, blackoutEveryMs:0, blackoutStartMs:0, blackoutMs:0 },
  delay: { bytesPerSecond:64000, delayMs:100, durationMs:60000, loss:.01, every:0, blackoutEveryMs:0, blackoutStartMs:0, blackoutMs:0 },
  loss: { bytesPerSecond:64000, delayMs:45, durationMs:60000, loss:.05, every:0, blackoutEveryMs:0, blackoutStartMs:0, blackoutMs:0 },
  burst: { bytesPerSecond:48000, delayMs:45, durationMs:60000, loss:.01, every:0, blackoutEveryMs:5000, blackoutStartMs:300, blackoutMs:180 },
} as const;
export function seededRandom(seed:number) {
  let state=seed>>>0;
  return () => { state=(Math.imul(state,1664525)+1013904223)>>>0; return state/4294967296; };
}
