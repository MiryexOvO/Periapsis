//! Loopback WebTransport media gateway fed by a trusted, bounded stdin publisher.
//! 仅本机使用的 WebTransport 媒体网关，接收可信发布进程提供的有界输入。
use base64::{Engine, engine::general_purpose::STANDARD};
use peri_transport::moq;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

type Error = Box<dyn std::error::Error + Send + Sync>;
#[derive(Deserialize)]
struct Config {
    origin: String,
    token: String,
    catalog: String,
}
#[derive(Deserialize)]
struct Frame {
    track: String,
    payload: String,
    timestamp: u64,
    keyframe: bool,
}

async fn line(input: &mut BufReader<tokio::io::Stdin>) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    input
        .take(2 * 1024 * 1024)
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.last() != Some(&b'\n') {
        return Err("publisher ended or input exceeded limit".into());
    }
    Ok(bytes)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut input = BufReader::new(tokio::io::stdin());
    let config: Config = serde_json::from_slice(&line(&mut input).await?)?;
    if config.token.len() < 32 || !config.origin.starts_with("http://127.0.0.1:") {
        return Err("invalid local gateway config".into());
    }
    let key = rcgen::KeyPair::generate()?;
    let mut params = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])?;
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::minutes(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(7);
    let cert = params.self_signed(&key)?;
    let hash: String = Sha256::digest(cert.der().as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr("127.0.0.1:0".parse()?)
        .with_certificate(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )?;
    let (origin, driver) =
        moq::origin::Producer::new(moq::origin::Config::new(moq::Hop::new(1).unwrap()));
    let origin_task = tokio::spawn(moq::time::run(driver));
    let broadcast = origin.create_broadcast("peri/test-video")?;
    let catalog = broadcast.create_track("catalog", None)?;
    let track = broadcast.create_track("video", None)?;
    let audio_track = broadcast.create_track("audio", None)?;
    let bytes = STANDARD.decode(&config.catalog)?;
    if bytes.len() > 16384 {
        return Err("catalog too large".into());
    }
    let mut catalog_group = catalog.append_group()?;
    catalog_group.write_frame(moq::Timestamp::ZERO, bytes)?;
    catalog_group.finish()?;
    broadcast.announce(Default::default())?;
    println!(
        "{}",
        serde_json::json!({"url":format!("https://127.0.0.1:{}/moq",server.local_addr()?.port()),"hash":hash})
    );
    std::io::stdout().flush()?;
    let producer = tokio::spawn(async move {
        let mut groups: [Option<moq::group::Producer>; 2] = [None, None];
        loop {
            let message: Frame = serde_json::from_slice(&line(&mut input).await?)?;
            let bytes = STANDARD.decode(message.payload)?;
            if bytes.len() > 1024 * 1024 {
                return Err::<(), Error>("video object too large".into());
            }
            let (index, source) = match message.track.as_str() {
                "video" => (0, &track),
                "audio" => (1, &audio_track),
                _ => return Err("unknown media track".into()),
            };
            let group = &mut groups[index];
            if message.keyframe {
                if let Some(previous) = group.take() {
                    previous.finish()?;
                }
                *group = Some(source.append_group()?);
            }
            group
                .as_mut()
                .ok_or("video must start with keyframe")?
                .write_frame(moq::Timestamp::from_micros(message.timestamp)?, bytes)?;
        }
    });
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
    tokio::pin!(producer);
    loop {
        tokio::select! {
            result = &mut producer => { origin_task.abort(); return result?; }
            request = server.accept() => {
                let Some(request) = request else { return Ok(()); };
                let authorized = request.url.path() == "/moq" && request.url.query_pairs().any(|(k,v)|k == "token" && v == config.token)
                    && request.headers.get("origin").and_then(|v|v.to_str().ok()) == Some(config.origin.as_str())
                    && request.protocols.iter().any(|p|p == "moqt-22");
                let permit = permits.clone().try_acquire_owned();
                if !authorized || permit.is_err() { request.reject(web_transport_quinn::http::StatusCode::FORBIDDEN).await?; continue; }
                let origin = origin.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = async {
                        let transport = request.respond(web_transport_quinn::proto::ConnectResponse::OK.with_protocol("moqt-22")).await?;
                        let session = peri_transport::accept_webtransport(transport, &origin).await?;
                        session.inner.closed().await;
                        Ok::<(), Error>(())
                    }.await;
                    if let Err(error) = result { eprintln!("WebTransport session failed: {error}"); }
                });
            }
        }
    }
}
