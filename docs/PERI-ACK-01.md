# PERI 频繁传输确认候选

状态：实验能力，默认关闭。此策略调整原生QUIC连接的确认频率请求，不改变PERI控制协议、对象格式或媒体编码。
依据锁定quinn-proto0.11.19的AckFrequencyConfig：不支持该QUIC扩展的对端会忽略请求；请求不是强制的固定时延保证。

## 接入与默认值

PERI_ACK_MODE只允许default/frequent，默认default保持既有行为。
frequent请求每个eliciting包及时确认（阈值0）、最大确认等待1ms及乱序阈值1。
实际最大等待还受对端min_ack_delay等传输参数约束，不等于从网络观测证明了精确1ms响应。
配置在连接建立前应用，已连接会话不动态切换；不修改拥塞控制、丢包判定、150ms正常读取预算或播放缓冲。

```powershell
$env:PERI_ACK_MODE='frequent'
pnpm native:live:check
```

原生SDK接入方可在自己的TransportConfig上设置AckFrequencyConfig，随后使用现有Publisher/Subscriber。
Session、Publisher和Subscriber新增transport_stats()，返回本连接QUIC统计快照，便于观察ACK频率请求、ACK帧、UDP字节与路径估计。
统计不是媒体接收、解码或播放成功证明；不涉及peri-types/peri-session的传输无关协商语义。

## 可复验对照

```powershell
$env:PERI_MATRIX_ACK_MODES='default,frequent,frequent,default'
$env:PERI_MATRIX_SEND_FAIRNESS='1'
$env:PERI_MATRIX_PROFILES='burst'
$env:PERI_MATRIX_RATE_CONTROL='vbr'
$env:PERI_MATRIX_INTRA='100'
$env:PERI_RECOVERY_CONCURRENCY='1'
$env:PERI_RECEIVE_RETAIN='0'
$env:PERI_RECOVERY_SUPPRESS='0'
pnpm native:matrix:check
```

四项按顺序运行，matrix索引记录ackMode和ackOrder；日志带运行序号，重复模式不会覆盖之前结果。
network-report的profile记录ackMode，播放总结transport保存最后一次观看连接的收发请求计数、ACK帧数、UDP收发字节、RTT及丢包估计。
计数覆盖此连接的握手及控制流；发生重连时不累计先前连接。实验要求零重连才能直接比较整次观看的计数。
lost_packets是观看端发送方向的路径估计，不能当成受损下行的丢包数；最后的RTT快照也不能代替损伤窗口内的RTT分布。
同版本双端frequent实验要求ACK_FREQUENCY收发计数均大于零，未观察到时先保存网络报告再判失败，不能只根据环境变量宣称生效。
收到请求仍不证明每个包都按1ms精确确认，ACK帧也不等同UDP数据报数量。

## 边界

频繁ACK可能增加上行字节及处理开销。当前弱网配方仅损伤下行，上行未限速、未施加丢包，尚未覆盖双向拥塞条件。
实际吞吐、累计冻结、读取超时和上行开销须一起判断，不能只看一次软件最大延迟。
真实QUIC测试验证双方请求收发及8个对象逐字节一致；本轮未另建完全不支持扩展的异构实现端点。
不支持扩展时忽略配置的行为依据锁定实现源码，不宣称已经完成异构互通与降级验收。
同机软件输入到输出时延不是物理采集到屏幕时延，合成源离线PSNR也不是弱网最终观看画质。

## 2026-11-06四轮结果

按默认、候选、候选、默认顺序完成四轮60秒周期中断，均931帧、零重连，软件首帧、最大延迟和最大输出间隔三个亚秒判据均通过。

| 轮次 | 模式 | 软件延迟P95 | 最大输出间隔 | 冻结比例 | 观看连接上行字节 | 请求收/发 |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| 1 | 默认 | 359ms | 653ms | 8.27% | 101942 | 0/0 |
| 2 | 频繁确认 | 468ms | 549ms | 7.08% | 114537 | 1/1 |
| 3 | 频繁确认 | 374ms | 761ms | 9.29% | 118368 | 1/1 |
| 4 | 默认 | 388ms | 727ms | 8.52% | 99774 | 0/0 |

两轮描述均值：默认冻结8.40%、候选8.19%；逐轮P95均值默认373.5ms、候选421ms；观看连接上行字节默认100858、候选116452.5（约增加15.5%）。
逐轮P95均值不是合并全部帧的总体P95。上行字节包含握手与控制流，不能把全部增加量都归为ACK帧。
本地驻留超时默认合计21次、候选22次，没有证明减少本地期限失败。每模式仅两轮，实际包序与调度仍不同，不宣称统计显著性或泛化性能。
请求实际收发已经验证，但本轮整体收益不足，PERI_ACK_MODE继续默认default。
原始索引dist/matrix-adKHHp，聚合dist/ack-comparison.json；相同编码参数的离线质量报告一致，不表示弱网最终显示画质一致。
