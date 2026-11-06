# OBS 节目音视频 / OBS program audio and video

当前开发入口传输一个 AV1 视频轨和一个 Opus 音轨。网页把两轨按源时间戳重新封装为 WebM，点击原生 video 控件播放。当前观看进程内所有观众共用一个 PERI 订阅；当前适配器每进程最多同时四路。此前的共享 JPEG 预览仍可独立使用。

## OBS 一键直播

加载配套脚本，填写服务器和推流码，点击「开始 PERI 推流」。脚本自动启动 OBS 直播，通过原生 OBS 输出模块取得 AV1 和 Opus 编码包，经本机管道交给 PERI 发布进程，以 QUIC 发送至 Relay。停播后恢复 OBS 原直播服务。首次使用自签名证书时在高级设置选择该服务器的 cert.der。运行环境详见配套 OBS 工程 README。

## 配置和启动服务器

首次运行 `python3 scripts/peri-server.py setup`。监听地址默认 0.0.0.0，推流码默认 stream。服务器配置保存在 ~/.config/peri/server.json。已有配置直接使用；要修改推流码，运行 `python3 scripts/peri-server.py stream-key` 并重启 Relay。任何知道服务器地址和推流码的人都可以尝试发布，公网直播宜自定义推流码。

~~~sh
python3 scripts/peri-server.py relay
python3 scripts/peri-server.py watch
python3 scripts/peri-server.py link
~~~

在不同终端用同一账户运行 Relay 和观看服务；link 显示服务器和推流码两栏。默认 UDP 4444 可省略；其他端口写为 peri://IP:端口。配置为 0.0.0.0 时直接显示监听地址模板，远程 OBS 将其换为主播可访问的 IP。默认两项服务监听 0.0.0.0，向客户端公布相同的端口。对外端口不同时，setup 支持 --public-udp 和 --public-tcp。旧配置在首次使用新入口时自动迁移。手动运行 program-av.py watch 时，传入 0.0.0.0:端口 会自动监听所有 IPv4 接口，并通过回环连接本机 Relay；--bind 可单独指定网页监听地址。

要更改观看端口，运行 `python3 scripts/peri-server.py watch-port`，依次填写本机监听 TCP 端口与观众访问的公网 TCP 端口，然后重启 watch。公网转发规则须对应这两个端口。

## 手动启动

构建当前 peri 程序并启动 Relay。向观看进程提供独立订阅令牌。需要 Python 和 PyAV 16.1.0（Windows 开发目录为 dist/live-python）。在服务器上的 PERI 仓库运行：

~~~powershell
$env:PYTHONPATH = "$PWD/dist/live-python"
$env:PERI_SUBSCRIBE_TOKEN = Read-Host "订阅令牌"
python scripts/program-av.py watch 127.0.0.1:4444 localhost ./cert.der live/av1 --peri ./target/debug/peri.exe
~~~

启动时打印实际监听地址；默认选择空闲端口，也可通过 --bind 和 --port 指定。公网部署时将已配置的 HTTPS 代理转发到该端口；远程访问权限由代理管理。Linux 上使用同样的脚本、对应平台构建的 peri，以及安装了 PyAV 的 Python。先前的 peri-viewer.service 启动 JPEG 预览，双轨模式使用上述 watch 命令。

## 观看

网页默认静音自动播放，播放器随视口放大，支持原生音量和全屏控制。网络或直播中断后网页重试。观众需要支持 AV1/Opus WebM 的浏览器。打开观看页后，watch 日志应先出现 subscribed（含尺寸），然后出现 first video keyframe；后者表示网关已经收到可用于开始播放的视频对象。

1440p60 配置要求 OBS 输出 2560×1440、60 fps，发布端目标视频码率 20 Mbps。可从 obs-publish.log 的 PERI_PROGRAM_VIDEO 核对实际发布速率。网页共享一次原生订阅，浏览器分别通过 HTTP 接收媒体，服务器出口须为每个观众提供足够视频、音频和协议带宽。更新时用 cargo build --release --locked -p peri-native --bin peri 构建发布与服务器端程序，服务器更新 scripts/program-av.py 后在维护窗口重启 Relay 与 watch。

## 开发验证

publish 子命令默认在回环 TCP 4455 等待一份实时 WebM，也可用 --file INPUT.webm 以源时间戳节奏发布 AV1/Opus 文件。对象需按解码顺序到达，视频依赖限定为低延迟连续参考链，中途加入等待周期性关键帧。支持程度取决于本机 OBS 的 FFmpeg 编解码器和浏览器。当前验证范围为本机 TCP 节目源、PERI Relay 和 Chrome。真实 OBS 混音录入、主观听音、长时间同步和公网播放仍需实机验证。
