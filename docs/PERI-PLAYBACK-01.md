# PERI 原生 GOP 视频播放

`peri publish --gop` 发布带依赖帧的 AV1 IVF；`peri play` 经独立 PERI/QUIC Relay 接收，
由 FFmpeg 持续解码为 RGB，再通过 FFplay 显示。采集、转码、音频和网页接入不在此命令内。
构建、证书及角色凭据沿用 [原生服务说明](PERI-SERVICE-01.md)。

## 准备与发布

需要 FFmpeg、FFprobe、FFplay；当前验证工具版本为 8.1.2。
如果 PATH 有多套版本，可分别设置 `PERI_FFMPEG`、`PERI_FFPROBE`、`PERI_FFPLAY` 为可执行文件路径。
文件适配器仍限制 16 MiB、120 个包、归一化时间轴 120 秒。

以下命令生成固定 15 fps、每秒一个关键帧、关闭前瞻与隐藏替代参考帧的低延迟测试源。
输出路径应选择新的文件名：

```powershell
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=320x180:rate=15 -t 2 -c:v libaom-av1 -strict -2 -cpu-used 8 -usage realtime -lag-in-frames 0 -auto-alt-ref 0 -g 15 -crf 40 -b:v 0 -pix_fmt yuv420p -f ivf dist/peri-local/gop.ivf
.\target\debug\peri.exe publish 127.0.0.1:4444 localhost dist/peri-local/cert.der live/av1 dist/peri-local/gop.ivf --gop --loop
```

`--gop` 和 `--all-intra` 二选一。GOP 模式通过管道探测已读取的同一份字节，
检查包与解码帧数量、PTS、负载长度和固定尺寸，要求首帧为关键帧。
不支持重排序、多个显示帧合包、隐藏帧或动态配置。
这不是通用 AV1 比特流验证器；仅支持上述受限编码配置。

依赖采用保守的解码顺序链：非关键帧依赖前一个单元，意味着自最近关键帧起都必须顺序解码。
它不是 AV1 的精确参考槽位图，不据此独立丢弃参考帧。
循环发布时依赖编号与全局帧号一起偏移。

## 观看或录制

在订阅终端设置 `PERI_SUBSCRIBE_TOKEN` 后：

```powershell
.\target\debug\peri.exe play 127.0.0.1:4444 localhost dist/peri-local/cert.der live/av1 300
```

观看 300 个成功解码的帧后退出，Ctrl+C 可提前停止；中途加入等待新的关键帧。
当前显示端验证范围为固定 15 fps、最高 1080p；调度器按媒体 PTS 工作，显示进程按 15 fps 接收 RGB。
网络中断后最多退避重试 8 次，重新校验证书、鉴权、目录并等待关键帧。
初次认证或建立播放前失败直接退出。连接恢复后重新建立本地播放时间映射。
解码超过预算或参考链过期时重建解码器，等待关键帧；连续 10 秒无解码进展则失败退出。

`--headless` 保留实际解码和调度，不启动显示进程。
实验参数 `--fifo` 使用逐帧到达后的驻留预算作对照，不执行基于原始播放目标的过期处理；
仍保留同样的资源上限与依赖确认，不是无限缓存基线。

`receive` 同样可录制 GOP 流，首帧必须是关键帧，声明的依赖必须已写入；仍拒绝覆盖文件。

## 时钟与边界

首个关键帧映射为本地收到时刻之后 120 ms；之后按媒体时间差调度，目标后再容忍 120 ms。
DecodeBuffer 限制为 32 帧、8 MiB，驻留时间还受协商上限限制；解码器仅在读出完整 RGB 帧后确认成功。
RGB 单帧最高 1920×1080×3 字节；管道、QUIC 接收窗口和子进程内存另计。
接收仍串行读取对象；慢流和 Relay 严格发送时限可能导致连接关闭，当前通过重订阅恢复。

`PERI_PLAY_DONE` 输出成功解码数量、局部丢弃数、参考链恢复数、重连数、
两次输出间最大间隔、本地播放目标迟到的最大值及 P50/P95。
`dropped` 不统计 Relay 或网络中未到达的单元；`max_gap_ms` 包括重连后的间隔。
头部无画面的初始等待不计入 gap；重新映射时钟后，迟到数值不能表示累计跨端延迟。
显示模式统计 RGB 写入完成，headless 统计解码调度完成，均不是屏幕实际呈现时间。
没有跨机时钟同步、音画同步或采集到显示的端到端延迟结论。

## 可复现检查

```powershell
pnpm run native:gop:check
```

检查双接收录制、逐帧编码与解码校验、循环依赖、中途加入、持续实际解码及显示输出管道。
显示输出使用 SDL dummy 驱动检查，不代替物理屏幕验收。

两种策略分别通过本机 UDP 代理：下行 24,000 B/s，额外延迟 45 ms，持续 2 秒；
扰动期间每第 97 个下行包丢弃，队列最多 64 个数据报、调度延迟最多 250 ms，超出也丢弃。
上行不施加扰动。计数包括 QUIC 控制包，配置可复现，具体丢包数及重连时刻受系统调度影响。
该配置用于恢复回归，不保证低于每个时刻的视频码率，不能替代带宽阶跃实验。

报告输出到被 Git 忽略的 `dist/gop-check-*/network-report.json`。
单次顺序实验不足以证明任一策略更优；正式性能研究仍需重复试验、统一起点、码率阶跃、
真实显示测量、画质与冻结比例，以及基础协议和 WebRTC 对照。
