# NovaStream / NSP

基于标准 MoQ 的现代流媒体会话与媒体策略研发工程。面向直播、实时互动、桌面和游戏串流，
支持浏览器与原生端的长期目标；P2P 是可选扩展。是否优于 WebRTC 必须通过可复现实验验证。

**当前是 P0a 传输与策略参考实现，还不能播放音视频。** 已验证 MOQT-22 对象通过本机
Relay 转发到两个订阅端，以及 offer/accept/ack 双向协商。测试使用真实 WebSocket 网络连接，上游 QMux/MOQT 编解码，
不是内存模拟；它也不是 QUIC 性能测试或独立实现互通认证。

## 快速运行

需要 Node.js 24+，支持 **pnpm 和 npm**。已验证 pnpm 11.18.0；项目提供
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

Demo 自动启动随机端口 Relay、建立发布/订阅连接，通过 MOQT 发送 NSP hello，
在接收端执行能力选择，再发送 64 KiB 随机对象并核对 SHA-256。完成后自动关闭。
成功输出包含 `moq-transport-22`、`websocket`、`sentBytes: 65536`、`receivedBytes: 65536`。
`demo` 保留单向 hello 与对象完整性诊断；hello 的结果在接收端本地计算。

`handshake` 演示通过两个 MOQT 控制轨道交换 offer、accept、ack。响应端选择配置，发起端
重新验证配置并发送 ACK，响应端收到 ACK 后进入 ACTIVE。成功输出两端均为 `ACTIVE`。
失败的协商不会返回活动会话；连接关闭后，其协商结果也会失效。

上游库会输出订阅及关闭诊断，不能把 `subscribe ...` 日志当作额外的协议能力。

## 分进程发布和订阅对象

所有终端配置相同的环境变量。以下值仅为本地演示，应替换成随机值：

```powershell
$env:NSP_PUBLISH_TOKEN = 'local-publisher-example-123456'
$env:NSP_SUBSCRIBE_TOKEN = 'local-subscriber-example-654321'
$env:NSP_RELAY_URL = 'ws://127.0.0.1:4443/moq'
```

使用 pnpm 时，终端 A 运行 `pnpm run dev`，终端 B 运行：

```powershell
pnpm run object publish README.md nsp/probe
```

终端 C 运行：

```powershell
pnpm run object subscribe received.bin nsp/probe
```

使用 npm 时，终端 A 运行 `npm run dev`，终端 B 运行：

```powershell
npm run object -- publish README.md nsp/probe
```

终端 C：

```powershell
npm run object -- subscribe received.bin nsp/probe
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

现已加入 Rust workspace：`nsp-types`、`nsp-session` 和 `nsp-scheduler`，作为原生核心。
TypeScript 保留为浏览器 SDK 及对象链路参考。原生 Rust 网络后端、浏览器播放器、
HTTP/MSE 网关、LOC/MSF 媒体映射、音画同步、ABR 和 WebRTC 对比尚待实现。

## Rust 核心

已检测 Rust 1.99.1（MSVC）。当前全 workspace 的 check 和 Clippy 已通过，15 项 Rust
行为测试的代码通过类型检查；测试可执行文件因缺少 `link.exe` 而无法构建，尚未执行。
Windows 需安装 Visual Studio Build Tools 的“使用 C++ 的桌面开发”，包含 MSVC x64/x86
工具及 Windows SDK。安装后新开终端；如果 Cargo 未进入 PATH，可先执行：

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Rust 核心目前不依赖第三方库；会话能力使用结构化类型，JSON 线格式解析暂由 TypeScript
参考端提供。原生传输适配器接入前将固定上游版本，不自写 MOQT/QUIC 握手。

## HTTP / HTTPS 设计

HTTPS 浏览器计划使用 MoQ/WebTransport + WebCodecs。普通远程 HTTP 浏览器计划使用
媒体网关 + MSE。当前 WS 链路验证了 HTTP 兼容环境中的对象传输，**不等于完成 HTTP 视频播放**。
WSS 可以由 TLS 网关终止，但当前参考服务仅允许本机 CLI，拒绝浏览器 Origin，尚不可公网部署。

完整边界、后续里程碑及对比基准见 [协议规格](specs/NSP-CORE.md)，固定依赖和标准兼容性
记录见 [依赖基线](specs/DEPENDENCIES.md)，测试结果见 [验证记录](specs/VALIDATION.md)。
公共 API 注释使用中英双语，内部逻辑注释使用中文，详见 [开发约定](specs/CONTRIBUTING.md)。
