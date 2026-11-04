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
