# OBS 节目音视频 / OBS program audio and video

当前开发入口传输一个 AV1 视频轨和一个 Opus 音轨。网页把两轨按源时间戳重新封装为 WebM，点击原生 video 控件播放。每位观众使用一个 PERI 订阅；当前适配器每进程最多同时四路。此前的共享 JPEG 预览仍可独立使用。

## 配置和启动服务器

首次运行 `python3 scripts/peri-server.py setup`。监听地址默认 0.0.0.0，推流码默认 stream。服务器配置保存在 ~/.config/peri/server.json。已有配置直接使用；要修改推流码，运行 `python3 scripts/peri-server.py stream-key` 并重启 Relay。任何知道服务器地址和推流码的人都可以尝试发布，公网直播宜自定义推流码。

~~~sh
python3 scripts/peri-server.py relay
python3 scripts/peri-server.py watch
python3 scripts/peri-server.py link
~~~

在不同终端用同一账户运行 Relay 和观看服务；link 显示服务器和推流码两栏。默认 UDP 4444 可省略；其他端口写为 peri://IP:端口。配置为 0.0.0.0 时直接显示监听地址模板，远程 OBS 将其换为主播可访问的 IP。默认两项服务监听 0.0.0.0，向客户端公布相同的端口。对外端口不同时，setup 支持 --public-udp 和 --public-tcp。旧配置在首次使用新入口时自动迁移。手动运行 program-av.py watch 时，传入 0.0.0.0:端口 会自动监听所有 IPv4 接口，并通过回环连接本机 Relay；--bind 可单独指定网页监听地址。

## 手动启动

构建当前 peri 程序并启动 Relay。向观看进程提供独立订阅令牌。需要 Python 和 PyAV 16.1.0（Windows 开发目录为 dist/live-python）。在服务器上的 PERI 仓库运行：

~~~powershell
$env:PYTHONPATH = "$PWD/dist/live-python"
$env:PERI_SUBSCRIBE_TOKEN = Read-Host "订阅令牌"
python scripts/program-av.py watch 127.0.0.1:4444 localhost ./cert.der live/av1 --peri ./target/debug/peri.exe
~~~

启动时打印实际监听地址；默认选择空闲端口，也可通过 --bind 和 --port 指定。公网部署时将已配置的 HTTPS 代理转发到该端口；远程访问权限由代理管理。Linux 上使用同样的脚本、对应平台构建的 peri，以及安装了 PyAV 的 Python。先前的 peri-viewer.service 启动 JPEG 预览，双轨模式使用上述 watch 命令。

## 主播

1. 在 Windows OBS 的高级输出设置中选择自定义 FFmpeg 录制输出，输出类型选择 URL，地址为 tcp://127.0.0.1:4455，容器选择 webm。使用低延迟 AV1 视频（关闭前瞻和帧重排，建议关键帧间隔一秒），Opus 音频选定含节目混音的轨道，48 kHz 单声道或立体声。当前适配器接受最大 1920×1080；建议先用 320×180 或 640×360 验证。此步骤会占用 OBS 录制输出，先保存常用配置。
2. 加载配套 obs-periapsis/peri_publish.py，填写服务器与推流码，在高级设置选择从该 Relay 安全复制的 cert.der，资源名和证书服务器名与服务器配置一致。勾选「节目画面 + 混音（本机 WebM 输出）」，点击「开始 PERI 推流」，再在 OBS 点击「开始录制」，把节目送进本机适配器。该模式不使用虚拟摄像机。
3. 观众打开观看页并点击播放。停播先停止录制，再停止 PERI 推流。重新开播后重新载入观看页。节目输入分辨率或音轨参数变化时应重启发布会话。

## 开发验证

publish 子命令默认在回环 TCP 4455 等待一份实时 WebM，也可用 --file INPUT.webm 以源时间戳节奏发布 AV1/Opus 文件。对象需按解码顺序到达，视频依赖限定为低延迟连续参考链，中途加入等待周期性关键帧。支持程度取决于本机 OBS 的 FFmpeg 编解码器和浏览器。当前验证范围为本机 TCP 节目源、PERI Relay 和 Chrome。真实 OBS 混音录入、主观听音、长时间同步和公网播放仍需实机验证。
