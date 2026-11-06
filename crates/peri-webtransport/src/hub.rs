//! One authenticated PERI subscription per resource, shared across both browser transports.
//! 每个直播资源共享一个已鉴权的 PERI 订阅，网页客户端只持有有界广播游标。
use crate::{Error, config::WatchConfig};
use peri_native::{
    media::{Catalog, Frame, FrameKind, Subscriber},
    quinn,
};
use peri_types::{Hello, PERI_VERSION, Profile};
use std::{sync::Arc, time::Duration};
use tokio::sync::{broadcast, watch};
pub type Hub = watch::Receiver<Option<std::sync::Weak<Feed>>>;
pub type Announce = watch::Sender<Option<std::sync::Weak<Feed>>>;
#[derive(Default)]
struct Recent {
    frames: std::collections::VecDeque<Arc<Frame>>,
    bytes: usize,
    start: Option<std::time::Instant>,
}
pub struct Feed {
    catalog: Arc<Catalog>,
    sender: broadcast::Sender<Arc<Frame>>,
    recent: std::sync::Mutex<Recent>,
}
pub struct Cursor {
    live: broadcast::Receiver<Arc<Frame>>,
    cached: std::collections::VecDeque<Arc<Frame>>,
}
impl Cursor {
    pub async fn recv(&mut self) -> Result<Arc<Frame>, broadcast::error::RecvError> {
        if let Some(f) = self.cached.pop_front() {
            return Ok(f);
        }
        self.live.recv().await
    }
}
impl Feed {
    pub fn announce(catalog: Arc<Catalog>, tx: &Announce) -> Arc<Self> {
        let (sender, _) = broadcast::channel(64);
        let feed = Arc::new(Self {
            catalog,
            sender,
            recent: Default::default(),
        });
        tx.send_replace(Some(Arc::downgrade(&feed)));
        feed
    }
    pub fn push(&self, f: Frame) {
        let f = Arc::new(f);
        let mut recent = self.recent.lock().expect("frame cache");
        if f.kind == FrameKind::Key {
            *recent = Recent {
                start: Some(std::time::Instant::now()),
                ..Recent::default()
            };
        }
        if let Some(start) = recent.start {
            if start.elapsed() <= Duration::from_millis(200)
                && recent.frames.len() < 32
                && recent.bytes + f.payload.len() <= 2 * 1024 * 1024
                && recent
                    .frames
                    .front()
                    .is_none_or(|key| f.timestamp_us.saturating_sub(key.timestamp_us) <= 200000)
            {
                recent.bytes += f.payload.len();
                recent.frames.push_back(f.clone());
            } else {
                *recent = Recent::default();
            }
        }
        // Attach and publish are atomic with respect to the hot GOP. No network I/O under this lock.
        // 缓存快照与实时游标在同把锁内衔接，锁内不做网络 I/O。
        let _ = self.sender.send(f);
    }
    fn attach(&self) -> Cursor {
        let recent = self.recent.lock().expect("frame cache");
        let cached = if recent
            .start
            .is_some_and(|t| t.elapsed() <= Duration::from_millis(200))
        {
            recent.frames.clone()
        } else {
            Default::default()
        };
        Cursor {
            cached,
            live: self.sender.subscribe(),
        }
    }
}
const BUDGET: Duration = Duration::from_secs(8);

pub fn start(cfg: WatchConfig) -> Hub {
    let (tx, rx) = watch::channel(None);
    tokio::spawn(async move {
        loop {
            let result = receive(&cfg, &tx).await;
            tx.send_replace(None);
            if tx.is_closed() {
                break;
            }
            if let Err(e) = result {
                eprintln!("PERI subscription: {e}");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    rx
}
async fn receive(cfg: &WatchConfig, hub: &Announce) -> Result<(), Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls_pki_types::CertificateDer::from(std::fs::read(
        &cfg.cert,
    )?))?;
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![peri_native::ALPN.to_vec()];
    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    )));
    let conn = tokio::time::timeout(
        BUDGET,
        endpoint.connect(([127, 0, 0, 1], cfg.udp).into(), &cfg.name)?,
    )
    .await??;
    let hello = Hello {
        version: PERI_VERSION,
        profiles: vec![Profile::Live],
        codecs: vec!["video/av1".into(), "audio/opus".into()],
        extensions: vec![],
    };
    let session =
        peri_native::subscribe_authenticated(conn, hello, &cfg.resource, &cfg.subscribe, BUDGET)
            .await?;
    let mut sub = Subscriber::new(session, BUDGET).await?;
    let catalog = Arc::new(sub.catalog().clone());
    if !catalog
        .tracks
        .iter()
        .any(|t| t.id == 1 && t.codec == "video/av1")
        || !catalog
            .tracks
            .iter()
            .any(|t| t.id == 2 && t.codec == "audio/opus")
    {
        return Err("unsupported program catalog".into());
    }
    let frames = Feed::announce(catalog, hub);
    sub.request_keyframe_after(1, 0).await?;
    let mut video = None;
    loop {
        let frame = sub.recv(BUDGET).await?;
        match frame.kind {
            FrameKind::Reference if !video.is_some_and(|id| frame.dependencies == [id]) => {
                if let Some(last) = video.take() {
                    sub.request_keyframe_after(1, last).await?;
                }
                continue;
            }
            FrameKind::Key | FrameKind::Reference => video = Some(frame.sequence),
            FrameKind::Audio if video.is_some() => (),
            _ => continue,
        }
        frames.push(frame);
    }
}
pub async fn join(mut hub: Hub) -> Result<(Arc<Catalog>, Cursor, Arc<Frame>), Error> {
    tokio::time::timeout(BUDGET, async {
        let feed = hub
            .wait_for(|f| f.is_some())
            .await?
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or("subscription ended")?;
        let catalog = feed.catalog.clone();
        let mut rx = feed.attach();
        drop(feed);
        loop {
            let f = rx.recv().await?;
            if f.kind == FrameKind::Key {
                return Ok((catalog, rx, f));
            }
        }
    })
    .await?
}
