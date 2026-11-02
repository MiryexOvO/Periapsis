import { spawn, spawnSync } from "node:child_process";
import { createInterface } from "node:readline";
import { once } from "node:events";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import { Handshake, encodeControl, decodeControl } from "../src/handshake.ts";
import { defaultHello } from "../src/session.ts";

// 显式构建真实 Rust 程序，避免将 TypeScript 内部往返误认为跨语言互通。
const root = fileURLToPath(new URL("../", import.meta.url));
const build = spawnSync("cargo", ["build", "--locked", "-p", "nsp-session", "--example", "responder"], { cwd: root, stdio: "inherit", windowsHide: true });
if (build.error) throw build.error;
if (build.status !== 0) throw new Error("Rust responder build failed");
const binary = resolve(process.env.CARGO_TARGET_DIR ?? resolve(root, "target"), "debug/examples", process.platform === "win32" ? "responder.exe" : "responder");
const id = randomBytes(16).toString("hex");
const processPeer = spawn(binary, [id], { cwd: root, stdio: ["pipe", "pipe", "inherit"], windowsHide: true });
const exited = once(processPeer, "exit");
const lines = createInterface({ input: processPeer.stdout });
const reader = lines[Symbol.asyncIterator]();
const timer = setTimeout(() => processPeer.kill(), 5000);
try {
  const initiator = new Handshake("initiator", id, defaultHello());
  initiator.transportReady("moq-transport-22");
  processPeer.stdin.write(Buffer.concat([encodeControl(initiator.start()!), Buffer.from("\n")]));
  const acceptance = await reader.next();
  if (acceptance.done) throw new Error("Rust peer ended before ACCEPT");
  const ack = initiator.receive(decodeControl(new TextEncoder().encode(acceptance.value)))!;
  processPeer.stdin.write(Buffer.concat([encodeControl(ack), Buffer.from("\n")]));
  initiator.ackSent();
  const result = await reader.next();
  if (result.done || JSON.parse(result.value).state !== "ACTIVE") throw new Error("Rust peer did not activate");
  const [code] = await exited;
  if (code !== 0) throw new Error(`Rust peer exited with ${code}`);
  console.log(JSON.stringify({ initiator: "TypeScript", responder: "Rust", states: [initiator.state, "ACTIVE"], agreement: initiator.agreement, transport: "stdio (control-format interop only)" }, null, 2));
} finally { clearTimeout(timer); lines.close(); processPeer.stdin.end(); if (processPeer.exitCode === null) processPeer.kill(); }
