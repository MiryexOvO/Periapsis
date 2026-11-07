# Periapsis / PERI

**基于 QUIC 的实时音视频传输项目，连接 OBS 推流与浏览器观看。**

PERI 提供原生发布、Relay 分发和浏览器观看网关。主播使用 Windows OBS 插件发布 AV1 + Opus；观众通过网页观看原画或共享 720p 转码。项目同时提供 Rust 媒体接口，供其他采集、编码和播放应用接入。

当前阶段：**核心链路已打通，进入产品化验证。** 本轮弱网极限优化已收口，后续优先完善长时间运行、音画同步、断线恢复和安装升级。尚未发布稳定 SDK，也未完成公网生产验收。

## 工作方式

```text
OBS + PERI 插件
      │ AV1 + Opus / 原生 PERI over QUIC
      ▼
    Relay
      │ 原生 PERI 订阅
      ▼
  浏览器观看网关
      ├── 原画：AV1 + Opus
      └── 共享转码：最高 720p60 / H.264 + AAC
                  │
                  ▼
       WebTransport + WebCodecs
       或 HTTP 兼容播放
```

原画链路无需服务器转码；共享 720p 档位按需启动 FFmpeg，同一资源的观看者共用转码结果。浏览器侧提供 WebM / HLS 兼容路径，具体路径取决于浏览器能力和部署配置。兼容播放不承诺与 WebTransport 相同的延迟。

## 当前支持

| 能力 | 当前范围 |
| --- | --- |
| OBS 推流 | Windows x64 原生插件，已在 OBS Studio 32.1.2 验证 |
| 音视频 | 原画 AV1 + Opus；共享 720p 转码 H.264 + AAC |
| 浏览器观看 | Chrome / Edge 的 WebTransport + WebCodecs 路径；WebM / HLS 兼容播放，iPhone / iPad 默认 HLS |
| 发布与分发 | QUIC/TLS、发布/订阅独立凭据、单资源 Relay |
| 恢复与资源约束 | 视频依赖检查、关键帧恢复反馈、有界队列与对象预算 |
| 主动编码恢复 | 已验证 OBS 32.1.2 + NVENC AV1 + CBR；其他组合依赖周期关键帧 |
| 应用接入 | Rust Publisher / Subscriber；OBS 专用 C ABI |

PERI 原生协议使用实验 ALPN `peri-dev-02`。浏览器通过网关接入，不直接使用该 ALPN。当前没有与 MoQ / WebRTC 的标准互通或全面性能领先结论。

## 开始使用

### 主播：安装 OBS 插件

使用配套的 `PERI-OBS-Windows-x64.zip`，安装步骤见相邻插件仓库的 [OBS 安装说明](../obs-periapsis/README.md)。若只取得本仓库，需要另行取得插件包或配套插件源码。

1. 完全退出 OBS，解压插件包并运行 `Install-PERI.cmd`。升级时成套更新两个 DLL。
2. 向服务器管理员取得发布 Token。
3. 打开 OBS「设置 → 直播」，选择 **PERI**，将完整 Token 粘贴到「推流码」。
4. 选择 AV1 编码及所需分辨率、帧率，点击「开始直播」。

主播端只需要 OBS、可用编码器和插件包，无需安装 Rust、Node.js 或 FFmpeg。发布 Token 含发布凭据，应私下传递。

### 管理员：构建与启动服务

源码构建需要 Rust 1.91+；Windows 还需 MSVC C++ 工具与 Windows SDK。提供共享 720p 档位时，服务器需安装支持 AV1 解码、libx264 和 AAC 编码的 FFmpeg，可通过 `PERI_FFMPEG` 指定路径。

在仓库根目录构建两个服务程序：

```sh
cargo build --release --locked -p peri-native --bin peri -p peri-webtransport --bin peri-webtransport
```

以下以 Linux 为例。首次部署运行交互初始化，填写主播可达地址、端口和证书配置：

```sh
./target/release/peri-webtransport setup
```

保持两个二进制在同一目录，并在两个终端分别运行：

```sh
# 终端 A：发布接入与分发
./target/release/peri-webtransport relay
```

```sh
# 终端 B：观看网关
./target/release/peri-webtransport watch
```

生成交给主播的 Token（将示例地址替换为主播可达的实际 IPv4）：

```sh
./target/release/peri-webtransport obs-token --public-ip 192.0.2.10
```

Windows 对应程序位于 `target\release\`，文件名带 `.exe`。默认配置位于用户目录下的 `.config/peri/server.json`，可用 `PERI_CONFIG` 指定配置文件。已有部署沿用配置，无需重复初始化。

**低延迟网页观看还需要配置 WebTransport。** 公网部署应使用 HTTPS 观看页、受信任的 TLS 证书和独立可达的 HTTP/3 UDP 端口，并设置匹配的页面 Origin。只启动上述服务不会自动完成这些配置；普通公网 HTTP 页面走兼容路径。

完整步骤、环境变量和 systemd 部署见 [音视频部署与观看指南](docs/PERI-PROGRAM-AV.md)。

### 观众：打开观看页

打开管理员提供的观看地址，选择原画或 720p，并点击「开启声音」。状态栏可确认当前是否使用 WebTransport / WebCodecs。没有声音时先检查是否已完成这一用户交互；无法使用低延迟路径时，按部署指南检查证书、UDP 可达性与 Origin 配置。

## 验证结果与使用边界

截至 2026-11-07，最近一次本机、双浏览器、无注入网络损伤的回归结果为：

| 观看档位 | 实际绘制帧率 | 测得冻结 | 像素时钟延迟 P95 |
| --- | --- | --- | --- |
| 1440p60 原画 | 约 60 FPS | 0 | 328–337 ms |
| 共享 720p60 | 约 60 FPS | 0 | 482–487 ms |

这是一轮短时合成测试，证明限定环境下的播放链路可运行，不是公网延迟、画质或长时间稳定性保证。测量口径见 [延迟说明](docs/PERI-LATENCY-01.md)。

复合丢包和断流测试曾出现通过结果，但重复测试未稳定通过冻结门槛。当前不承诺任意弱网下的 1440p60 原画连续播放，也不把固定低分辨率档位视为已经实现自动降级。

实验性上行预算、编码器调整与关键帧接纳策略保持默认关闭。日常使用无需设置实验环境变量；已有实验环境应移除对应变量，按变量存在启用的开关不能通过设置字符串 `0` 关闭。实验记录、失败结果与后续研究保存在 `specs/`，不作为默认使用步骤。

当前仍需完成的交付验证包括：长时间直播的资源稳定性、音画同步、断线重连与再次开播，以及安装、升级和卸载。直播中自动分辨率切换、P2P、MPQUIC、账号体系和大规模分发不属于当前交付范围。

## 开发与第三方接入

采集、编码和呈现由应用负责；PERI 提供会话、媒体对象、分发与恢复反馈。Rust 应用可直接接入 Publisher / Subscriber，使用自己的编解码器和渲染器。

| 组件 | 职责 |
| --- | --- |
| `peri-types` | 共用类型与能力配置 |
| `peri-session` | 会话协商逻辑 |
| `peri-scheduler` | 通用调度原型 |
| `peri-native` | 原生传输、媒体接口、Relay 与命令行工具 |
| `peri-obs` | OBS 发布接入的 Rust C ABI 库 |
| `peri-webtransport` | 浏览器观看网关与内嵌播放器 |

工作区包含以上六个 crate，默认成员为前四个。当前通过本地路径或固定仓库版本集成，尚未发布到 crates.io，没有稳定 API/ABI 兼容承诺。OBS 专用 C ABI 不等同于通用 C++ SDK。

```sh
# 默认核心成员的 Rust 测试
cargo test --locked

# 浏览器、验收与诊断脚本测试；需要 Node.js 24+ 和 pnpm
pnpm install --frozen-lockfile
pnpm test
pnpm run check
```

真实 OBS、编码器和浏览器链路需要另行运行集成验证，单元测试通过不能替代实际播放验收。

- [Rust 接入指南](docs/PERI-INTEGRATION-01.md)
- [模块与接入边界](docs/PERI-MODULES.md)
- [实验线协议](docs/PERI-WIRE-02.md)与[媒体配置](docs/PERI-MEDIA-01.md)
- [原生命令行服务](docs/PERI-SERVICE-01.md)与[文件发布、播放工具](docs/PERI-PLAYBACK-01.md)

开发脚本、测试源和历史实验均不属于对外稳定 SDK。
