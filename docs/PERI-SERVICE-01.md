# PERI 原生分进程服务

`peri` 提供独立 Relay、AV1 文件发布和接收录制。使用 QUIC/TLS 与实验 ALPN
`peri-dev-01`，尚无网页播放器、推流软件插件或互联网运行验证。

## 构建与证书

在项目根目录构建一次，再在各终端运行生成的程序：

```powershell
cargo build -p peri-native --bin peri
.\target\debug\peri.exe init dist/peri-local
```

Linux/macOS 使用 `./target/debug/peri`；自定义 CARGO_TARGET_DIR 时调整路径。
init 生成 localhost 开发证书 cert.der 和 PKCS#8 DER 私钥 key.key，拒绝覆盖已有文件。
客户端显式信任该证书并验证服务器名称，不关闭 TLS 校验。
当前支持单个 DER 证书和 DER 私钥，不读取 PEM 证书链。
远程使用需提供与服务器名称匹配的证书和信任配置；本轮仅验证本机独立进程。

## 凭据

运行两次以下命令，各生成一个随机值作为发布、订阅凭据：

```powershell
node -p "require('node:crypto').randomBytes(24).toString('hex')"
```

凭据须为32–128个ASCII可见字符，发布与订阅必须不同。下方占位符需替换为生成值；
Relay与客户端对应的凭据值须一致。环境变量在每个相应终端分别设置：

```powershell
$env:PERI_PUBLISH_TOKEN = '<your-generated-publish-token>'
$env:PERI_SUBSCRIBE_TOKEN = '<your-generated-subscribe-token>'
```

Relay需要两个变量，发布端只需要发布变量，接收端只需要订阅变量。
凭据不放命令参数或URL，通过TLS控制流中的authorization字符串传递。
服务端不输出控制消息内容，凭据类没有Debug输出；发布/订阅角色不可互换。
目前为启动时加载的静态凭据，没有用户体系、动态轮换或过期令牌。

## 终端 A：Relay

```powershell
.\target\debug\peri.exe relay 127.0.0.1:4444 dist/peri-local/cert.der dist/peri-local/key.key live/av1
```

输出PERI_RELAY_READY表示监听成功。绑定地址与单个资源由参数指定。
默认最多16个TLS/授权/媒体连接、32条待回收任务记录，TLS及应用协商分别限5秒。
每连接最多1个双向控制流、8个单向媒体流；Relay订阅者限8个，媒体队列另有配额。
每次只允许一个发布者，包括目录准备阶段；第二个发布者不能替换现有资源。
发布者退出、失败或连续5秒无对象时关闭当前资源及下游，之后允许重新发布。
新发布生命周期不自动迁移旧订阅者。发布者上线前订阅会被拒绝。
Ctrl+C关闭监听并取消连接任务。

## 准备全关键帧 AV1 文件

需要支持libaom-av1的FFmpeg：

```powershell
ffmpeg -hide_banner -loglevel error -y -f lavfi -i testsrc2=size=320x180:rate=15 -t 2 -c:v libaom-av1 -strict -2 -cpu-used 8 -g 1 -crf 40 -b:v 0 -pix_fmt yuv420p -f ivf dist/peri-local/source.ivf
```

-strict -2兼容把该编码器标为实验的FFmpeg构建；-y会覆盖所指定的测试文件。
全关键帧是当前文件适配器的限制，不是正式媒体协议的唯一帧类型。

## 终端 B：发布

设置发布凭据后：

```powershell
.\target\debug\peri.exe publish 127.0.0.1:4444 localhost dist/peri-local/cert.der live/av1 dist/peri-local/source.ivf --all-intra --loop
```

发布端PERI_PUBLISH_READY表示目录已发送；Relay的PERI_RESOURCE_READY表示可订阅。
按文件时间戳节奏发送，不转码、不内置采集。loop循环续增时间戳及全局序号。
Ctrl+C停止；不加loop时发送一次并留0.5秒本地排空时间，不代表播放确认。

all-intra是调用方声明，解析器不分析任意AV1依赖；不能这样发送带参考帧的文件。
输入限16MiB、120帧、归一化时间轴120秒，帧间隔8,333–1,000,000微秒。
时长由相邻PTS推导，尾帧沿用前帧时长；单帧优先使用合适的时间基准，否则取33,333微秒。

## 终端 C：接收录制

设置订阅凭据后：

```powershell
.\target\debug\peri.exe receive 127.0.0.1:4444 localhost dist/peri-local/cert.der live/av1 dist/peri-local/received.ivf 30
```

收到30帧后输出PERI_RECEIVE_DONE 30。使用不同输出路径可同时运行多个接收端。
选择首条AV1视频轨道，当前只接受全关键帧，流式写文件且拒绝覆盖已有文件。
录制限10,000帧、128MiB编码负载；失败可能留下不完整文件。这不是实时观看UI。

## 自动验证

```powershell
pnpm run native:check
# 或 npm run native:check
```

需要Node.js 24+、Rust与支持AV1编解码的FFmpeg，可用PERI_FFMPEG指定路径。
检查构建程序，启动独立Relay、发布者与双接收端，验证编码与解码校验值，
错误凭据、角色、证书、服务器名称和重复发布者拒绝，以及退出后重新发布。
进程在检查结束后关闭，产物位于被Git忽略的dist/service-check-*。
浏览器迁移、真实GOP分析、实时音视频播放、软件插件、完整反馈、P2P和MPQUIC仍待完成。
