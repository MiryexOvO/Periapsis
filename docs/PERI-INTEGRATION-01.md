# 第三方 Rust 发布/接收接入

当前公共 API 为实验接口，可以集成，尚未稳定发布。已有模块和缺口见[模块表](PERI-MODULES.md)。

## 依赖

在应用的 Cargo.toml 中使用整个仓库的本地副本，保留 workspace 目录结构：

```toml
[dependencies]
peri-native = { path = "../periapsis/crates/peri-native" }
peri-types = { path = "../periapsis/crates/peri-types" }
```

也可使用部署方提供的仓库 URL 和固定 rev；当前 publish=false，不能假设注册表中有这些包。
Rust 版本和工具链要求沿用仓库根 Cargo.toml。编码数据由应用提供，Rust 库不依赖 Python 或 FFmpeg 运行。
但当前单个 crate 仍包含工具适配及证书生成相关依赖，尚无精细 feature 拆分。

## 发布方

1. 用 Quinn 创建 Endpoint，配置可信服务器证书、服务器名称及 `peri_native::ALPN`，保持 TLS 校验。
2. 将独占连接交给 `publish_authenticated`，提供 Hello、资源和发布角色凭据。
3. 构造不可变 Catalog/Track，调用 `media::Publisher::new`；先完成目录发送再提交媒体帧。
4. 使用应用自己的采集和编码器构造 Frame，通过 `Publisher::send` 发送。Frame 序号须全资源唯一，
   独立单元依赖为空，参考单元声明同轨道更早的依赖，时间戳是媒体时间，不是跨端墙钟。
5. 轮询 `poll_recovery` 将请求交给自己的编码器；按应用能力选择是否使用 `poll_report`，
   协议不要求采用测试源的码率算法或四档编码池。
可选用 `adaptation::RecoveryPacer` 合并恢复需求，按单调时钟限频；只在实际编码出独立帧后确认。
该工具不驱动编码器，接入方可使用自己的恢复策略。

6. 保留会话和 Endpoint 生命周期，停止时清理采集、编码和连接；对象传输确认不等于接收方解码。

crate 首页的 `publish_encoded` no_run 示例由 Rust 文档测试编译，验证只通过公共 API 接入。
它接受调用方提供的 Session、Catalog 和 Frame；不是端点配置或真实采集的完整应用。
可运行原生服务和 Relay 示例提供 TLS、鉴权和媒体传递参考；实验中的自签证书仅用于明确配置的测试信任。

## 接收方

`subscribe_authenticated` → `Subscriber::new` → `recv` 或实时 `recv_live` → 应用解码器。
如果采用 DecodeBuffer，只有实际解码成功后才调用 `decoded`；对象缺口或解码失败须处理参考链恢复。
`recv_live` 的 None 表示对象放弃，不等于连接断开；其他协议错误仍应失败退出。
晚加入视频时，建议选定轨道后主动调用 `request_keyframe`，等待独立帧时继续通过控制层限频重试；
不要依赖发布方自然 GOP 恰好到来。原生播放器已实现这一流程，应用无需采用我们的编码器。
应用可使用 `send_report`，并自行实现播放时钟和有界输出缓冲。

## 互通边界

当前原生可观看样例验证 AV1，不能据协商字符串推断任意编解码器已经完成封装与播放验证。
独立媒体 Catalog 支持描述音视频轨道，但原生音频同步播放尚未完成。
不能直接把早期网页演示的连接地址作为新原生协议浏览器入口。
实验 ALPN、控制字段及媒体语义仍可能变化，应固定版本，在升级时运行协议一致性与应用回归。

## 等待独立帧的有界接收

`Session::recv_recovery` 与 `media::Subscriber::recv_recovery` 在恢复期间按头部跳过所有依赖对象，
返回None并STOP(1)，不分配或等待其负载。保留的独立对象仍需完整负载、FIN和媒体校验。
因此None也包含主动跳过；头部非法仍报错，主动丢弃的负载不会做完整校验。
每次调用只处理一个对象，idle和residence上限沿用recv_live；应用保留总无进展超时。
仅在所有依赖轨道都允许被丢弃时使用。音视频混合应用若只恢复某条轨道，应使用recv_live并自行处理，
不能把这个全会话过滤接口当作逐轨道选择器。原生单视频播放器仅在等待关键帧时调用它。

恢复接收的资源边界：一次只接收一个流；依赖对象校验25字节头部后直接STOP(1)，不分配其负载。
独立对象分配仍受1MiB上限限制，要求完整负载及FIN，读取受调用方的residence约束。
`None`包含主动跳过、读取超时和RESET(1)，不是连接关闭；其他协议错误仍返回错误。
该接口适合全部依赖轨道均在恢复的阶段。多轨道中仍正常播放的轨道应继续使用recv_live，避免全局跳过。
头部本身停滞仍会占用一次读取预算，接口没有承诺消除所有跨流等待或任意网络下的亚秒延迟。

## 实验传输适配边界

`Frame::to_object/from_object`供外部传输适配器复用媒体校验，Catalog仍须先按Agreement验证。
`Object::encode_packet/decode_packet`处理单个有界完整包，拒绝截断、超长和尾随数据。
这些接口不替代Session协商、接收配额、跨帧排序或解码确认。
外部传输适配应复用已校验的媒体类型，不应改变媒体负载或假定收取等于解码。

可选共享播放工具位于playback模块：Decoder需要本机FFmpeg，核心Session/Frame接入仍可使用自己的解码器。
Decoder::decode必须受调用方超时保护，取消或失败后停止并重建，不能复用半帧管道。
GopScheduler::push返回ResetDecoder时必须取消旧任务；decoded只在实际输出完整像素后调用。
多轨道音视频同步、网络接收并发及网络读前配额仍由应用负责，单视频调度器不提供这些能力。

Handshake::transport_ready()为无参数的本地就绪通知；调用方必须先验证传输身份与版本。状态名为TransportReady，不再暴露具体外部传输实现。

## 可选修复媒体通路

在双方Hello加入peri_native::repair::capability(false)，以实际Agreement决定是否启用。
Publisher/Subscriber在构造时识别共同扩展，Relay逐跳协商，不要求调用方自行拆解修复片。
原始Session对象接口保持底层语义，不自动进入该媒体配置。默认CLI关闭，实验开关PERI_REPAIR=1。
协商修复后各Subscriber收帧入口共用四槽重组器，恢复过滤改为还原后检查，
不再具有原始recv_recovery的头部跳过负载行为，也不叠加retained的有序接收算法。
组间ACK等待与原始读取1MiB上限仍存在；完整配额、计时和性能边界见PERI-REPAIR-01。

## 对象发送期限

Session::start_send的预算覆盖槽等待、写入和ObjectSend::finish的确认等待。
句柄保存原始绝对截止时间；延后调用finish不会取得新的预算，已有ACK也不能使过期调用成功。
零预算返回Timeout且不开流，无法表示的超长预算返回Protocol("deadline overflow")。
取消未完成句柄时RESET(1)并释放发送槽。ObjectSend仍可从peri_native根路径导入。
修复分片调用同一发送执行器，逐片沿用组的原始截止时间。

2026-11-06期限重构验证：工作区92项Rust测试通过（另1项外部AV1夹具按原规则跳过），严格Clippy通过。
默认关闭修复的short/fixed单轮实际播放完成60帧、零重连，报告位于dist/live-check-j9Mlay。
该运行软件延迟P95为310ms、最大313ms，但最大输出间隔2396ms，连续性亚秒条件失败。
运行用于收发链路回归，未开启严格亚秒门禁；不能据此宣称冻结减少或修复候选通过弱网验收。
