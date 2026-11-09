# Changelog

## 1.0.0

采用分层授权：OBS 插件和客户端 SDK 为 BSD-3-Clause，协议文档为 Apache-2.0，Relay 与观看网关为 AGPL-3.0-only。Relay 独立为 peri-relay crate，客户端正常依赖链不包含 AGPL 服务端；第三方许可证保持不变。

首个正式协议发布基线。冻结现有 QUIC 线封装、握手、媒体对象与恢复反馈；保留历史 ALPN 以避免人为断连。统一版本、增加 CLI 版本查询、发布规范、升级回滚说明与可校验的安装包。

包含两字段 OBS 接入、自动证书指纹验证、可编辑服务器地址和 HTTP 原画黑白过场播放修复。支持边界及升级步骤见 [发布说明](docs/RELEASE-1.0.0.md)。
