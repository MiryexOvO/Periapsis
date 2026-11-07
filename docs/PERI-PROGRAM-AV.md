# PERI 原生发布与观看

主播安装 PERI Windows x64 插件。OBS C++ 输出模块将编码包写入有界环形缓冲，独立线程通过 C ABI 调用同目录的 Rust 动态库 `peri_obs.dll`，直接以 QUIC 发布。主播端只需 OBS 和插件包。服务器端部署 Rust Relay 与 Rust 观看网关。

OBS 32.1.2、NVENC AV1 (`obs_nvenc_av1_tex`) 的 CBR 发布支持主动关键帧恢复：独立反馈线程将已鉴权的下游恢复请求传到本次直播专用编码器，保留码率设置、合并请求并限频。其他编码器/版本/码率模式仍依赖周期关键帧。该接入没有改变网关每轨可靠流的传输方式，也不代表公网弱网验收已经完成。

Active keyframe recovery is supported for OBS 32.1.2 NVENC AV1 CBR publishing. An independent feedback worker forwards authenticated recovery demand to a dedicated streaming encoder, preserving bitrate settings and pacing requests. Other configurations retain periodic recovery; browser stream delivery and public-network validation remain separate.

## 更新已有服务器

在 PERI 仓库执行：

```sh
cargo build --release --locked -p peri-native --bin peri -p peri-webtransport --bin peri-webtransport
./target/release/peri-webtransport configure
./target/release/peri-webtransport link
```

网关沿用 `~/.config/peri/server.json`（Linux 权限 0600），也可通过 `PERI_CONFIG` 指定文件。已有部署保留原证书与凭据，缺少旧版本 `stream_key` 字段时使用已有发布凭据。`configure` 交互修改主播连接的 IP 和外部 UDP 端口；地址不变时可直接运行 `link`。

把 `link` 输出的两项私下交给主播：OBS「设置 → 直播」选择 PERI，服务器栏直接输入 `peri://IP:端口`，推流码栏粘贴 `peri1_` 开头的代码。插件自动校验服务器身份，主播不需要处理证书文件。IP/端口变更不改变推流码。首次部署运行 `setup`，证书和随机凭据自动生成。`stream-key` 会生成新的随机推流码，执行后需重启 Relay 并更新主播配置；`watch-port` 管理观看端口。旧 `obs-token` 命令与完整 Token 仍兼容。

直连服务器与端口映射服务器的主播输入方式相同，均使用主播可达的 `peri://IP:端口`。`configure` 保留本机监听端口，外部端口可以与之不同。IPv6 使用 `peri://[IPv6地址]:端口`，当前不支持域名。观看网页和浏览器 WebTransport 的端口、证书另行配置。

Relay 与网关二进制放在同一目录，分别从两个终端启动：

```sh
./target/release/peri-webtransport relay
./target/release/peri-webtransport watch
```

迁移时先停止原 watch 进程以释放 TCP 端口；运行中的 Relay 可以继续使用。观看页、WebCodecs 播放器和 hls.js 已嵌入 Rust 二进制。原画发布、订阅、分发和解码链路均可在没有 FFmpeg 的环境运行。需要提供 720p60 时，服务器另需带 AV1 解码、libx264 和 AAC 编码的本机 FFmpeg，可用 `PERI_FFMPEG` 指定路径。转码进程按需启动，两种观看路径共用同一次编码。

### HTTPS 与浏览器连接

在 HTTPS 反向代理后运行网关的 HTTP 观看端口。独立开放或转发一个公网 UDP 端口给 HTTP/3。示例中观看页为 https://live.example.com，网关 UDP 为 4450：

```sh
export PERI_WT_BIND=0.0.0.0:4450
export PERI_WT_URL=https://live.example.com:4450
export PERI_WT_CERT=/path/to/fullchain.pem
export PERI_WT_KEY=/path/to/privkey.pem
export PERI_PAGE_ORIGIN=https://live.example.com
./target/release/peri-webtransport watch
```

`PERI_PAGE_ORIGIN` 必须与页面 Origin 一致（非默认端口也要填写）。浏览器证书和 Relay 自签证书各有用途。受信任的浏览器证书用于公网；开发机可在 localhost 使用短期 ECDSA 证书，并通过 `PERI_WT_CERT_HASH` 提供 DER 的 SHA-256 标准 Base64。普通公网 HTTP 页面使用兼容路径。

## 协议与网关边界

网关使用 Quinn 向 Relay 做 TLS 鉴权和原生 PERI 订阅，每个资源共享一个订阅。它校验视频依赖链，用有界广播队列分发不可变帧。浏览器侧使用 web-transport-quinn 完成 HTTP/3 CONNECT，验证 Origin，并在两条独立的 QUIC 单向流中分别发送视频和音频。每轨先传 4 字节大端长度与轨道 JSON；每帧为 4 字节长度、1 字节关键标志、8 字节 PTS（微秒）、4 字节时长（微秒）和编码负载。浏览器展示协议与原生 PERI ALPN 各自演进。

Chrome / Edge 的原画使用 AV1 + Opus，720p60 使用 H.264 + AAC，两档都直接送入 VideoDecoder 与 AudioDecoder；音频由观众点击「开启声音」后通过 Web Audio 播放。观看页状态栏显示 WebTransport / WebCodecs。客户端绘制队列、解码积压与服务器广播保留量都有上限。每轨有序可靠，公网丢包仍可能造成等待。不能建立 WebTransport 或解码失败时，原画自动回退到 Rust WebM 封装，720p60 回退到 HLS。iPhone/iPad 默认使用 HLS。720p60 转码器限制为最高 1280×720、60 fps，视频目标 3.2 Mbps；LL-HLS 使用 250 毫秒碎片。

## systemd

HTTP 原画兼容播放在支持 AV1/Opus MediaSource 的浏览器上通过流式 fetch 及时追加 WebM 数据，减少纯色过场编码量变化造成的渐进下载缓冲停顿；其他浏览器保留原生 WebM 路径。此方式不要求 HTTPS，但不等同于 WebTransport。播放器保留停滞重连兜底，服务器流响应请求代理关闭缓冲；实际代理是否遵守取决于部署配置。

安装两个发布二进制到 `/opt/peri/bin`。观看服务单元位于 `deploy/linux/peri-viewer.service`。用 `peri-webtransport viewer-config` 生成仅含公开连接参数的 JSON，保存为 `/etc/peri/viewer.json`。确保其中的 cert 指向服务用户可读的公开证书。将订阅凭据 `PERI_SUBSCRIBE_TOKEN` 单独放入 root 可读的 `/etc/peri/viewer.env`。按需在同一环境文件中配置上述 WebTransport 变量；为 DynamicUser 提供 TLS 私钥时宜通过 systemd LoadCredential 传入并设置相应读取路径。
