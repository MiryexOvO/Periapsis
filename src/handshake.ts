import { negotiate, parseHello, MAX_CONTROL_BYTES, type Agreement, type Hello } from "./session.ts";

export type State = "TRANSPORT_PENDING" | "MOQ_READY" | "AWAITING_OFFER" | "AWAITING_ACCEPT" |
  "AWAITING_ACK" | "ACK_PENDING" | "ACTIVE" | "REJECTED" | "CLOSED";
export type Role = "initiator" | "responder";
export type Control =
  { type: "nsp.offer"; sessionId: string; hello: Hello } |
  { type: "nsp.accept"; sessionId: string; hello: Hello; agreement: unknown } |
  { type: "nsp.ack"; sessionId: string } |
  { type: "nsp.reject"; sessionId: string };

const validId = (value: unknown): value is string => typeof value === "string" && /^[a-f0-9]{32}$/.test(value);
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);

/** Encode bounded application control objects without modifying MOQT framing.
 * 编码有大小限制的应用层控制对象，沿用 MOQT 自身的帧格式。
 */
export function encodeControl(message: Control): Uint8Array {
  const bytes = new TextEncoder().encode(JSON.stringify(message));
  decodeControl(bytes);
  return bytes;
}

/** Validate the control envelope before passing it to the state machine.
 * 控制消息进入状态机前，先校验封装、会话标识和嵌套能力声明。
 */
export function decodeControl(bytes: Uint8Array): Control {
  if (bytes.byteLength > MAX_CONTROL_BYTES) throw new Error("control message too large");
  const value: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  if (!object(value) || !validId(value.sessionId)) throw new Error("invalid control envelope");
  const sessionId = value.sessionId;
  if (value.type === "nsp.offer" || value.type === "nsp.accept") {
    const hello = parseHello(new TextEncoder().encode(JSON.stringify(value.hello)));
    if (value.type === "nsp.offer") return { type: value.type, sessionId, hello };
    if (!object(value.agreement)) throw new Error("invalid agreement");
    return { type: value.type, sessionId, hello, agreement: value.agreement };
  }
  if (value.type === "nsp.ack" || value.type === "nsp.reject") return { type: value.type, sessionId };
  throw new Error("unsupported control message");
}

function agreementEquals(value: unknown, expected: Agreement): boolean {
  if (!object(value) || value.type !== expected.type || value.version !== expected.version ||
      value.profile !== expected.profile || value.targetBufferMs !== expected.targetBufferMs ||
      value.maxBufferMs !== expected.maxBufferMs || !Array.isArray(value.codecs) ||
      value.codecs.length !== expected.codecs.length || !value.codecs.every((c, i) => c === expected.codecs[i]) ||
      !Array.isArray(value.extensions) || value.extensions.length !== expected.extensions.length) return false;
  return value.extensions.every((e, i) => object(e) && e.id === expected.extensions[i]!.id && e.version === expected.extensions[i]!.version);
}

/** Enforce a correlated offer/accept/ack exchange before media activation.
 * 强制执行带会话关联的双向协商，收到有效确认后才允许进入媒体活动状态。
 */
export class Handshake {
  private current: State = "TRANSPORT_PENDING";
  private agreed?: Agreement;
  private readonly local: Hello;
  readonly sessionId: string;
  readonly role: Role;
  constructor(role: Role, sessionId: string, hello: Hello) {
    if (!validId(sessionId)) throw new Error("invalid session id");
    this.role = role; this.sessionId = sessionId;
    this.local = parseHello(new TextEncoder().encode(JSON.stringify(hello)));
  }
  get state(): State { return this.current; }
  get agreement(): Agreement | undefined {
    return this.current === "ACTIVE" && this.agreed ? structuredClone(this.agreed) : undefined;
  }
  transportReady(version: string): void {
    if (this.current !== "TRANSPORT_PENDING") this.fail("unexpected transport transition");
    if (version !== "moq-transport-22") this.fail("unsupported MOQT version");
    this.current = "MOQ_READY";
  }
  start(): Control | undefined {
    if (this.current !== "MOQ_READY") this.fail("unexpected start");
    if (this.role === "responder") { this.current = "AWAITING_OFFER"; return; }
    this.current = "AWAITING_ACCEPT";
    return { type: "nsp.offer", sessionId: this.sessionId, hello: structuredClone(this.local) };
  }
  receive(message: Control): Control | undefined {
    if (!["AWAITING_OFFER", "AWAITING_ACCEPT", "AWAITING_ACK"].includes(this.current)) this.fail("unexpected control message");
    if (message.sessionId !== this.sessionId) this.fail("session mismatch");
    try {
      if (message.type === "nsp.reject") this.fail("peer rejected negotiation");
      if (this.current === "AWAITING_OFFER" && message.type === "nsp.offer") {
        this.agreed = negotiate(this.local, message.hello);
        this.current = "AWAITING_ACK";
        return { type: "nsp.accept", sessionId: this.sessionId, hello: structuredClone(this.local), agreement: structuredClone(this.agreed) };
      }
      if (this.current === "AWAITING_ACCEPT" && message.type === "nsp.accept") {
        // 响应端按自己的偏好选择；发起端重新计算，拒绝放宽限制或伪造能力。
        const expected = negotiate(message.hello, this.local);
        if (!agreementEquals(message.agreement, expected)) this.fail("agreement mismatch");
        this.agreed = expected; this.current = "ACK_PENDING";
        return { type: "nsp.ack", sessionId: this.sessionId };
      }
      if (this.current === "AWAITING_ACK" && message.type === "nsp.ack") { this.current = "ACTIVE"; return; }
      this.fail("unexpected control message");
    } catch (error) { return this.fail(error instanceof Error ? error.message : "negotiation failed"); }
  }
  /** Commit activation only after the ACK is handed to the transport.
   * ACK 成功交给传输层后，发起端才能启用协商结果。
   */
  ackSent(): void {
    if (this.current !== "ACK_PENDING") this.fail("unexpected ACK completion");
    this.current = "ACTIVE";
  }
  close(): void { this.current = "CLOSED"; this.agreed = undefined; }
  private fail(message: string): never {
    if (this.current !== "CLOSED") this.current = "REJECTED";
    this.agreed = undefined; throw new Error(message);
  }
}
