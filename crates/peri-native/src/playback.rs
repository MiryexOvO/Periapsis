//! 传输无关的AV1解码、单调播放时钟及单视频GOP调度。
use crate::{
    Error,
    av1::IvfWriter,
    media::{Catalog, DecodeBuffer, Event, Frame, FrameKind, Track, TrackKind},
};
use peri_types::Agreement;
use std::{
    io::Cursor,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
};
type DecodeError = Box<dyn std::error::Error + Send + Sync>;
fn command(name: &str, variable: &str) -> Command {
    let mut command = Command::new(std::env::var(variable).unwrap_or_else(|_| name.into()));
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}
/// 共享FFmpeg解码器：只有完整RGB输出后才算成功；decode被取消后必须停止并重建。
pub struct Decoder {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    pixels: Vec<u8>,
    // 进入一次解码即置为false；取消/错误后保持不可复用，避免像素与帧号错配。
    ready: bool,
    trace: bool,
    started: Instant,
}
impl Decoder {
    /// 启动固定单线程AV1解码，最多1080p；进程释放时自动终止。
    pub async fn new(track: &Track) -> Result<Self, DecodeError> {
        let started = Instant::now();
        let trace = std::env::var("PERI_DECODER_TRACE").as_deref() == Ok("1");
        let width = track.width.ok_or("width")?;
        let height = track.height.ok_or("height")?;
        if track.codec != "video/av1"
            || track.kind != TrackKind::Video
            || width == 0
            || height == 0
            || u32::from(width) * u32::from(height) > 1920 * 1080
        {
            return Err("playback limited to 1080p".into());
        }
        let mut child = command("ffmpeg", "PERI_FFMPEG")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-xerror",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-fpsprobesize",
                "0",
                "-threads",
                "1",
                "-flags",
                "low_delay",
                "-blocksize",
                "32",
                "-f",
                "ivf",
                "-i",
                "pipe:0",
                "-map",
                "0:v:0",
                "-fps_mode",
                "passthrough",
                "-threads",
                "1",
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-flush_packets",
                "1",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        if trace {
            eprintln!("PERI_DECODER spawn_us={}", started.elapsed().as_micros());
        }
        let mut input = child.stdin.take().ok_or("decoder stdin")?;
        let header = IvfWriter::new(Cursor::new(Vec::new()), track.clone())?
            .finish()?
            .into_inner();
        input.write_all(&header).await?;
        if trace {
            eprintln!("PERI_DECODER header_us={}", started.elapsed().as_micros());
        }
        let output = child.stdout.take().ok_or("decoder stdout")?;
        Ok(Self {
            ready: true,
            trace,
            started,
            child,
            input,
            output,
            pixels: vec![0; usize::from(width) * usize::from(height) * 3],
        })
    }
    /// 输入须先通过媒体校验；调用方必须设置超时，失败后不能复用半帧管道。
    pub async fn decode(&mut self, frame: &Frame) -> Result<(), DecodeError> {
        if !self.ready {
            return Err("decoder requires restart after interrupted frame".into());
        }
        self.ready = false;
        // 输入与输出并行推进，避免子进程输出管道写满时反过来阻塞输入。
        // 取消任一方向后仍须停止并重建解码器，不能继续使用半帧状态。
        let trace = self.trace;
        let started = self.started;
        let sequence = frame.sequence;
        if trace {
            eprintln!(
                "PERI_DECODER begin sequence={sequence} us={}",
                started.elapsed().as_micros()
            );
        }
        let input = &mut self.input;
        let output = &mut self.output;
        let pixels = &mut self.pixels;
        let write = async {
            input
                .write_all(&(frame.payload.len() as u32).to_le_bytes())
                .await?;
            input.write_all(&frame.timestamp_us.to_le_bytes()).await?;
            input.write_all(&frame.payload).await?;
            input.flush().await?;
            if trace {
                eprintln!(
                    "PERI_DECODER written sequence={sequence} us={}",
                    started.elapsed().as_micros()
                );
            }
            Ok::<(), std::io::Error>(())
        };
        let read = async {
            // 按块追踪首字节与完整帧的耗时，同时明确拒绝半帧EOF。
            let mut count = 0;
            let mut chunks = 0;
            while count < pixels.len() {
                let size = output.read(&mut pixels[count..]).await?;
                if size == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "decoder output ended inside frame",
                    ));
                }
                count += size;
                chunks += 1;
                if trace && chunks == 1 {
                    eprintln!(
                        "PERI_DECODER first_chunk sequence={sequence} bytes={size} us={}",
                        started.elapsed().as_micros()
                    );
                }
            }
            if trace {
                eprintln!("PERI_DECODER chunks sequence={sequence} count={chunks}");
            }
            if trace {
                eprintln!(
                    "PERI_DECODER pixels sequence={sequence} us={}",
                    started.elapsed().as_micros()
                );
            }
            Ok::<_, std::io::Error>(count)
        };
        tokio::try_join!(write, read)?;
        self.ready = true;
        Ok(())
    }
    /// 最近一次成功解码的RGB像素；不代表已经呈现到物理屏幕。
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    /// 终止并回收子进程，恢复参考链时应重新创建解码器。
    pub async fn stop(&mut self) {
        self.ready = false;
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

/// 单调播放时钟；到达抖动不改变既有媒体时间映射，显式恢复时才重建锚点。
pub struct PlaybackClock {
    lead: Duration,
    anchor: Option<(u64, Instant)>,
}
impl PlaybackClock {
    /// 预留播放前导最多一秒，不能用不断重建时钟掩盖迟到。
    pub fn new(lead: Duration) -> Result<Self, Error> {
        if lead > Duration::from_secs(1) {
            return Err(Error::Protocol("playback lead"));
        }
        Ok(Self { lead, anchor: None })
    }
    /// 在已选择独立帧时重建锚点；使用本地单调时钟，不依赖跨端墙钟。
    pub fn rebase(&mut self, timestamp_us: u64, now: Instant) -> Result<(), Error> {
        let origin = now
            .checked_add(self.lead)
            .ok_or(Error::Protocol("playback clock overflow"))?;
        self.anchor = Some((timestamp_us, origin));
        Ok(())
    }
    /// 返回媒体帧目标时间；时间倒退或溢出返回错误。
    pub fn target(&self, timestamp_us: u64) -> Result<Instant, Error> {
        let (base, origin) = self
            .anchor
            .ok_or(Error::Protocol("playback clock unanchored"))?;
        origin
            .checked_add(Duration::from_micros(
                timestamp_us
                    .checked_sub(base)
                    .ok_or(Error::Protocol("playback timeline reversal"))?,
            ))
            .ok_or(Error::Protocol("playback clock overflow"))
    }
}

/// 单视频GOP缓存上限：字节额度仅计排队负载；帧槽包含解码中未确认的帧。
#[derive(Clone, Copy)]
pub struct GopLimits {
    pub bytes: usize,
    pub frames: usize,
}
impl Default for GopLimits {
    fn default() -> Self {
        Self {
            bytes: 4 * 1024 * 1024,
            frames: 32,
        }
    }
}
/// 入队结果；ResetDecoder要求调用方取消旧解码任务并重建解码器。
#[derive(Debug, PartialEq, Eq)]
pub enum GopAdmission {
    Expired,
    Accepted,
    ResetDecoder,
    Stale,
    WaitingKey,
}
/// 最新GOP优先的有界单视频缓存；旧组到达不能推翻新的关键帧。
/// 不读取网络流；订阅端必须在分配负载之前另设接收/组并发边界。
pub struct GopScheduler {
    catalog: Catalog,
    agreement: Agreement,
    track: u16,
    limits: GopLimits,
    buffer: DecodeBuffer,
    key: Option<(u64, u64)>,
    waiting: bool,
}
impl GopScheduler {
    /// 创建单视频调度器，禁止无界缓存；目录按协商结果校验。
    pub fn new(
        catalog: Catalog,
        agreement: &Agreement,
        track: u16,
        limits: GopLimits,
    ) -> Result<Self, Error> {
        if limits.bytes == 0
            || limits.bytes > 8 * 1024 * 1024
            || limits.frames == 0
            || limits.frames > 128
            || !catalog
                .tracks
                .iter()
                .any(|t| t.id == track && t.kind == TrackKind::Video)
        {
            return Err(Error::Protocol("GOP limits or track"));
        }
        let buffer = DecodeBuffer::new(catalog.clone(), agreement, limits.bytes, limits.frames)?;
        Ok(Self {
            catalog,
            agreement: agreement.clone(),
            track,
            limits,
            buffer,
            key: None,
            waiting: true,
        })
    }
    /// 先校验再决定是否丢弃；新关键帧原子替换旧GOP，过大帧失败不清空旧状态。
    pub fn push(
        &mut self,
        frame: Frame,
        now: Instant,
        deadline: Instant,
    ) -> Result<GopAdmission, Error> {
        frame.validate(&self.catalog)?;
        if frame.track != self.track {
            return Err(Error::Protocol("GOP track mismatch"));
        }
        if deadline <= now {
            return Ok(GopAdmission::Expired);
        }
        if let Some((key, pts)) = self.key {
            if frame.sequence < key || (frame.sequence == key && frame.kind == FrameKind::Key) {
                return Ok(GopAdmission::Stale);
            }
            if frame.timestamp_us < pts {
                return Err(Error::Protocol("GOP timeline reversal"));
            }
        }
        if frame.kind == FrameKind::Key {
            let point = (frame.sequence, frame.timestamp_us);
            let mut replacement = DecodeBuffer::new(
                self.catalog.clone(),
                &self.agreement,
                self.limits.bytes,
                self.limits.frames,
            )?;
            replacement.push(frame, now, deadline)?;
            self.buffer = replacement;
            self.key = Some(point);
            self.waiting = false;
            return Ok(GopAdmission::ResetDecoder);
        }
        if self.waiting {
            return Ok(GopAdmission::WaitingKey);
        }
        if self
            .key
            .is_some_and(|(key, _)| frame.dependencies.iter().any(|dep| *dep < key))
        {
            return Err(Error::Protocol("cross-GOP dependency"));
        }
        self.buffer.push(frame, now, deadline)?;
        Ok(GopAdmission::Accepted)
    }
    /// 只在依赖已由解码器确认后输出Decode；超时恢复不会继续放行依赖帧。
    pub fn next(&mut self, now: Instant) -> Option<Event> {
        let event = self.buffer.next(now)?;
        if matches!(event, Event::Recover { .. }) {
            self.waiting = true;
        }
        Some(event)
    }
    /// 必须在实际解码成功后调用；旧GOP确认不影响新的GOP。
    pub fn decoded(&mut self, sequence: u64, now: Instant) -> Result<(), Error> {
        let result = self.buffer.decoded(sequence, now);
        if matches!(&result, Err(Error::Protocol("late decode confirmation"))) {
            self.waiting = true;
        }
        result
    }
    /// 当前排队负载字节，不含解码器拥有的帧负载。
    pub fn queued_bytes(&self) -> usize {
        self.buffer.queued_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Catalog, Agreement) {
        let h = peri_types::Hello {
            version: peri_types::PERI_VERSION,
            profiles: vec![peri_types::Profile::Live],
            codecs: vec!["video/av1".into()],
            extensions: vec![],
        };
        (
            Catalog {
                tracks: vec![Track {
                    id: 1,
                    kind: TrackKind::Video,
                    codec: "video/av1".into(),
                    init: vec![],
                    width: Some(320),
                    height: Some(180),
                    sample_rate: None,
                    channels: None,
                }],
            },
            peri_session::negotiate(&h, &h).unwrap(),
        )
    }
    fn f(seq: u64, key: bool) -> Frame {
        Frame {
            track: 1,
            sequence: seq,
            timestamp_us: seq * 66667,
            duration_us: 66667,
            kind: if key {
                FrameKind::Key
            } else {
                FrameKind::Reference
            },
            dependencies: if key { vec![] } else { vec![seq - 1] },
            payload: vec![42; 10],
        }
    }
    #[test]
    fn latest_gop_replaces_inflight_and_ignores_old_groups() {
        let (c, a) = fixture();
        let mut s = GopScheduler::new(c, &a, 1, GopLimits::default()).unwrap();
        let t = Instant::now();
        let d = t + Duration::from_millis(200);
        assert_eq!(s.push(f(2, false), t, d).unwrap(), GopAdmission::WaitingKey);
        s.push(f(1, true), t, d).unwrap();
        assert!(matches!(s.next(t), Some(Event::Decode(_))));
        assert_eq!(
            s.push(f(3, true), t, d).unwrap(),
            GopAdmission::ResetDecoder
        );
        assert!(s.decoded(1, t).is_err());
        assert_eq!(s.push(f(1, true), t, d).unwrap(), GopAdmission::Stale);
        s.push(f(4, false), t, d).unwrap();
        s.next(t);
        assert!(s.next(t).is_none());
        s.decoded(3, t).unwrap();
        assert!(matches!(
            s.next(t),
            Some(Event::Decode(Frame { sequence: 4, .. }))
        ));
    }
    #[test]
    fn overflow_and_expired_key_do_not_replace_good_gop() {
        let (c, a) = fixture();
        let mut s = GopScheduler::new(
            c,
            &a,
            1,
            GopLimits {
                bytes: 10,
                frames: 2,
            },
        )
        .unwrap();
        let t = Instant::now();
        let d = t + Duration::from_millis(100);
        s.push(f(1, true), t, d).unwrap();
        let mut big = f(3, true);
        big.payload.push(1);
        assert!(s.push(big, t, d).is_err());
        assert_eq!(s.push(f(3, true), t, t).unwrap(), GopAdmission::Expired);
        assert_eq!(s.queued_bytes(), 10);
        assert!(matches!(
            s.next(t),
            Some(Event::Decode(Frame { sequence: 1, .. }))
        ));
        assert!(s.decoded(1, d).is_err());
        assert_eq!(
            s.push(f(2, false), d, d + Duration::from_millis(100))
                .unwrap(),
            GopAdmission::WaitingKey
        );
    }
    #[test]
    fn reordered_dependencies_wait_for_actual_decode_and_expiry_recovers() {
        let (c, a) = fixture();
        let mut s = GopScheduler::new(c, &a, 1, GopLimits::default()).unwrap();
        let t = Instant::now();
        let d = t + Duration::from_millis(100);
        s.push(f(1, true), t, d).unwrap();
        s.push(f(3, false), t, d).unwrap();
        s.push(f(2, false), t, d).unwrap();
        s.next(t);
        s.decoded(1, t).unwrap();
        assert!(matches!(
            s.next(t),
            Some(Event::Decode(Frame { sequence: 2, .. }))
        ));
        assert!(s.next(t).is_none());
        s.decoded(2, t).unwrap();
        assert!(matches!(
            s.next(t),
            Some(Event::Decode(Frame { sequence: 3, .. }))
        ));
        assert!(matches!(s.next(d), Some(Event::Recover { track: 1 })));
        assert_eq!(
            s.push(f(4, false), d, d + Duration::from_millis(100))
                .unwrap(),
            GopAdmission::WaitingKey
        );
    }
    #[test]
    fn clock_requires_explicit_rebase_and_rejects_reversal() {
        let t = Instant::now();
        let mut c = PlaybackClock::new(Duration::from_millis(120)).unwrap();
        assert!(c.target(0).is_err());
        c.rebase(1000, t).unwrap();
        assert_eq!(c.target(11000).unwrap(), t + Duration::from_millis(130));
        assert!(c.target(999).is_err());
        c.rebase(20000, t + Duration::from_secs(1)).unwrap();
        assert_eq!(c.target(20000).unwrap(), t + Duration::from_millis(1120));
    }
}
