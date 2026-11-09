# PERI 分层授权

自当前 1.0.0 发布工作树起，按文件所属组件确定许可证。本文件的明确例外优先于根目录 LICENSE 的默认 AGPL-3.0-only。第三方代码始终保留其自己的许可与署名；这不是整仓库可以任选三种许可证的三重授权。

| 范围 | 许可证 | 边界 |
| --- | --- | --- |
| crates/peri-types、peri-session、peri-scheduler、peri-native | BSD-3-Clause | 客户端类型、协商、调度、QUIC 传输和媒体 SDK；包含各自测试与示例 |
| crates/peri-obs | BSD-3-Clause | OBS 客户端 C ABI 动态库 |
| 相邻 obs-periapsis 仓库的第一方代码与文档 | BSD-3-Clause | OBS 插件、构建和测试；以该仓库 LICENSE 为准 |
| crates/peri-relay | AGPL-3.0-only | Relay 实现、服务端 peri 命令及服务器集成测试 |
| crates/peri-webtransport | AGPL-3.0-only | 观看网关及其内置网页，不作为 BSD 客户端 SDK 发布 |
| docs/、根 README.md、CHANGELOG.md、LICENSING.md、LICENSE-HISTORY.md | Apache-2.0 | 协议规范、架构、部署和发布文档 |
| 其他第一方文件 | AGPL-3.0-only | 根目录开发工具、部署脚本等；更具体的文件或目录声明优先 |

完整条款在 LICENSES/ 中；各 Rust 组件的 Cargo.toml 和 LICENSE 与上表一致。规范文档采用 Apache-2.0 不会把 Relay 实现改为 Apache，也不代表标准认证或额外专利池授权。

## 依赖方向

`OBS / peri-obs → peri-native → peri-session / peri-types` 是 BSD 客户端路径。`peri-relay → peri-native` 是 AGPL 服务端使用 BSD 公共库的方向。SDK 与 OBS 的正常构建依赖不得反向包含 peri-relay 或 peri-webtransport；`scripts/license-boundary-check.mjs` 检查 Cargo 实际依赖图。

SDK 不再导出 `peri_native::relay`。服务端使用 `peri_relay::{Relay, Limits}`，构建命令改为 `cargo build --release --locked -p peri-relay --bin peri`。线上 ALPN、握手和媒体封装没有改变。BSD 授权不等于 Rust SDK API 已冻结。

## 分发与历史

服务端包保留 AGPL 及依赖声明，OBS 包使用 BSD 作为第一方主许可证，规范文档附 Apache 文本。源码包按目录分别授权。OBS/Qt/FFmpeg 不随包捆绑，也不由本项目重新授权；第三方依赖的原许可证继续适用。第一方 BSD 声明不替代与 OBS/Qt 组合分发时需遵守的第三方许可要求。

LICENSE-HISTORY.md 记录此前全仓 AGPL 历史再发布操作，不是当前所有组件的统一许可。旧版本已经授出的权利不因新政策或历史重写而撤回。本次分层政策不再次改写已推送的历史。
