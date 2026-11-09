<!-- SPDX-License-Identifier: Apache-2.0 -->
# PERI 1.0.0

首个正式协议版本。协议稳定范围见 [PERI 1.0](PERI-1.0.md)。本文件为发布说明，公开发布时间由最终发布操作确定。

## 已交付能力

- QUIC/TLS 原生发布、单资源 Relay、独立发布/订阅凭据、媒体依赖检查与恢复反馈。
- Windows x64 OBS 插件；服务器自由输入 `peri://IP:端口`，推流码独立填写，自动校验证书指纹。
- 服务器自动生成证书和随机凭据；setup、configure、link、stream-key 管理接入信息。
- 原画 AV1 + Opus；共享 720p H.264 + AAC 转码；WebTransport/WebCodecs 与 HTTP WebM/HLS 观看。
- HTTP 原画以 MSE 推送字节改善黑白过场的停顿，播放停滞时重连；OBS 地址编辑、删除和粘贴修复。
- 服务程序支持 `--version`。安装包带版本清单和 SHA-256，源码和构建工具同步交付。

## 支持边界

OBS 已验证 Windows x64、OBS Studio 32.1.2、Qt 6.8.3；主动关键帧恢复已验证 NVENC AV1 + CBR。其他 OBS/编码器组合未获得同等验证。Linux 服务端可从锁定依赖构建；是否提供该平台二进制以对应构建记录为准。Windows 构建成功不代表 Linux 已验证。

稳定范围是单资源直播链路；无多租户、P2P、多路径、任意弱网原画保证或相对 MoQ/WebRTC 全面领先承诺。浏览器能力决定播放路径；HTTP 兼容路径不承诺 WebTransport 延迟。可选 FFmpeg 由管理员安装，发布包不捆绑 FFmpeg 或 OBS/Qt 运行时。

## 从实验版升级

1. 停止直播、watch 和 relay；备份旧二进制、整个配置目录及配置引用的证书/密钥。默认配置为 `~/.config/peri/server.json`；设置了 PERI_CONFIG 时以实际位置为准。备份含凭据，应限制访问。
2. 服务端成套替换 peri 和 peri-webtransport。运行两者 `--version`，均应为 1.0.0。
3. 退出 OBS，安装配套插件，成套替换 peri-output.dll 与 peri_obs.dll。重启 OBS。
4. 保留已有配置。不要为升级而运行 stream-key 或删除配置。需要修改连接地址时使用 configure；link 显示交给主播的信息。旧完整 Token 保留兼容入口。
5. 启动 relay 和 watch，开启一次短直播，确认原画音视频、黑白过场、停止后重新开播和网页重连。用户已完成的测试不必无限重复；记录所用版本及环境即可。

近期实验版与本版使用相同 ALPN/线前导，但不保证所有历史构建都兼容；建议三端同步升级。NAT 仅需填写映射后的可达端口，不需要专用协议模式。

## 回滚

停止服务，恢复先前成套二进制和 OBS 两个 DLL，再恢复备份配置与其引用证书（仅当升级过程中修改过配置）。重启 relay/watch。未轮换凭据或证书时无需重新发码；已经轮换则使用与恢复配置相匹配的旧凭据。不要覆盖正在运行的 DLL 或混用两个版本。

## 故障反馈

提供两端版本、OS/OBS/浏览器版本、播放状态栏路径、复现时间和脱敏日志。不要公开推流码、完整 Token、私钥、server.json 或令牌文件。无法连接先核对目标 IP/UDP 端口与监听，黑白过场问题记录 HTTP/MSE 或 WebTransport 路径及音视频是否同时停住。
