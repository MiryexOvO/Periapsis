//! One on-demand native encoder shared by all 720p viewers. / 所有 720p 观众共用一个按需启动的原生编码器。
use crate::Error;
use std::{
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    process::{Child, Command},
    sync::Mutex,
};
struct Encoder {
    child: Child,
    hub: crate::hub::Hub,
    dir: Arc<tempfile::TempDir>,
    base: u64,
    last: Instant,
}
#[derive(Clone)]
pub struct Mobile {
    inner: Arc<Mutex<Option<Encoder>>>,
    source: String,
}
pub struct Parts {
    dir: Arc<tempfile::TempDir>,
    pub base: u64,
    pub hub: crate::hub::Hub,
}
impl Mobile {
    pub fn new(port: u16) -> Self {
        let this = Self {
            inner: Default::default(),
            source: format!("http://127.0.0.1:{port}/live.webm"),
        };
        let inner = Arc::downgrade(&this.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(inner) = inner.upgrade() else { break };
                let mut state = inner.lock().await;
                if state.as_ref().is_some_and(|e| {
                    Arc::strong_count(&e.dir) == 1 && e.last.elapsed() > Duration::from_secs(30)
                }) {
                    if let Some(mut e) = state.take() {
                        let _ = e.child.kill().await;
                    }
                }
            }
        });
        this
    }
    pub async fn access(&self, start: bool) -> Result<Parts, Error> {
        let mut lock = self.inner.lock().await;
        if let Some(e) = lock.as_mut() {
            if e.child.try_wait()?.is_some() {
                *lock = None;
            }
        }
        if lock.is_none() {
            if !start {
                return Err("encoder idle".into());
            }
            let dir = Arc::new(tempfile::tempdir()?);
            let base = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64 * 4;
            // One encode, two muxers: native packets and LL-HLS share the bitrate ladder.
            // 一次编码同时供裸帧与 LL-HLS 两种出口使用。
            let tee = format!(
                "[f=hls:hls_time=0.25:hls_segment_type=fmp4:hls_fmp4_init_filename=init{base}.mp4:hls_list_size=48:hls_delete_threshold=16:hls_flags=delete_segments+independent_segments+temp_file+program_date_time:start_number={base}:hls_segment_filename=p%d.m4s]parts.m3u8|[f=flv:flvflags=no_duration_filesize]pipe:1"
            );
            let mut child=Command::new(std::env::var_os("PERI_FFMPEG").unwrap_or_else(||"ffmpeg".into()))
                .current_dir(dir.path()).stdin(Stdio::null()).stdout(Stdio::piped()).kill_on_drop(true)
                .args(["-nostdin","-hide_banner","-loglevel","error","-probesize","2M","-analyzeduration","500000","-i",&self.source,
                    "-map","0:v:0","-map","0:a:0","-vf","fps=60,scale=w='min(1280,iw)':h='min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
                    "-c:v","libx264","-preset","veryfast","-tune","zerolatency","-flags","+global_header","-bf","0","-pix_fmt","yuv420p","-b:v","3200k","-maxrate","3600k","-bufsize","3600k","-g","15","-sc_threshold","0",
                    "-c:a","aac","-b:a","96k","-ar","48000","-ac","2","-f","tee",&tee]).spawn()?;
            let hub =
                crate::low_frames::spawn(child.stdout.take().ok_or("missing encoder output")?);
            *lock = Some(Encoder {
                hub,
                child,
                dir,
                base,
                last: Instant::now(),
            });
        }
        let e = lock.as_mut().unwrap();
        e.last = Instant::now();
        Ok(Parts {
            hub: e.hub.clone(),
            dir: e.dir.clone(),
            base: e.base,
        })
    }
}
struct Part {
    index: u64,
    duration: f64,
    date: String,
}
impl Parts {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    async fn index(&self) -> Result<Vec<Part>, Error> {
        let text = tokio::fs::read_to_string(self.path("parts.m3u8")).await?;
        let mut list = Vec::new();
        let mut duration = 0.;
        let mut date = String::new();
        for line in text.lines() {
            if let Some(d) = line.strip_prefix("#EXTINF:") {
                duration = d.trim_end_matches(',').parse()?;
            } else if line.starts_with("#EXT-X-PROGRAM-DATE-TIME:") {
                date = line.into();
            } else if let Some(n) = number(line, "p", ".m4s") {
                list.push(Part {
                    index: n,
                    duration,
                    date: std::mem::take(&mut date),
                });
            }
        }
        Ok(list)
    }
    pub async fn manifest(
        &self,
        msn: Option<u64>,
        part: Option<u64>,
    ) -> Result<(String, u64), Error> {
        let end = Instant::now() + Duration::from_secs(9);
        loop {
            if let Ok(list) = self.index().await
                && let (Some(first), Some(last)) = (list.first(), list.last())
            {
                let seq = first.index.div_ceil(4);
                if msn.is_some_and(|m| m > last.index / 4 + 2) {
                    return Err("future sequence".into());
                }
                if msn.is_none_or(|m| {
                    last.index >= m.saturating_mul(4).saturating_add(part.unwrap_or(3).min(4))
                }) {
                    let mut text = format!(
                        "#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-TARGETDURATION:1\n#EXT-X-PART-INF:PART-TARGET=0.250\n#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=1.000\n#EXT-X-MEDIA-SEQUENCE:{seq}\n#EXT-X-MAP:URI=\"init{}.mp4\"\n#EXT-X-INDEPENDENT-SEGMENTS\n",
                        self.base
                    );
                    let mut total = 0.;
                    for p in list.iter().filter(|p| p.index >= seq * 4) {
                        if p.index % 4 == 0 && !p.date.is_empty() {
                            text.push_str(&p.date);
                            text.push('\n');
                        }
                        total += p.duration;
                        if p.index / 4 + 3 >= last.index / 4 {
                            text.push_str(&format!(
                                "#EXT-X-PART:DURATION={:.6},URI=\"p{}.m4s\",INDEPENDENT=YES\n",
                                p.duration, p.index
                            ));
                        }
                        if p.index % 4 == 3 {
                            text.push_str(&format!("#EXTINF:{total:.6},\ns{}.m4s\n", p.index / 4));
                            total = 0.;
                        }
                    }
                    text.push_str(&format!(
                        "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"p{}.m4s\"\n",
                        last.index + 1
                    ));
                    return Ok((text, last.index));
                }
            }
            if Instant::now() >= end {
                return Err("720p encoder unavailable".into());
            }
            tokio::time::sleep(Duration::from_millis(35)).await;
        }
    }
    pub async fn file(&self, name: &str) -> Result<Vec<u8>, Error> {
        if name == format!("init{}.mp4", self.base) {
            return Ok(tokio::fs::read(self.path(name)).await?);
        }
        if let Some(n) = number(name, "s", ".m4s") {
            let start = n
                .checked_mul(4)
                .filter(|v| *v <= u64::MAX - 4)
                .ok_or("sequence overflow")?;
            let mut data = Vec::new();
            for i in start..start + 4 {
                data.extend(tokio::fs::read(self.path(&format!("p{i}.m4s"))).await?);
            }
            return Ok(data);
        }
        let n = number(name, "p", ".m4s").ok_or("unknown fragment")?;
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(bytes) = tokio::fs::read(self.path(name)).await {
                return Ok(bytes);
            }
            if Instant::now() >= end {
                return Err("fragment unavailable".into());
            }
            if let Ok(list) = self.index().await
                && let (Some(first), Some(last)) = (list.first(), list.last())
                && (n < first.index || n > last.index + 1)
            {
                return Err("fragment outside window".into());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
fn number(s: &str, prefix: &str, suffix: &str) -> Option<u64> {
    let n = s.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if n.is_empty() || n.len() > 18 || !n.bytes().all(|v| v.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}
