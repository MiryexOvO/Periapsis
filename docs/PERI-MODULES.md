# PERI 当前模块与第三方接入边界

项目已经初步分层，但尚未形成稳定发布的第三方 SDK。正式协议目录当前有四个 Rust crate；
原生协议的大部分能力仍以 `peri-native` 内部模块提供，并非每个模块都独立成包。
表中的“已有”描述当前实现，不代表标准化、生产就绪或独立实现互通。

|层/模块|代码位置|已有职责|边界与状态|
|---|---|---|---|
|共用类型|crates/peri-types|配置、缓冲策略、能力与协商结果|无网络 I/O；与原型共用|
|会话协商|crates/peri-session|能力校验、配置选择、握手状态机与旧控制编解码|原生路径复用其协商逻辑，采用自己的控制消息|
|通用调度原型|crates/peri-scheduler|依赖、过期与队列计费|独立原型；原生媒体层未依赖该 crate|
|原生会话/对象|crates/peri-native/src/lib.rs|ALPN、发布订阅、会话、独立流、取消、有界发送|公共 Rust API；底层接口耦合 Quinn|
|线格式|peri-native/src/wire.rs|有界控制消息和对象头/FIN 校验|内部模块；独立实现依据协议文档而非调用此模块|
|媒体层|peri-native/src/media.rs|Catalog、Track、Frame、Publisher、Subscriber、DecodeBuffer|公共 Rust API；调用方提供编码数据和真实解码确认|
|分发服务|peri-native/src/relay.rs|单资源接入、独立订阅队列、恢复与反馈转发|公共库；不是完整资源管理平台|
|鉴权|peri-native/src/auth.rs|发布/订阅角色凭据校验|公共 API；没有账号体系、令牌轮换服务|
|反馈与测量|peri-native/src/feedback.rs|关键帧请求、接收报告、有界邮箱与日志|协议处理内部封装；请求/报告类型和方法公开|
|媒体策略|peri-native/src/adaptation.rs|单观看端目标码率、切换调度|可选实验；默认关闭，不是固定线协议必选算法|
|AV1 文件适配|peri-native/src/av1.rs|有界 IVF 导入和录制、探测元数据检查|工具适配；不要求第三方使用 IVF|
|命令与示例|peri-native/src/bin/peri*、examples|证书初始化、Relay、文件/实时发布、观看录制|应用层，非 SDK 协议要求|
|共享播放工具|peri-native/src/playback.rs|AV1解码、单调时钟、有界GOP调度|公共可选工具；接入方可使用自有解码与呈现|
|实时测试源|scripts/live-av1-worker.py|PyAV AV1合成编码源|内部验证工具，不属于SDK要求|

## 给外部开发者保留的路径

外部采集/编码器 → 公共 Rust Publisher → PERI 会话/对象 → Relay 或原生接收端。
接收端也可使用公共 Subscriber，将编码数据交给自己的解码器和渲染器。
协议不要求接入方使用测试画面、Python、FFmpeg、我们的播放器或码率策略。

Rust 库当前可以通过本地 path 或固定 Git 提交集成；所有 crate 均为 0.1.0、publish=false，
尚未发布到 crates.io，也没有稳定 SDK/API 兼容承诺。
身份、证书信任及 QUIC Endpoint 配置由接入应用负责；协议库提供鉴权会话和媒体 API。

其他语言可按公开实验线格式开发独立实现，或等待后续 C ABI；目前没有 C ABI/FFI、
C++ SDK、OBS 插件或独立实现互通证明。现有 Rust/TypeScript 互通实验不等于新原生线协议跨语言互通。

## 尚未完成的模块边界

- 独立传输适配接口和易用 Endpoint/Client Builder，避免接入方直接处理全部 Quinn/TLS 配置。
- 核心媒体协议、策略、服务和工具的 crate/feature 进一步拆分；当前 module 不等于可单独安装的软件包。
- 通用调度原型与原生 DecodeBuffer 的职责统一，避免两套调度逻辑演进分叉。
- 可协商的扩展执行机制、版本演进规则与一致性测试向量。当前不能把扩展能力字段视为已实现插件系统。
- C ABI、SDK 发布、真实软件插件、原生音频播放同步与新线协议浏览器接入。
- MPQUIC/P2P 当前为研发方向，没有已实现的可启用模块。

第三方 Rust 接入步骤见[接入指南](PERI-INTEGRATION-01.md)，线格式见[协议草案](PERI-WIRE-02.md)。

## 依赖边界

正式Cargo工作区只包含上述四个crate，默认构建与全工作区构建均不包含内部对照实现。
内部研发工具独立管理依赖和锁文件，不作为第三方接入模块或对外SDK能力。
协商状态机接收中性的传输就绪通知；具体传输认证、ALPN和版本检查由连接层负责。
