// Display-time metrics include stalls at both boundaries, even with zero output.
// 按显示时间统计，包含窗口两端及完全无输出时的停顿，避免只统计成功帧。
export interface DisplayFrame { at: number; pts: number; transport: string }
export interface PixelTick { at: number; lagMs: number | null; transport: string }
export function summarizeDisplay(frames: DisplayFrame[], ticks: PixelTick[], start: number, end: number, fps = 60) {
  const times = frames.map(f => f.at).sort((a,b)=>a-b);
  const inside = frames.filter(f=>f.at>=start && f.at<end);
  const nominal = 1000/fps;
  let previous = times.filter(t=>t<start).at(-1) ?? start;
  let freezeMs = 0, maxGapMs = 0, freezeCount = 0;
  for (const next of [...times.filter(t=>t>=start && t<=end), end]) {
    const gap = next-previous;
    const clipped = Math.max(0, Math.min(next,end)-Math.max(previous,start));
    maxGapMs = Math.max(maxGapMs,clipped);
    if (gap>250) {
      freezeCount++;
      freezeMs += Math.max(0,Math.min(next,end)-Math.max(previous+nominal,start));
    }
    previous=next;
  }
  const samples=ticks.filter(t=>t.at>=start && t.at<end);
  const lag=samples.flatMap(t=>t.lagMs===null?[]:[t.lagMs]).sort((a,b)=>a-b);
  const percentile=(p:number)=>lag.length?lag[Math.floor((lag.length-1)*p)]!:null;
  return {durationMs:end-start,renderedFrames:inside.length,renderFps:inside.length*1000/(end-start),
    freezeCount,freezeMs,freezeRatio:freezeMs/(end-start),maxGapMs,
    pixelSamples:lag.length,missingPixelSamples:samples.length-lag.length,
    pixelP50Ms:percentile(.5),pixelP95Ms:percentile(.95),pixelP99Ms:percentile(.99),pixelMaxMs:lag.at(-1)??null,
    transports:[...new Set(samples.map(t=>t.transport))]};
}
