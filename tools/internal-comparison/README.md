# 内部对照实验

此目录为研发人员使用的独立Cargo工作区，不属于PERI正式协议、用户文档或SDK接口。
保留真实第三方依赖名称和实验记录，不对用户开放相应协议能力。

在仓库根目录执行：

```powershell
cargo test --manifest-path tools/internal-comparison/Cargo.toml --all-features
node --experimental-strip-types tools/internal-comparison/media-check.ts
```

媒体验收仍需本机PyAV与FFmpeg，具体完成边界见COMPARISON.md。
历史网页实验构建脚本使用该独立工作区，不能当作原生PERI浏览器支持。
