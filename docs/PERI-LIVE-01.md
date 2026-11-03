# PERI 实时 AV1 编码测试源

`peri live` 在运行期间逐帧生成测试画面，由持续运行的 libaom AV1 编码器编码并通过原生 PERI 发布。
不是预编码文件跳帧；当前是合成画面，尚未接入屏幕、摄像头、麦克风或硬件编码。
视频固定 320×180、15 fps、YUV420P，编码器目标码率 300 kbit/s，自然关键帧间隔 60 帧。

## 安装与运行

依赖 Python、PyAV 16.1.0，以及验证/观看使用的 FFmpeg、FFplay。
Windows x64 可使用以下脚本，从官方 PyPI 下载匹配当前 Python 的 wheel，校验发布哈希后安装至忽略的 dist：

```powershell
python scripts/setup-live-python.py
$env:PYTHONPATH = "$PWD\dist\live-python"
cargo build -p peri-native --bin peri
```

其他平台可使用 `python -m pip install --target dist/live-python av==16.1.0`，并相应设置 PYTHONPATH。
本次验证使用 Windows Python 3.14；其他平台的编码器可用性需自行检查。

证书、Relay 和角色凭据配置沿用[服务说明](PERI-SERVICE-01.md)。在设置发布凭据的终端运行：

```powershell
.\target\debug\peri.exe live 127.0.0.1:4444 localhost dist/peri-local/cert.der live/av1 scripts/live-av1-worker.py 900
```

最后参数为生成帧数，范围 1–13500；900 帧约一分钟。Ctrl+C 取消发布并结束编码子进程。
使用 `PERI_PYTHON` 指定解释器，工作进程路径作为显式参数。
观看端使用[播放命令](PERI-PLAYBACK-01.md)。

## 编码与恢复语义

Rust 按单调时钟请求生成每一帧，编码工作进程每次只返回一个实际编码包。
收到有效关键帧请求后标记待响应，在下一次允许的编码周期强制 I 帧，并检查编码器返回的关键帧标志。
[PyAV 帧接口](https://pyav.org/docs/stable/api/video.html)用于设置帧类型，编码器返回的包仍必须满足本适配器约束。
输出帧号、PTS 连续，不通过跳到未来内容响应请求；非关键帧仍采用前帧顺序依赖。

Relay 在本地放弃参考链时也会主动请求恢复，避免依赖“后续帧到达观看端”才能发现缺口。
播放器对单视频顺序依赖链的缺失参考也会请求恢复。
多个请求合并为一个待响应状态；相邻强制响应至少间隔 15 帧，最多每秒一次。
自然关键帧不计入这个强制响应配额。`PERI_KEYFRAME_RESPONSE` 输出帧号和编码字节数，
表示编码完成并验证关键帧标志，不代表网络交付或实际显示。
目标码率不是 QUIC 线上流量硬上限；当前没有根据带宽反馈动态调整码率、分辨率或响应预算。
编码落后时仍保留帧序列，可能积压本地时间；每次编码交换限两秒，未实现实时采集丢帧策略。

本地工作协议为：输入大端 u64 帧索引 + u8 强制标记；输出 u32 负载长度 + u64 帧索引 + u8 关键帧标记 + 编码负载。
负载上限 1 MiB−68，元数据匹配后才分配。固定配置仅支持单个显示帧/编码包；进程失败即退出，不静默替换编码配置。

## 自动检查

```powershell
pnpm run native:live:check
```

检查编码器在第 0、10 帧生成关键帧且 30 帧均可实际解码；再检查持续发布和正常观看。
网络测试默认三轮交替策略，各播放 60 帧：下行 24,000 B/s、45 ms 延迟、持续两秒，
每第 11 个包丢弃，另外在扰动开始后 300–480 ms 完全丢弃下行包。
代理排队最多 64 包/250 ms，超限也丢弃，上行不施加扰动。
必须发生真实丢包、没有重连，并观察到满足间隔限制的编码器响应。
报告位于忽略目录 `dist/live-check-*/network-report.json`。

此内容和扰动配置与文件发布实验不同，不能直接比较冻结数字来证明编码方式或策略更优。
显示管道使用 SDL dummy 检查；未测量物理屏幕呈现或采集到显示的延迟。
