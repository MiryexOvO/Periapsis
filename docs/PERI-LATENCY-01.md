# 亚秒目标与测量口径

PERI 的研发目标为低于 1000 ms，目标不等于现有实现已经达标。
必须同时报告持续播放延迟、首帧等待、弱网冻结和恢复；不能用单项低延迟覆盖多秒中断。

当前 `native:live:check` 增加同机软件输入到输出测量：

- source_input：Rust 向实时合成编码工作进程提交一帧前。
- frame_output：真实解码与本地调度完成后；显示模式为 RGB 写入显示进程完成，headless 没有物理显示。
- 按同一发布生命周期的帧序号配对，报告匹配数、P50、P95、最大值和负墙钟样本数。
- 至少匹配30帧，否则检查失败；发生时钟回退的样本单独计数并使亚秒判定失败。

`subsecond.softwarePass` 要求全部匹配的软件链路样本低于1000ms且没有负时差；
`continuityPass` 要求输出最大间隔低于1000ms；`joinPass` 要求 `first_frame_ms` 低于1000ms；
`pass` 要求三者同时满足。
这个字段是目标评估，不因目标未达成而把协议正确性检查伪装成通过目标。

只展示成功输出的帧，因此必须同时查看 dropped、freeze_ms、max_gap_ms 和恢复计数。
持续丢帧时，软件链路样本可能仍很低，而 continuityPass 会失败。
没有包含真实摄像头/屏幕采集或物理屏幕呈现，没有跨主机时钟同步，不能称为真实采集到显示延迟。
`first_frame_ms` 使用播放器单调时钟，从进入播放操作（连接、鉴权、目录、解码器启动之前）
计到首帧实际解码并完成软件输出；重连不重置计时。它不包含操作系统启动播放器进程的耗时。
无损链路的 deadline/FIFO 加入结果另外写入 `startup-report.json`。
播放器选定视频轨道后主动调用 `request_keyframe`，等待独立帧时继续通过已有控制层限频重试，
避免被动等待自然 GOP。发布方仍决定是否响应，不能据此保证所有源的首帧都低于一秒。

目标未达成的案例应保留原始报告，按 source_input/编码/收取/解码/输出阶段追查。
新增模块、编码器或策略不能替代这项验收，也不承诺任意网络条件下保证亚秒。

## 弱网模拟的排队边界

报告中的 `profile.queueModel=admitted-datagrams-v2` 表示只有实际接纳的数据报才占用串行发送时间。
排队数64、排队延迟250ms的上限仍然保留；超过上限的包立即丢弃，不累积虚假的后续带宽债务。
旧模拟器会给被拒绝的包继续累加时间，其结果不能与 v2 结果直接比较来宣称协议性能提升。

Relay 追踪包含 `relay_key_send`（帧大小及剩余预算）、`relay_drop`（过期、依赖缺失或发送槽满）、
`relay_start_failed` 和 `relay_send_failed`。结合 `encoder_response`、`key_received`、`key_decoded`
及 `frame_output` 定位恢复瓶颈；发送完成仍不等于解码完成。

## 实验：限制编码器关键帧突发

实时合成源支持 `PERI_MAX_INTRA_PCT=100` 或 `200`（默认 `0`，保留编码器原配置）。
该选项透传到 libaom 的 `aom-params=max-intra-rate`，仅作用于示例编码器；协议接入方可选择自己的策略。
FFmpeg 参数接口参考：https://ffmpeg.org/ffmpeg-codecs.html#libaom_002dav1 。
这是相对于平均帧码率的编码器软目标，不能作为严格的网络字节上限或帧大小保证；初始关键帧也可能明显超出。
超出目标后不能截断编码包，否则破坏解码；应继续按完整媒体对象和原有截止时间处理。

在设置 `PYTHONPATH=dist/live-python` 和合适的 `PERI_FFMPEG` 后，设置该变量并运行
`pnpm native:live:check`；报告 `maxIntraPct` 标识配置。未设置时仍为原有对照配置。
同一源30帧离线PSNR用于暴露画质代价，不能代替网络播放画质测量。
候选配置需同时查看 `freeze_ms`、恢复次数及最大间隔，多个短冻结仍影响体验。
此选项不改变控制层限频、Relay驻留预算或接收预算，也尚未解决跨应用的独立帧字节预算协商。
