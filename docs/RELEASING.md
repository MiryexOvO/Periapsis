<!-- SPDX-License-Identifier: Apache-2.0 -->
# 正式版本制作与发布

版本入口为根目录 VERSION；Cargo 工作区和 package.json 必须一致。先完成修改、针对性检查，再构建和打包。用户实播反馈与自动检查分别记录，不能把本机结果写成所有平台承诺。

## 构建

需要 Rust 1.91+、Node.js 24+ 和系统 tar。Windows OBS 构建另需 MSVC、Windows SDK、OBS 32.1.2 libobs SDK 及匹配 Qt 6.8.3 SDK。

```sh
cargo test --locked --workspace
node --experimental-strip-types --test tests/*.test.ts tests/*.test.mjs
cargo build --release --locked -p peri-relay --bin peri -p peri-webtransport --bin peri-webtransport -p peri-obs
cargo fetch --locked
node scripts/package-release.mjs
```

Node 打包脚本默认只制作本机平台服务端包与源码包。它拒绝不匹配的二进制版本，收集 Cargo 依赖许可证和工作树源文件 SHA-256，不包含本地配置、证书、specs 或实验产物。输出为 `dist/releases/1.0.0/<唯一构建目录>/`。校验使用同目录 `SHA256SUMS`。校验和不等于发布者数字签名，应从可信发布页面取得。

OBS 仓库与本仓库并排放置。在 MSVC 开发者 PowerShell 中运行插件构建后添加 `--plugin`：

```powershell
../obs-periapsis/native/build.ps1 -ObsInclude D:/sdk/libobs -ObsLibrary D:/sdk/obs.lib -Qt D:/sdk/qt6
node scripts/package-release.mjs --plugin
```

插件包与源码包必须同时提供。按 LICENSING.md 分层授权：客户端 / OBS 为 BSD-3-Clause，协议文档为 Apache-2.0，Relay / 网关为 AGPL-3.0-only。源码包包含当前两个工作树的受控源文件和构建脚本，不使用 git archive 丢失未提交修复。第三方 Rust 源码由 Cargo.lock 标识；打包目录内提供许可证集合和依赖清单。OBS/Qt/FFmpeg 为外部依赖，不随包提供。

## 发布门槛

- 版本一致，Rust/网页必要检查通过；插件重建并检查输入框回归。
- 包内两个服务程序运行 --version 为目标版本；插件两个 DLL 来自同一次构建。
- 保存构建日志、包清单、源码文件哈希、两仓库提交和 dirty 状态。正式发布应提交最终修改并重新打包，dirty 包只能作为预发布审核包。
- 在实际 Linux 服务器构建/短测或运行 Linux CI，未执行前不得声称已经提供验证过的 Linux 二进制。
- 已有实播反馈可作为人工验收记录；不要求重跑极端弱网研究矩阵。
- 对照安装说明做最终安装冒烟；删除任何用户凭据再附日志。不要把研究目录上传到发布资产。

## 公开发布

两个仓库提交最终修改，使用匹配的 `v1.0.0` 标签；本地校验完成后由发布者明确执行推送和创建 Release。上传平台包、配套源码包及 SHA256SUMS，把 RELEASE-1.0.0.md 作为发布说明，链接两个仓库的精确提交。不能只上传 DLL 而遗漏对应源码与许可证。

GitHub Actions 的 release-build 工作流只做 Linux 构建、测试和上传工作流产物，不自动创建公开 Release。实际运行结果以 Actions 日志为准。
