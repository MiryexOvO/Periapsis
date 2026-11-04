//! 验收共用的实际解码与播放时钟路径，不是网络接收器或性能基准。
use peri_native::{
    media::{Catalog, Event, Frame, Track},
    playback::{Decoder, GopAdmission, GopLimits, GopScheduler, PlaybackClock},
};
use peri_types::Agreement;
use std::{
    fs::File,
    io::Write,
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error + Send + Sync>;
pub struct PlaybackCheck {
    scheduler: GopScheduler,
    clock: PlaybackClock,
    decoder: Option<Decoder>,
    track: Track,
    output: File,
    pub frames: usize,
}
impl PlaybackCheck {
    pub fn new(catalog: Catalog, agreement: &Agreement, path: String) -> Result<Self, Error> {
        let track = catalog.tracks[0].clone();
        Ok(Self {
            scheduler: GopScheduler::new(catalog, agreement, track.id, GopLimits::default())?,
            clock: PlaybackClock::new(Duration::from_millis(120))?,
            decoder: None,
            track,
            output: File::create(path)?,
            frames: 0,
        })
    }
    pub async fn render(&mut self, frame: Frame) -> Result<(), Error> {
        self.render_before(frame, Instant::now() + Duration::from_millis(300))
            .await
    }
    /// 实时接收帧沿用原始截止时间，不在调度器入口重置预算。
    pub async fn render_before(&mut self, frame: Frame, deadline: Instant) -> Result<(), Error> {
        let now = Instant::now();
        let admission = self.scheduler.push(frame, now, deadline)?;
        if admission == GopAdmission::ResetDecoder {
            if let Some(decoder) = self.decoder.as_mut() {
                decoder.stop().await;
            }
            self.decoder = Some(Decoder::new(&self.track).await?);
        }
        while let Some(event) = self.scheduler.next(Instant::now()) {
            match event {
                Event::Decode(frame) => {
                    if admission == GopAdmission::ResetDecoder {
                        self.clock.rebase(frame.timestamp_us, Instant::now())?;
                    }
                    let target = self.clock.target(frame.timestamp_us)?;
                    if target > Instant::now() + Duration::from_secs(1) {
                        return Err("fixture playback timeline gap".into());
                    }
                    let decoder = self.decoder.as_mut().ok_or("missing decoder")?;
                    eprintln!(
                        "SHARED_DECODE_START {} {}",
                        frame.sequence,
                        frame.payload.len()
                    );
                    tokio::time::timeout(Duration::from_millis(240), decoder.decode(&frame))
                        .await??;
                    eprintln!("SHARED_DECODE_DONE {}", frame.sequence);
                    self.scheduler.decoded(frame.sequence, Instant::now())?;
                    if target > Instant::now() {
                        tokio::time::sleep_until(target.into()).await;
                    }
                    self.output.write_all(decoder.pixels())?;
                    self.frames += 1;
                }
                _ => return Err("unexpected recovery in lossless fixture".into()),
            }
        }
        Ok(())
    }
}
