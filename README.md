# Periapsis / PERI

基于 QUIC 的实验性实时媒体传输协议，面向低延迟推流、实时互动与原生客户端。
采集和编码由接入应用提供，协议负责会话、媒体对象、分发和恢复反馈。

## 当前能力

- 独立实验 ALPN `peri-dev-02`，原生发布、订阅和单资源 Relay。
- TLS 证书校验、独立发布/订阅凭据、能力协商和有界对象传输。
- 媒体目录、帧依赖、实际解码确认、截止时间和关键帧恢复反馈。
- AV1 文件与实时测试源、原生观看和录制工具。

项目仍处于研发阶段，尚无稳定 SDK/API 兼容承诺。
Windows OBS 原生插件通过 C ABI 调用 Rust 发布库。Rust 网关向浏览器提供 WebTransport 和 WebCodecs 播放。
AV1 + Opus 双轨开发入口和浏览器观看步骤见[节目音视频接入](docs/PERI-PROGRAM-AV.md)。
网关支持原画与共享 720p60，HTTP 兼容观看使用 WebM 或 LL-HLS。
亚秒是验收目标，不保证任意网络下达标；测量范围见[延迟说明](docs/PERI-LATENCY-01.md)。

## 构建与运行

需要 Rust 1.91+；Windows MSVC 目标需要 Visual Studio C++ 工具和 Windows SDK。

```powershell
cargo build --locked -p peri-native --bin peri
cargo test --locked
.\target\debug\peri.exe --help
```

Linux/macOS 使用 `./target/debug/peri`。
程序提供 init、relay、publish、live、receive、play 命令。
原生观看工具需要支持 AV1 的 FFmpeg/FFplay；自有应用可使用其他解码器和渲染器。

证书、凭据及分进程使用方法见[服务说明](docs/PERI-SERVICE-01.md)，
文件发布和观看见[播放说明](docs/PERI-PLAYBACK-01.md)。

## 第三方接入

外部采集/编码器 → 公共 Rust Publisher → PERI 会话/对象 → Relay → Subscriber → 应用解码器与渲染器。

正式 Rust 工作区包含 peri-types、peri-session、peri-scheduler、peri-native 四个 crate。
当前使用本地路径或固定仓库版本接入，均未发布到包注册表。
应用负责证书信任、身份生命周期、采集、编码及呈现；协议不要求采用测试源或内置播放器。

- [模块表](docs/PERI-MODULES.md)
- [Rust 接入指南](docs/PERI-INTEGRATION-01.md)
- [实验线协议](docs/PERI-WIRE-02.md)
- [媒体配置](docs/PERI-MEDIA-01.md)

仓库中的开发脚本和历史实验不是对外 SDK 或正式接入入口。
研发弱网验收可使用[累计卡顿诊断](docs/PERI-FREEZE-01.md)关联接收、解码和Relay日志。
全轨道恢复时的可选接收策略见[四路恢复接收候选](docs/PERI-RECEIVE-01.md)。
发送端完成对象的候选调度策略见[按对象集中发送实验](docs/PERI-SEND-01.md)。
底层确认频率与可观测统计见[频繁传输确认候选](docs/PERI-ACK-01.md)。
可选对象修复的内核、媒体适配器与接入边界见[有界修复配置](docs/PERI-REPAIR-01.md)。
