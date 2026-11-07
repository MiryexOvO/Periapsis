//! Native browser gateway: PERI over Quinn in, WebTransport/HTTP out.
//! 原生分发网关：Quinn 上订阅 PERI，通过 WebTransport 或 HTTP 向观众交付。
mod config;
mod http_server;
mod hub;
mod low_frames;
mod mobile;
mod webm;
use config::Config;
use peri_native::media::{Frame, FrameKind};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Semaphore, time::timeout};
use web_transport_quinn::{Request, ServerBuilder};
type Error = Box<dyn std::error::Error + Send + Sync>;
const LIMIT: Duration = Duration::from_secs(5);

async fn raw_track(
    mut out: web_transport_quinn::SendStream,
    track: peri_native::media::Track,
    mut rx: hub::Cursor,
    first: Arc<Frame>,
) -> Result<(), Error> {
    let header = serde_json::to_vec(&track)?;
    out.write_all(&(header.len() as u32).to_be_bytes()).await?;
    out.write_all(&header).await?;
    let mut f = first;
    loop {
        if f.track == track.id {
            // Length, key flag, media PTS and duration, followed by directly decodable bytes.
            // 长度、关键标志、媒体时间戳、时长后紧跟可直接解码的媒体负载。
            let mut h = [0; 17];
            h[..4].copy_from_slice(&(f.payload.len() as u32).to_be_bytes());
            h[4] = u8::from(f.kind != FrameKind::Reference);
            h[5..13].copy_from_slice(&f.timestamp_us.to_be_bytes());
            h[13..].copy_from_slice(&f.duration_us.to_be_bytes());
            timeout(LIMIT, async {
                out.write_all(&h).await?;
                out.write_all(&f.payload).await
            })
            .await??;
            if f.kind == FrameKind::Key {
                peri_native::trace(
                    "gateway_key_write",
                    serde_json::json!({"sequence":f.sequence,"pts_us":f.timestamp_us,"codec":track.codec,"bytes":f.payload.len()}),
                );
            }
        }
        f = timeout(LIMIT, rx.recv()).await??;
    }
}
async fn forward(
    request: Request,
    hub: hub::Hub,
    mobile: mobile::Mobile,
    origin: &str,
) -> Result<(), Error> {
    if request.headers.get("origin").and_then(|h| h.to_str().ok()) != Some(origin) {
        request.reject(http::StatusCode::FORBIDDEN).await?;
        return Ok(());
    }
    let low = match request.url.path() {
        "/source" => false,
        "/720" => true,
        _ => {
            request.reject(http::StatusCode::NOT_FOUND).await?;
            return Ok(());
        }
    };
    let session = request.ok().await?;
    let pump = async {
        let rendition = if low {
            Some(mobile.access(true).await?)
        } else {
            None
        };
        let source = rendition.as_ref().map_or(hub, |r| r.hub.clone());
        let ((catalog, rx, first), (_, audio, audio_first)) =
            tokio::try_join!(hub::join(source.clone()), hub::join(source))?;
        // Subscribe before waiting on stream credit, preserving the GOP at join time.
        // 等待开流额度前复制游标，以保留接入时的关键帧依赖链。
        let track = |id| {
            catalog
                .tracks
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or("missing track")
        };

        let (v, a) = tokio::try_join!(session.open_uni(), session.open_uni())?;
        tokio::try_join!(
            raw_track(v, track(1)?, rx, first.clone()),
            raw_track(a, track(2)?, audio, audio_first)
        )?;
        Ok::<(), Error>(())
    };
    tokio::select! {r=pump=>r,_=session.closed()=>Ok(())}
}
#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let action = args.first().map(String::as_str).unwrap_or("watch");
    if action == "--help" {
        println!(
            "peri-webtransport setup | configure | relay | watch | obs-token [--public-ip IP] [--public-port PORT] | watch-port | stream-key | link | viewer-config"
        );
        return Ok(());
    }
    let path = Config::path()?;
    if action != "watch" {
        return config::manage(action, &args[1..], &path);
    }
    let cfg = config::WatchConfig::load(&path)?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", cfg.tcp)).await?;
    let port = listener.local_addr()?.port();
    let mobile = mobile::Mobile::new(port);
    let hub = hub::start(cfg);
    println!("PERI_WEB_READY {port}");
    let web = http_server::serve(listener, hub.clone(), mobile.clone());
    let Ok(bind) = std::env::var("PERI_WT_BIND") else {
        return web.await;
    };
    let chain = CertificateDer::pem_file_iter(std::env::var("PERI_WT_CERT")?)?
        .collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(std::env::var("PERI_WT_KEY")?)?;
    let origin = Arc::new(std::env::var("PERI_PAGE_ORIGIN")?);
    let mut server = ServerBuilder::new()
        .with_addr(bind.parse()?)
        .with_certificate(chain, key)?;
    println!("PERI_WT_READY {}", server.local_addr()?);
    let slots = Arc::new(Semaphore::new(128));
    let wt = async {
        while let Some(request) = server.accept().await {
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                request
                    .reject(http::StatusCode::SERVICE_UNAVAILABLE)
                    .await?;
                continue;
            };
            let (hub, mobile, origin) = (hub.clone(), mobile.clone(), origin.clone());
            tokio::spawn(async move {
                let _slot = permit;
                if let Err(e) = forward(request, hub, mobile, &origin).await {
                    eprintln!("WT viewer ended: {e}");
                }
            });
        }
        Ok::<(), Error>(())
    };
    tokio::try_join!(wt, web)?;
    Ok(())
}
