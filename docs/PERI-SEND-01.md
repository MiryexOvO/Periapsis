<!-- SPDX-License-Identifier: Apache-2.0 -->
# PERI 按对象集中发送候选

状态：实验开关，默认关闭，线协议与媒体格式保持不变。
依据锁定的quinn-proto 0.11.19中TransportConfig::send_fairness的实现说明，默认对同优先级流轮转调度，关闭后按写入顺序服务。
本候选尝试减少参考帧负载交错，让较早对象先完整；是否改善期限内收齐概率必须由弱网测量判断。

```powershell
# 单次运行集中发送候选；1为默认轮转策略。
$env:PERI_SEND_FAIRNESS='0'
pnpm native:live:check

# 各一轮60秒周期中断，先默认后候选；其余参数固定。
$env:PERI_MATRIX_SEND_FAIRNESS='1,0'
$env:PERI_MATRIX_PROFILES='burst'
$env:PERI_MATRIX_INTRA='100'
$env:PERI_MATRIX_RATE_CONTROL='vbr'
$env:PERI_RECOVERY_CONCURRENCY='1'
$env:PERI_RECEIVE_RETAIN='0'
$env:PERI_RECOVERY_SUPPRESS='0'
pnpm native:matrix:check
```

PERI_SEND_FAIRNESS仅允许0/1，默认1。CLI在创建发布、Relay和观看端连接前配置；已经建立的连接不会动态切换。
SDK调用方可在其QUIC端点的TransportConfig上配置send_fairness(false)，随后使用原Publisher/Subscriber API，无需新增媒体字段。
高优先级独立对象仍优先于依赖对象；同优先级控制流也可能受排序影响，不能假定一定减少控制反馈等待。
不修改丢包判定阈值、拥塞控制、读取/发送截止时间、编码器码率、画质或播放缓冲。

可能的代价是较大或较早对象占据发送机会，延迟其他同优先级流。它不是带宽扩容或前向纠错机制，也不能保证恢复丢失的数据。
接入应同时看负载读取超时、累计冻结、软件延迟、下行字节和重连，不以单个最大间隔判定胜出。
网络报告记录sendFairness；matrix索引和日志区分每种发送策略，单项失败仍保留其他项结果。

单轮固定随机种子不能固定实际QUIC分包、丢包位置或操作系统调度；正式收益结论还需要多轮交替和不同网络条件。

## 2026-11-06四轮顺序反转诊断

使用上述burst配方，按默认、候选、候选、默认顺序，各运行60秒损伤窗口，均解码931帧、零重连，当前软件亚秒三判据均通过。

| 顺序 | 策略 | 软件延迟P95 | 最大输出间隔 | 冻结比例 | 本地驻留超时 |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | 默认轮转 | 480ms | 618ms | 9.72% | 13 |
| 2 | 集中发送 | 438ms | 656ms | 9.01% | 9 |
| 3 | 集中发送 | 388ms | 652ms | 8.23% | 9 |
| 4 | 默认轮转 | 356ms | 562ms | 6.84% | 10 |

两轮均值：冻结比例默认8.28%、候选8.62%；P95默认418ms、候选413ms；下行字节默认2,488,899、候选2,501,162。
本地驻留超时合计默认23、候选18；接收侧RESET(1)合计默认14、候选35。减少某一种期限失败并未带来整体连续性改善，候选继续默认关闭（PERI_SEND_FAIRNESS=1）。
各组仅两轮、单一损伤配方，不能据此证明任何网络下都更好或更差。编码参数及同源30帧离线PSNR/SSIM相同，不代表丢帧后的观看质量相同。
均值仅为这批样本的描述统计，逐轮P95均值不是合并全部帧后的总体P95；字节数受实际播放时长、重传和恢复帧数影响，不直接等同编码效率。
完整索引见本地dist/send-order-comparison.json及dist/matrix-yQJuQa、dist/matrix-nYHF16。
