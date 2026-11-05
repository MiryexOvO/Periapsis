// 本工具关联同机实验日志中的证据，不把时间上相邻的事件当作已证明的根因。
export interface TraceEvent {
  event: string;
  process: number;
  wall_ms: number;
  data: Record<string, unknown>;
}

export function analyzeFreezes(events: TraceEvent[]) {
  const outputs = new Map<number, TraceEvent[]>();
  for (const event of events) {
    if (!Number.isSafeInteger(event.wall_ms) || !Number.isSafeInteger(event.process)) {
      throw new Error("invalid trace clock or process");
    }
    if (event.event === "frame_output") {
      const group = outputs.get(event.process) ?? [];
      // 保留输入顺序检查时钟回退，不用排序掩盖异常。
      if (group.length && event.wall_ms < group.at(-1)!.wall_ms) throw new Error("output clock reversed");
      group.push(event);
      outputs.set(event.process, group);
    }
  }
  const gaps = [];
  for (const [process, frames] of outputs) {
    for (let index = 1; index < frames.length; index++) {
      const before = frames[index - 1]!, after = frames[index]!;
      const gapMs = after.wall_ms - before.wall_ms;
      if (gapMs <= 250) continue;
      const window = events.filter(e => e.wall_ms > before.wall_ms && e.wall_ms <= after.wall_ms);
      const player = window.filter(e => e.process === process);
      const recovery = player.filter(e => e.event === "player_recovery").map(e => e.data);
      const reads = player.filter(e => e.event === "player_receive");
      const decodes = player.filter(e => e.event === "player_decode");
      gaps.push({
        process, fromSequence: before.data.sequence, toSequence: after.data.sequence,
        startWallMs: before.wall_ms, endWallMs: after.wall_ms, gapMs, freezeMs: gapMs - 67,
        recovery, observation: recovery.length ? "recovery_observed" : "unattributed",
        // 阶段耗时可能跨越间隔边界，仅提供最大值，不累加为整个停顿耗时。
        maxReceiveMs: maximum(reads, "elapsed_ms"), maxDecodeMs: maximum(decodes, "elapsed_ms"),
        decodeFailures: decodes.filter(e => e.data.outcome !== "ok").map(e => e.data),
        readEnds: player.filter(e=>e.event==="object_read_end" || e.event==="retained_slot_expired").map(e=>({event:e.event,...e.data})),
        sendCancels: window.filter(e=>e.event==="object_send_cancel").map(e=>({process:e.process,...e.data})),
        // 其他进程的事件需要同机墙钟；多观看者时这里只能作为共享上下文。
        relayEvidence: window.filter(e => ["relay_drop", "relay_send_failed", "relay_start_failed"].includes(e.event)).map(e => ({event:e.event,...e.data})),
        keys: window.filter(e => ["encoder_response", "relay_key_send", "key_received", "key_decoded"].includes(e.event)).map(e => ({event:e.event,process:e.process,wall_ms:e.wall_ms,...e.data})),
      });
    }
  }
  return {
    scope: "same-host-software-output-evidence", thresholdMs: 250, nominalFrameMs: 67,
    gaps: gaps.sort((a,b) => a.startWallMs-b.startWallMs),
    freezeCount: gaps.length, freezeMs: gaps.reduce((sum,g) => sum+g.freezeMs,0),
    unattributed: gaps.filter(g => g.observation === "unattributed").length,
    recoveryReasons: countReasons(events.filter(e => e.event === "player_recovery")),
    objectFailures: analyzeObjectFailures(events),
    limitations: "Wall clock association is not causality. Start/end stalls without two outputs are excluded. Shared relay events are not viewer attribution.",
  };
}

// 只输出同序号/流号的候选关联，未知头部不匹配；跨连接的流号可能重复，不能据此断言因果。
export function analyzeObjectFailures(events: TraceEvent[]) {
  const reads=events.filter(e=>e.event==="object_read_end");
  const sends=events.filter(e=>e.event==="object_send_cancel");
  return {
    readReasons:countReasons(reads), sendReasons:countReasons(sends),
    reads:reads.map(read=>({
      process:read.process,wall_ms:read.wall_ms,...read.data,
      sendCandidates:typeof read.data.sequence==="number" && typeof read.data.stream==="number"
        ? sends.filter(send=>send.process!==read.process && send.data.sequence===read.data.sequence && send.data.stream===read.data.stream && send.wall_ms<=read.wall_ms && read.wall_ms-send.wall_ms<=1000)
            .map(send=>({process:send.process,wall_ms:send.wall_ms,...send.data})) : [],
    })),
    limitation:"Candidates share sequence/stream within 1s on the same host. Connection identity and causality are not proven; partial headers remain unknown.",
  };
}
function maximum(events: TraceEvent[], field: string) {
  const values = events.map(e=>e.data[field]).filter((v):v is number => typeof v==="number" && Number.isFinite(v));
  return values.length ? Math.max(...values) : null;
}
function countReasons(events: TraceEvent[]) {
  const counts: Record<string,number> = {};
  for (const event of events) {
    const reason = String(event.data.reason ?? "unknown");
    counts[reason] = (counts[reason] ?? 0)+1;
  }
  return counts;
}
