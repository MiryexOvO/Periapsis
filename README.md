# Periapsis / PERI

基于标准 MoQ 的现代流媒体会话与媒体策略研发工程。面向直播、实时互动、桌面和游戏串流，
支持浏览器与原生端的长期目标；P2P可选

目前提供 MOQT-22 对象发布/订阅、本地 Relay 和 offer/accept/ack 双向会话协商，
以及 Rust 原生 QUIC 直连适配，尚不支持音视频播放。浏览器参考传输使用 QMux/WebSocket。

## 快速运行

需要 Node.js 24+，支持 **pnpm 和 npm**。项目使用 pnpm 11.18.0，提供
`pnpm-lock.yaml` 和 `package-lock.json`，安装时选用对应的锁文件。

在本目录打开 PowerShell，使用 pnpm：

```powershell
pnpm install --frozen-lockfile --ignore-scripts
pnpm run check
pnpm test
pnpm run demo
pnpm run handshake
```

也可以使用 npm：

```powershell
npm ci --ignore-scripts
npm run check
npm test
npm run demo
npm run handshake
```

Demo 自动启动随机端口 Relay、建立发布/订阅连接，通过 MOQT 发送 PERI hello，
在接收端执行能力选择，再发送 64 KiB 随机对象并核对 SHA-256。完成后自动关闭。
成功输出包含 `moq-transport-22`、`websocket`、`sentBytes: 65536`、`receivedBytes: 65536`。
`demo` 保留单向 hello 与对象完整性诊断；hello 的结果在接收端本地计算。

`handshake` 演示通过两个 MOQT 控制轨道交换 offer、accept、ack。响应端选择配置，发起端
重新验证配置并发送 ACK，响应端收到 ACK 后进入 ACTIVE。成功输出两端均为 `ACTIVE`。
失败的协商不会返回活动会话；连接关闭后，其协商结果也会失效。

## 分进程发布和订阅对象

所有终端配置相同的环境变量。以下值仅为本地演示，应替换成随机值：

```powershell
$env:PERI_PUBLISH_TOKEN = 'local-publisher-example-123456'
$env:PERI_SUBSCRIBE_TOKEN = 'local-subscriber-example-654321'
$env:PERI_RELAY_URL = 'ws://127.0.0.1:4443/moq'
```

使用 pnpm 时，终端 A 运行 `pnpm run dev`，终端 B 运行：

```powershell
pnpm run object publish README.md peri/probe
```

终端 C 运行：

```powershell
pnpm run object subscribe received.bin peri/probe
```

使用 npm 时，终端 A 运行 `npm run dev`，终端 B 运行：

```powershell
npm run object -- publish README.md peri/probe
```

终端 C：

```powershell
npm run object -- subscribe received.bin peri/probe
```

发布端保持运行至 Ctrl+C。接收端拒绝覆盖现有文件。对象限制 1 byte–1 MiB；这是通用对象
链路诊断，不是文件流媒体、OBS 或实际视频接入。健康检查为 `http://127.0.0.1:4443/health`。

## 代码入口

| 文件               | 当前职责                                                |
| ------------------ | ------------------------------------------------------- |
| `src/session.ts`   | 有界 hello 解析、版本/配置协商、关键扩展拒绝            |
| `src/handshake.ts` | 双向协商状态机、会话关联、配置复核与确认顺序            |
| `src/peer.ts`      | 真实 MOQT 控制轨道上的 offer/accept/ack 交换            |
| `src/scheduler.ts` | 本地单调时间 deadline、依赖就绪检查、有界队列和恢复建议 |
| `src/transport.ts` | draft-22 严格选择、对象发布/订阅、连接与收取超时        |
| `src/relay.ts`     | 本机 QMux/WebSocket 参考 Relay、角色凭据、广播路由      |
| `src/browser.ts`   | 浏览器可打包 SDK 入口；WebTransport 路径尚未浏览器实测  |
| `src/cli.ts`       | 独立发布/订阅探针                                       |

## Rust 核心

Rust workspace 包含共用类型、会话协商、媒体调度和原生传输四个模块。需要 Rust 1.91+；
Windows MSVC 目标需安装 Visual Studio 的“使用 C++ 的桌面开发”和 Windows SDK。

```powershell
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

单独运行原生 QUIC 对象传输检查：

```powershell
cargo test -p peri-transport --test native_quic
```

该测试使用本机 UDP、TLS 证书校验和固定的 MOQT-22 ALPN，比较发送与接收的对象内容。
它不包含 Relay 转发、音视频播放或独立 MoQ 实现之间的互通验证。

控制消息格式互通示例（需要 Cargo）：

```powershell
pnpm run interop
```

该命令通过本地进程管道连接 TypeScript 发起端与 Rust 响应端，交换 offer、accept、ack。
它用于检查控制消息格式，不包含 QUIC 网络传输。

## HTTP / HTTPS 设计

HTTPS 浏览器计划使用 MoQ/WebTransport + WebCodecs。普通远程 HTTP 浏览器计划使用
媒体网关 + MSE。当前 WS 链路验证了 HTTP 兼容环境中的对象传输，**不等于完成 HTTP 视频播放**。
WSS 可以由 TLS 网关终止，但当前参考服务仅允许本机 CLI，拒绝浏览器 Origin，尚不可公网部署。
