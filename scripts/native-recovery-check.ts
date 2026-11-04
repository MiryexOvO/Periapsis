// 候选参数可由环境覆盖并记录，但严格亚秒失败门禁始终开启。
// Reproducible candidate acceptance; strict thresholds cannot be disabled here.
process.env.PERI_RECOVERY_MS ??= "333";
process.env.PERI_MAX_INTRA_PCT ??= "100";
process.env.PERI_RECOVERY_READ_MS ??= "300";
process.env.PERI_NETWORK_REPEATS ??= "5";
process.env.PERI_REQUIRE_SUBSECOND = "1";
await import("./native-live-check.ts");
export {};
