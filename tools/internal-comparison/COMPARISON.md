# PERI 与 MoQ 对照的实施边界

当前已有原生MoQT-22加密UDP对象传递回归（moq-net固定0.3.18），以及浏览器媒体演示。
这两项不能替代与原生PERI同条件的长时媒体性能对照；当前没有已验证的MoQ胜负结论。

正式对照需要一个原生MoQ媒体适配器，并共用以下输入和测量路径：

1. 同一个编码器输出的AV1包序列与原始像素，记录文件SHA-256、分辨率、帧率、编码器版本、码率、关键帧策略。
2. 相同发布/中继/接收拓扑、TLS校验、同一FFmpeg解码器和输出时钟；先关闭自适应做固定输入对照，再单独比较恢复与自适应策略。
3. 共用network-profiles.ts中的持续时间、容量、单向附加延迟、丢包种子及队列上限。
4. 每种配方交替运行两个实现并重复，记录端到端软件输出、冻结比、首帧、输出帧率、实际交付字节与资源开销。
5. 画质应比较实际输出，包括停顿时保持上一画面的失真；现有30帧离线PSNR/SSIM只能说明源编码代价。

协议版本必须逐项注明，MoQT-22的结果不可外推成所有MoQ实现或最新草案的结果。
已提供可选的对象封装适配层：`peri-internal-comparison` 的 `media-interop` feature。
`media_interop::encode_frame/decode_frame`复用peri-native的Frame和对象封装校验，避免重新编码或复制媒体语义。
这是PERI媒体对象承载于MoQ的实验配置，不是官方MoQ媒体封装；目录与版本标识须由应用显式约定。
默认feature关闭，原生PERI主路径不依赖MoQ。

运行 `node --experimental-strip-types tools/internal-comparison/media-check.ts`，先按实时源说明配置PyAV与FFmpeg。
该命令生成同一段30帧AV1夹具，经两条带证书校验的原生QUIC传输后写回IVF，
用同一个FFmpeg逐帧核对像素哈希；输出report.json包含源文件哈希与固定MoQ版本。
这是回环媒体完整性验证，不测实时输出时钟、冻结、CPU或协议性能胜负。

MoQ可优先交付较新GOP；测试逐组生产与读取，保证完整夹具被消费，不能将此策略当作实时对照播放器。
后续仍须实现有界的实时跨组调度、共享输出时钟，以及相同拓扑和网络配方的长测。
目录必须由调用方校验并预先约定；适配器不负责鉴权、目录发现、反馈控制、自动资源回收策略或MoQ内部缓存配额。
MoQ订阅侧须在完整包交给decode_frame之前设置接收边界；不能仅依赖事后包长校验来宣称接收内存有界。
因此正式性能对照仍未完成，本文件同时记录已执行的正确性范围与待实施的对照契约。

## 已接入的共享调度与输出验证

`peri_native::playback` 提供公共Decoder、PlaybackClock和单视频GopScheduler。
原生CLI复用同一Decoder与PlaybackClock；MoQ与PERI夹具验收都经过相同GopScheduler、真实解码和时钟等待，
报告sharedDecoderClock=true，并比较全部RGB输出的SHA-256与源视频。
新关键帧替换旧GOP时返回ResetDecoder，调用方须取消旧解码任务、停止并重建解码器，显式重建时钟。
旧组回流不能替换新组；过期/超限关键帧不能清空有效旧状态，依赖只有实际解码确认后才能执行。

该调度器不读取MoQ网络流；验收仍逐组生产消费，未实现实时并发订阅和组流取消。
因此不把夹具的顺序交付或RGB一致性称为弱网性能胜负结果。
缓存字节上限仅含排队负载，帧槽同时计入解码中帧；交给调用方的负载仍由调用方拥有，必须及时释放。
默认4MiB排队/32帧，允许上限8MiB/128帧；每帧仍受媒体对象大小限制。

## 有界并发组接收

`media_interop::LiveReceiver`接收一个MoQ track::Subscriber和已校验目录，默认最多4个组任务、8个持有帧槽。
默认单包上限1MiB+25，字节额度为4倍该值；帧槽和字节许可证随LiveFrame持有，释放后归还。
声明尺寸在本层read_all及媒体拷贝之前校验，但上游MoQ库可能已经接收/缓存数据，仍必须单独配置缓存池。
媒体解析存在临时拷贝，本层许可证不是进程RSS上限；into_frame转移后，由GopScheduler的独立配额负责。

每次组内收取、等待配额、读负载与排队发送共用默认300ms窗口；队列交付前再检查驻留时间。
到达更高组并验证关键帧后取消旧组任务；并发耗尽时先取消最旧组并回收任务槽，再启动新组。
读取超时及上游Old/Lagged/Evicted/Dropped作为组缺口，畸形数据与其他错误返回Err。
关键帧的媒体序号不得随着更高MoQ组号回退；组内首帧须为关键帧，其余帧不得跨GOP声明依赖。

收到Frame后将into_frame结果交给GopScheduler，并沿用LiveFrame.deadline，不得按出队时间重新计算期限；调用方须结合group判断Gap是否属于当前GOP，不能把组缺口直接等同连接关闭。
LiveReceiver持有后台任务，Drop会取消组读取并释放本层许可证；它不替调用方关闭整个MoQ会话。
调用方必须持续读取事件，及时释放LiveFrame，并在Err后释放接收器执行会话错误处理。

`内部 media-check.ts`新增真实MoQT半帧停滞/新关键帧到达验证，报告liveGroupRecovery=true，
恢复关键帧经共享调度/解码后RGB须与源首帧相同。它仍不是完整实时弱网性能对照。
