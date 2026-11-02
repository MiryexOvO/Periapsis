import { createServer, type Server } from "node:http";
import { timingSafeEqual } from "node:crypto";
import { WebSocketServer, type WebSocket } from "ws";
import QMux from "@moq/qmux";
import * as Moq from "@moq/net";
import { QMUX_PROTOCOL } from "./transport.ts";
import { resolveBroadcast } from "./resolve.ts";

export interface RelayOptions {
  host?: string;
  port?: number;
  publishToken: string;
  subscribeToken: string;
  peerToken?: string;
  maxConnections?: number;
}
function equal(a: string, b: string): boolean {
  const aa = Buffer.from(a), bb = Buffer.from(b);
  return aa.length === bb.length && timingSafeEqual(aa, bb);
}
/** Local reference relay. QMux/WebSocket results are not a QUIC performance baseline. */
/** 本地参考 Relay；QMux/WebSocket 结果不代表 QUIC 性能。 */
export async function startRelay(options: RelayOptions) {
  if (options.publishToken.length < 16 || options.subscribeToken.length < 16 || options.publishToken === options.subscribeToken) {
    throw new Error("distinct publisher/subscriber tokens of at least 16 characters are required");
  }
  if (options.peerToken !== undefined && (options.peerToken.length < 16 ||
      [options.publishToken, options.subscribeToken].includes(options.peerToken))) {
    throw new Error("peer token must be distinct and at least 16 characters");
  }
  const host = options.host ?? "127.0.0.1";
  if (!["127.0.0.1", "::1"].includes(host)) throw new Error("reference relay only binds to loopback; deploy behind an authenticated TLS gateway");
  const max = options.maxConnections ?? 32;
  if (!Number.isSafeInteger(max) || max < 1) throw new Error("invalid connection limit");
  const origin = new Moq.Origin.Producer();
  const served = new Moq.Origin.Producer();
  // 上游客户端不重复公告远端路由；Relay 发布自己的服务路由，再查找入站发布端。
  const service = served.dynamic(Moq.Path.empty());
  void (async () => {
    for await (const request of service.requested()) {
      const pending = origin.request(request.path, { announced: true });
      try {
        const active = await resolveBroadcast(pending, 3000);
        request.accept(active.clone());
      } catch { request.reject(new Error("publisher unavailable")); }
      finally { pending.close(); }
    }
  })().catch(() => {});
  let accepted = 0;
  const sessions = new Set<Moq.Connection.Established>();
  const server: Server = createServer((req, res) => {
    if (req.url === "/health") {
      res.writeHead(200, { "content-type": "application/json", "cache-control": "no-store" });
      res.end(JSON.stringify({ protocol: "moqt-22", transport: "qmux-01/websocket", sessions: sessions.size, accepted }));
    } else { res.writeHead(404); res.end(); }
  });
  const sockets = new WebSocketServer({ noServer: true, maxPayload: 1024 * 1024, perMessageDeflate: false,
    handleProtocols: protocols => protocols.has(QMUX_PROTOCOL) ? QMUX_PROTOCOL : false });
  server.on("upgrade", (req, socket, head) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    const role = url.searchParams.get("role");
    const expected = role === "publish" ? options.publishToken : role === "subscribe" ? options.subscribeToken : role === "peer" ? options.peerToken : "";
    const offered = (req.headers["sec-websocket-protocol"] ?? "").split(",").map(p => p.trim());
    const reject = (code: number, message: string) => { socket.end(`HTTP/1.1 ${code} ${message}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`); };
    if (url.pathname !== "/moq" || !offered.includes(QMUX_PROTOCOL)) return reject(400, "Bad Request");
    if (!expected || !equal(url.searchParams.get("token") ?? "", expected)) return reject(401, "Unauthorized");
    if (sockets.clients.size >= max) return reject(503, "Service Unavailable");
    // 浏览器来源需要显式授权策略；本地命令行连接不携带 Origin。
    if (req.headers.origin) return reject(403, "Origin policy not configured");
    sockets.handleUpgrade(req, socket, head, ws => {
      accepted++;
      void attach(ws, role as "publish" | "subscribe" | "peer");
    });
  });
  async function attach(socket: WebSocket, role: "publish" | "subscribe" | "peer") {
    // ws 提供上游 QMux 适配器需要的浏览器事件接口。
    const transport = QMux.accept(socket as unknown as Parameters<typeof QMux.accept>[0], {
      config: { maxStreamsBidi: 32n, maxStreamsUni: 64n, maxData: 4n * 1024n * 1024n,
        maxStreamDataUni: 1024n * 1024n, maxIdleTimeout: 10_000n },
    });
    const timeout = setTimeout(() => transport.close(), 5000);
    let connection: Moq.Connection.Established | undefined;
    try {
      await transport.ready;
      connection = await Moq.Connection.accept({ transport, url: new URL("http://localhost/moq"),
        ...(role !== "subscribe" ? { consume: origin } : {}),
        ...(role !== "publish" ? { publish: served.consume() } : {}) });
      clearTimeout(timeout);
      sessions.add(connection);
      await connection.closed;
    } catch {
      // 日志中不记录凭据和请求地址，避免泄露访问令牌。
      transport.close();
    } finally {
      clearTimeout(timeout);
      if (connection) sessions.delete(connection);
      socket.close(1000);
    }
  }
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(options.port ?? 0, host, () => { server.off("error", reject); resolve(); });
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("unexpected listen address");
  const url = new URL(`ws://${host === "::1" ? "[::1]" : host}:${address.port}/moq`);
  const healthUrl = new URL(`http://${host === "::1" ? "[::1]" : host}:${address.port}/health`);
  return { url, healthUrl, async close() {
    for (const session of sessions) session.close();
    origin.close();
    service.close();
    served.close();
    for (const client of sockets.clients) client.close(1000);
    await new Promise<void>(resolve => {
      const timeout = setTimeout(() => { for (const client of sockets.clients) client.terminate(); }, 500);
      sockets.close(() => { clearTimeout(timeout); resolve(); });
    });
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  } };
}
