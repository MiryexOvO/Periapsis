import { readFile, writeFile } from "node:fs/promises";
import { analyzeFreezes, type TraceEvent } from "./freeze-analysis.ts";
const [input, output] = process.argv.slice(2);
if (!input || !output) throw new Error("usage: native:freeze:check <trace.jsonl> <report.json>");
const events = (await readFile(input,"utf8")).split(/\r?\n/).filter(Boolean).map(line=>JSON.parse(line) as TraceEvent);
const report = analyzeFreezes(events);
await writeFile(output,JSON.stringify(report,null,2));
console.log(JSON.stringify({freezeCount:report.freezeCount,freezeMs:report.freezeMs,unattributed:report.unattributed,recoveryReasons:report.recoveryReasons}));
