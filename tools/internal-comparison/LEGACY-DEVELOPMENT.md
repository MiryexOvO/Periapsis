# 内部历史开发说明

以下保存旧实验入口，仅供维护人员查阅，不属于用户接入说明。

# Periapsis / PERI

**基于 QUIC 的低延迟实时媒体传输协议。**

这是一个实验性项目！面向推流、实时互动、桌面和游戏串流，目标是在延迟预算内兼顾画质、播放连续性和弱网恢复。
面向浏览器与原生客户端，支持服务器分发的研发方向，P2P 为可选研究方向。
采集、场景编辑和编码由外部推流软件负责；软件通过原生输出插件或 SDK 接入。

## 项目状态

项目处于协议研发阶段，尚无稳定协议或性能领先承诺。

`peri-native` 提供独立原生实验线协议：使用 `peri-dev-01` ALPN、明确的控制消息封装、
能力协商及独立 QUIC 对象流，支持有界并发发送和超时取消。
详见 [原生实验线协议](docs/PERI-WIRE-01.md)。

现有网页演示仍使用早期第三方对象传输与媒体封装依赖，尚未切换到新线协议。
原生媒体层提供轨道目录、编码初始化数据、依赖标识与有界接收解码缓冲，
使用本地播放截止时间决定解码、丢弃和恢复，依赖需要实际解码确认。
详见 [media-01 实验媒体配置](docs/PERI-MEDIA-01.md)。
原生发布接入和单资源 Relay 库已实现，提供 AV1 编码源经 Relay 到双接收端的保真示例。
另提供分进程 Relay、AV1 文件发布与录制命令，使用证书校验及独立发布/订阅凭据。
原生 AV1 GOP 视频可持续解码播放，支持本地播放时间映射、截止时间恢复和有限重连；
已提供关键帧请求、接收报告及可选单观看端调节实验；原生音频同步、稳定 SDK 与推流插件尚待实现。
MPQUIC 定位为可选原生端多路径增强模块，当前尚未实现，单路径不依赖它运行。

主线是原生 QUIC 发布、Relay 分发和浏览器 WebTransport 接收。
已有 WebSocket 和 RTMP 入口保留为早期实验工具，不作为正式协议的必需组成部分。
原生输出插件与 SDK 尚未完成，不能让任意推流软件仅填写地址便直接接入。

## 快速运行

### 独立原生 PERI 传输

需要 Rust 1.91+ 和系统链接工具：

```powershell
cargo test -p peri-native
```

测试建立本机真实 QUIC/TLS 连接，校验资源和能力协商、有界对象传递、并发发送、
错误协商拒绝、超时处理，以及媒体目录和依赖调度。该模块提供 Rust API 和实验文件发布命令，暂未提供推流软件插件。
它与下方使用早期传输依赖的网页演示属于不同链路。

### 原生分进程 Relay、发布与接收

```powershell
cargo build -p peri-native --bin peri
.\target\debug\peri.exe --help
```

程序提供init、relay、publish、live、receive、play命令；Linux/macOS使用`./target/debug/peri`。
服务读取独立发布/订阅凭据，客户端校验证书和服务器名称；文件发布支持受限 AV1 GOP，另有实时合成编码源。
证书、凭据和三终端使用步骤见[原生服务说明](docs/PERI-SERVICE-01.md)。
可用`pnpm run native:check`或`npm run native:check`验证分进程保真、鉴权拒绝和重新发布。
需要Node.js、Rust及AV1 FFmpeg；原生观看采用实验 FFplay 输出；下方网页演示仍使用早期链路。

### 原生发布与 AV1 Relay 验证

需要支持 libaom-av1 编码及 AV1 解码的 FFmpeg：

```powershell
cargo run -p peri-native --example relay_video
```

示例生成 320×180、15 fps 的 AV1 全关键帧片段，经原生 PERI 发布接入和 Relay
分发给两个接收端，检查编码数据及解码后的逐帧校验值一致。
输出位于 `dist/native-relay/`，可通过 `PERI_FFMPEG` 指定 FFmpeg 路径。
这是单进程编排、本机真实 QUIC/TLS 的链路示例，该示例不使用分进程服务，也不提供实时观看 UI。
全关键帧片段用于内容保真验证，不代表生产编码配置或低延迟性能基准。
详见 [原生发布与 Relay 实验](docs/PERI-RELAY-01.md)。

### 本地测试音视频

需要 Node.js 24+、支持 libvpx/libopus 的 FFmpeg，以及支持音视频 WebCodecs 的 Chrome/Edge。

```powershell
pnpm install --frozen-lockfile --ignore-scripts
pnpm run video
```

也可使用 `npm ci --ignore-scripts` 和 `npm run video`。打开
[本地播放器](http://127.0.0.1:8080)，点击“连接”。视频为循环播放的 640×360、30 fps
VP8 测试图和 48 kHz 单声道 Opus 测试音，使用 LOC 和 MSF catalog 封装。
点击“连接”后开始播放 880 Hz 测试音，可使用“静音”关闭声音。
视频按音频播放时钟显示；“恢复画面”会等待下一个关键帧恢复解码。
播放器限制音频排队和视频帧队列，过期视频帧会被丢弃。
连接中断或媒体连续 3 秒没有数据时，播放器会重新连接、读取媒体描述并等待关键帧恢复。
重试使用 0.5 秒起步、最多 5 秒的退避间隔，连续失败最多重试 8 次。
点击“停止”会取消播放和重连；重新连接后静音设置保持不变。

可通过 `PERI_VIDEO_PORT` 指定端口，通过 `PERI_FFMPEG` 指定 FFmpeg 可执行文件路径。
服务仅绑定本机，使用 localhost 安全上下文和 WebSocket 兼容传输。
普通局域网 HTTP 地址和实时采集尚未包含。

本地 WebTransport/QUIC 视频模式还需要 Rust 工具链：

```powershell
pnpm run video:quic
```

打开相同的本地播放器。该模式启动 Rust HTTP/3 WebTransport 网关，浏览器媒体通过
QUIC 接收；使用短期证书和指纹校验，不需要手动安装证书。当前仍仅限本机，
公网 HTTPS 网页及域名证书部署尚未包含。默认 `pnpm run video` 保留 WebSocket 兼容模式。

安装 Chrome 后可运行 `pnpm run video:check` 检查音视频播放、中途加入、静音和关键帧恢复。
`pnpm run video:quic:check` 会检查 WebTransport 模式及错误证书、错误令牌的拒绝行为。

### 早期 RTMP 接入实验（非协议主线）

接收支持 **RTMP + H.264 + AAC-LC** 的推流软件（例如 OBS、FFmpeg），浏览器通过
WebTransport 接收。接入网关仅改变封装，不重新编码视频和音频；保留输入分辨率。
需要带 RTMP/FLV 支持的 FFmpeg、Rust 和支持 H.264/AAC WebCodecs 的 Chrome/Edge。

```powershell
pnpm run ingest
# 或 npm run ingest
```

启动后等待推流软件连接。填写：

| 参数         | 值                                  |
| ------------ | ----------------------------------- |
| 服务器地址   | `rtmp://127.0.0.1:1935/live`        |
| 推流密钥     | `stream`                            |
| 完整推流 URL | `rtmp://127.0.0.1:1935/live/stream` |
| 视频         | H.264，关闭 B 帧，关键帧间隔 1 秒   |
| 音频         | AAC-LC，单声道或立体声              |

收到音视频和第一个关键帧后，终端会输出观看地址。打开
[本地播放器](http://127.0.0.1:8080)，点击“连接”；支持中途加入及关键帧恢复。
如使用 OBS，可在“设置 → 直播”选择自定义服务，填写服务器地址和推流密钥，
在编码器设置中将 B 帧设为 0。其他推流工具使用相同地址与编码配置。

`PERI_RTMP_PORT` 可修改接入端口；`PERI_VIDEO_PORT` 可修改网页端口；
`PERI_FFMPEG` 可指定 FFmpeg 路径。`pnpm run ingest -- --ws` 或
`npm run ingest -- --ws` 选择 WebSocket 兼容传输。
`pnpm run ingest:check` 可使用 FFmpeg 推流验证样本保真、浏览器音视频播放和中途加入。

当前是单发布端、本机开发入口，RTMP 推流密钥不提供鉴权，推流结束或编码配置改变后需重启服务。
尚不支持公网部署、远程普通 HTTP 视频播放、SRT/RTSP 接入或 P2P 转发。
兼容范围按接入协议和编码组合确定，并非所有软件的全部输出格式均可直接使用。

### 对象传输与会话协商

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

Demo 自动启动随机端口 Relay、建立发布/订阅连接，通过实验对象传输发送 PERI hello，
在接收端执行能力选择，再发送 64 KiB 随机对象并核对 SHA-256。完成后自动关闭。
成功输出包含 `websocket`、`sentBytes: 65536`、`receivedBytes: 65536`。
`demo` 保留单向 hello 与对象完整性诊断；hello 的结果在接收端本地计算。

`handshake` 演示通过两个实验控制轨道交换 offer、accept、ack。响应端选择配置，发起端
重新验证配置并发送 ACK，响应端收到 ACK 后进入 ACTIVE。成功输出两端均为 `ACTIVE`。
失败的协商不会返回活动会话；连接关闭后，其协商结果也会失效。

## 分进程发布和订阅对象

Relay 地址使用程序默认的本机端点；所有终端配置相同的凭据环境变量。以下值仅为本地演示，应替换成随机值：

```powershell
$env:PERI_PUBLISH_TOKEN = 'local-publisher-example-123456'
$env:PERI_SUBSCRIBE_TOKEN = 'local-subscriber-example-654321'
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
| `src/peer.ts`      | 实验控制轨道上的 offer/accept/ack 交换                  |
| `src/scheduler.ts` | 本地单调时间 deadline、依赖就绪检查、有界队列和恢复建议 |
| `src/transport.ts` | 实验版本严格选择、对象发布/订阅、连接与收取超时         |
| `src/relay.ts`     | 本机 QMux/WebSocket 参考 Relay、角色凭据、广播路由      |
| `src/browser.ts`   | 浏览器可打包 SDK 入口；支持 WebTransport 与 WebSocket   |
| `src/cli.ts`       | 独立发布/订阅探针                                       |

## Rust 核心

Rust workspace 默认构建共用类型、会话协商、媒体调度和原生 PERI 协议。MoQ 对照工具单独位于 `tools/moq-baseline`，不属于正式协议模块。需要 Rust 1.91+；
Windows MSVC 目标需安装 Visual Studio 的“使用 C++ 的桌面开发”和 Windows SDK。

```powershell
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test
pnpm native:boundary:check
```

显式运行 MoQT-22 对照工具的原生 QUIC 对象检查：

```powershell
cargo test -p peri-moq-baseline --test native_quic
```

历史 PERI 控制消息通过 MoQ 承载的实验（不是独立 PERI 原生主链路）：

```powershell
cargo test -p peri-moq-baseline --test native_peer
```

该测试使用本机 UDP、TLS 证书校验和固定的实验 ALPN，比较发送与接收的对象内容。
它不包含 Relay 转发、音视频播放或独立协议实现之间的互通验证。

控制消息格式互通示例（需要 Cargo）：

```powershell
pnpm run interop
```

该命令通过本地进程管道连接 TypeScript 发起端与 Rust 响应端，交换 offer、accept、ack。
它用于检查控制消息格式，不包含 QUIC 网络传输。

## HTTP / HTTPS 设计

网页部署需区分浏览器能力：HTTPS 浏览器主线使用 WebTransport + WebCodecs；
普通远程 HTTP 页面无法直接获得同等安全上下文能力，兼容播放方案尚待评估。
当前本机 HTTP 演示利用 localhost 安全上下文，不代表已实现远程普通 HTTP 视频播放。
WSS 可以由 TLS 网关终止，但基础 Relay 默认拒绝浏览器 Origin；媒体演示仅允许其本机网页 Origin，尚不可公网部署。

## 原生 GOP 视频播放

带依赖帧的 AV1 文件可通过独立 PERI Relay 发布、原生解码观看与录制。
证书和凭据配置见[原生服务说明](docs/PERI-SERVICE-01.md)，
编码配置与观看命令见[播放说明](docs/PERI-PLAYBACK-01.md)。

```powershell
pnpm run native:gop:check
```

实时 AV1 合成测试画面发布与关键帧响应见[实时编码测试源](docs/PERI-LIVE-01.md)。

原生单观看端接收报告、恢复测量和可选码率实验见[反馈调节说明](docs/PERI-ADAPTATION-01.md)。默认使用固定码率。

减少码率切换重建的可选常驻编码方案与资源成本见[切换实验](docs/PERI-SWITCH-02.md)。

当前实现边界见[模块表](docs/PERI-MODULES.md)，外部应用接入见[Rust 指南](docs/PERI-INTEGRATION-01.md)。亚秒目标与当前测量口径见[延迟说明](docs/PERI-LATENCY-01.md)。
