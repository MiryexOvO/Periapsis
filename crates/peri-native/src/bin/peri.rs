//! Separate-process native PERI relay, file publisher and recorder.
//! 可分进程运行的原生 PERI Relay、文件发布与录制命令。

#[path = "peri/live.rs"]
mod live;
#[path = "peri/playback.rs"]
mod playback;

use peri_native::{
    ALPN,
    auth::Credentials,
    av1::{IvfWriter, read_all_intra_ivf, read_gop_ivf},
    media::{Publisher, Subscriber},
    quinn,
    relay::{Limits, Relay},
};
use peri_types::{Hello, PERI_VERSION, Profile};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinSet};

type Error = Box<dyn std::error::Error + Send + Sync>;
const SETUP: Duration = Duration::from_secs(5);
fn hello() -> Hello {
    Hello {
        version: PERI_VERSION,
        profiles: vec![Profile::Live],
        codecs: vec!["video/av1".into(), "audio/opus".into()],
        extensions: vec![],
    }
}
fn read_bounded(path: &str, limit: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("input file exceeds limit".into());
    }
    Ok(bytes)
}
fn transport() -> Result<Arc<quinn::TransportConfig>, Error> {
    let mut config = quinn::TransportConfig::default();
    // 默认沿用同优先级流轮转；候选0按写入顺序集中发送，减少参考帧负载交错。
    // 仅影响相同优先级；独立对象的较高优先级、拥塞控制和截止时间保持有效。
    let fairness = match std::env::var("PERI_SEND_FAIRNESS").as_deref() {
        Ok("0") => false,
        Ok("1") | Err(_) => true,
        _ => return Err("PERI_SEND_FAIRNESS must be 0 or 1".into()),
    };
    config.send_fairness(fairness);
    // 仅请求对端调整确认频率；不支持扩展的对端会忽略，不能冒充已协商成功。
    match std::env::var("PERI_ACK_MODE").as_deref() {
        Ok("frequent") => {
            let mut ack = quinn::AckFrequencyConfig::default();
            ack.ack_eliciting_threshold(0u32.into())
                .max_ack_delay(Some(Duration::from_millis(1)))
                .reordering_threshold(1u32.into());
            config.ack_frequency_config(Some(ack));
        }
        Ok("default") | Err(_) => {}
        _ => return Err("PERI_ACK_MODE must be default or frequent".into()),
    }
    config.max_concurrent_bidi_streams(1_u32.into());
    config.max_concurrent_uni_streams(8_u32.into());
    config.stream_receive_window((1024 * 1024 + 128_u32).into());
    config.receive_window((8 * 1024 * 1024_u32).into());
    config.max_idle_timeout(Some(Duration::from_secs(10).try_into()?));
    config.keep_alive_interval(Some(Duration::from_secs(2)));
    Ok(Arc::new(config))
}
fn server_endpoint(bind: SocketAddr, cert: &str, key: &str) -> Result<quinn::Endpoint, Error> {
    let cert = rustls::pki_types::CertificateDer::from(read_bounded(cert, 65536)?);
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(read_bounded(key, 65536)?);
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
    ));
    config.transport_config(transport()?);
    Ok(quinn::Endpoint::server(config, bind)?)
}
async fn connect(
    address: &str,
    name: &str,
    trust: &str,
) -> Result<(quinn::Endpoint, quinn::Connection), Error> {
    let address: SocketAddr = address.parse()?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(read_bounded(
        trust, 65536,
    )?))?;
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    ));
    config.transport_config(transport()?);
    let mut endpoint = quinn::Endpoint::client(
        if address.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        }
        .parse()?,
    )?;
    endpoint.set_default_client_config(config);
    let connection = tokio::time::timeout(SETUP, endpoint.connect(address, name)?).await??;
    Ok((endpoint, connection))
}

fn init(directory: &str) -> Result<(), Error> {
    let directory = Path::new(directory);
    std::fs::create_dir_all(directory)?;
    let cert_path = directory.join("cert.der");
    let key_path = directory.join("key.key");
    if cert_path.try_exists()? || key_path.try_exists()? {
        return Err("certificate output already exists".into());
    }
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(key_path)?
        .write_all(&cert.key_pair.serialize_der())?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(cert_path)?
        .write_all(cert.cert.der())?;
    println!("PERI_CERT_READY");
    Ok(())
}

#[derive(Default)]
struct Slot {
    publishing: bool,
    relay: Option<Arc<Relay>>,
}
struct Lease(Arc<Mutex<Slot>>);
impl Lease {
    fn claim(slot: Arc<Mutex<Slot>>) -> Result<Self, Error> {
        {
            let mut state = slot.lock().expect("resource lock");
            if state.publishing {
                return Err("publisher already active".into());
            }
            state.publishing = true;
        }
        Ok(Self(slot))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut slot = self.0.lock().expect("resource lock");
        if let Some(relay) = slot.relay.take() {
            relay.close();
        }
        slot.publishing = false;
        println!("PERI_RESOURCE_ENDED");
    }
}
async fn handle(
    connection: quinn::Connection,
    credentials: Arc<Credentials>,
    resource: Arc<str>,
    slot: Arc<Mutex<Slot>>,
) -> Result<(), Error> {
    let session =
        peri_native::accept_authenticated(connection, hello(), &resource, &credentials, SETUP)
            .await?;
    if session.can_send() {
        let relay = slot
            .lock()
            .expect("resource lock")
            .relay
            .clone()
            .ok_or("resource not live")?;
        relay.serve(session, SETUP).await?;
    } else {
        let _lease = Lease::claim(slot.clone())?;
        let source = Subscriber::new(session, SETUP).await?;
        let relay = Arc::new(Relay::new(source.catalog().clone(), Limits::default())?);
        slot.lock().expect("resource lock").relay = Some(relay.clone());
        println!("PERI_RESOURCE_READY");
        relay.ingest(source, SETUP).await?;
    }
    Ok(())
}
async fn relay(args: &[String]) -> Result<(), Error> {
    let credentials = Arc::new(Credentials::new(
        std::env::var("PERI_PUBLISH_TOKEN")?,
        std::env::var("PERI_SUBSCRIBE_TOKEN")?,
    )?);
    let endpoint = server_endpoint(args[0].parse()?, &args[1], &args[2])?;
    let resource: Arc<str> = args[3].as_str().into();
    if resource.is_empty()
        || resource.len() > 256
        || !resource
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte))
    {
        return Err("invalid resource".into());
    }
    let slot = Arc::new(Mutex::new(Slot::default()));
    let permits = Arc::new(Semaphore::new(16));
    let mut tasks = JoinSet::new();
    println!("PERI_RELAY_READY {}", endpoint.local_addr()?);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break; };
                let permit = permits.clone().try_acquire_owned();
                if permit.is_err() || tasks.len() >= 32 { incoming.refuse(); continue; }
                let credentials = credentials.clone(); let resource = resource.clone(); let slot = slot.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let result = async { let connection = tokio::time::timeout(SETUP, async { incoming.await }).await??; handle(connection, credentials, resource, slot).await }.await;
                    // 不记录控制消息解析内容，避免将凭据或恶意字符串写入日志。
                    if result.is_err() { eprintln!("PERI connection ended"); }
                });
            }
        }
    }
    endpoint.close(0_u32.into(), b"server stopped");
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    println!("PERI_RELAY_STOPPED");
    Ok(())
}

async fn publish(args: &[String]) -> Result<(), Error> {
    if args[5..]
        .iter()
        .filter(|arg| *arg == "--all-intra" || *arg == "--gop")
        .count()
        != 1
        || args[5..]
            .iter()
            .any(|arg| arg != "--all-intra" && arg != "--gop" && arg != "--loop")
    {
        return Err("publish requires exactly one of --all-intra / --gop; optional --loop".into());
    }
    let looping = args[5..].iter().any(|arg| arg == "--loop");
    let bytes = read_bounded(&args[4], 16 * 1024 * 1024)?;
    let (catalog, frames) = if args[5..].iter().any(|arg| arg == "--gop") {
        let probe = playback::probe(&bytes).await?;
        read_gop_ivf(&bytes, &probe)?
    } else {
        read_all_intra_ivf(&bytes)?
    };
    let token = std::env::var("PERI_PUBLISH_TOKEN")?;
    let (_endpoint, connection) = connect(&args[0], &args[1], &args[2]).await?;
    let session =
        peri_native::publish_authenticated(connection, hello(), &args[3], &token, SETUP).await?;
    let publisher = Publisher::new(session, catalog, SETUP).await?;
    println!("PERI_PUBLISH_READY");
    let operation = async {
        let started = tokio::time::Instant::now();
        let mut output_time = 0_u64;
        let mut cycle = 0_u64;
        loop {
            let mut index = 0;
            while index < frames.len() {
                if publisher.poll_keyframe_request()?.is_some() {
                    if let Some(next) = frames[index..]
                        .iter()
                        .position(|f| f.kind == peri_native::media::FrameKind::Key)
                    {
                        index += next;
                    } else if looping {
                        break;
                    }
                }
                let mut frame = frames[index].clone();
                let sequence = cycle
                    .checked_mul(frames.len() as u64)
                    .and_then(|n| n.checked_add(frame.sequence))
                    .ok_or("sequence exhausted")?;
                let offset = sequence - frame.sequence;
                for dependency in &mut frame.dependencies {
                    *dependency = dependency
                        .checked_add(offset)
                        .ok_or("dependency exhausted")?;
                }
                frame.sequence = sequence;
                frame.timestamp_us = output_time;
                tokio::time::sleep_until(started + Duration::from_micros(output_time)).await;
                publisher.send(&frame, SETUP).await?;
                output_time = output_time
                    .checked_add(u64::from(frame.duration_us))
                    .ok_or("timeline exhausted")?;
                index += 1;
            }
            if !looping {
                // 留出下游本地驻留窗口的排空时间，不代表实际播放确认。
                tokio::time::sleep(Duration::from_millis(500)).await;
                break;
            }
            cycle = cycle.checked_add(1).ok_or("cycle exhausted")?;
        }
        Ok::<_, Error>(())
    };
    tokio::select! { result = operation => result?, _ = tokio::signal::ctrl_c() => {} }
    Ok(())
}
async fn receive(args: &[String]) -> Result<(), Error> {
    let count: u32 = args[5].parse()?;
    if count == 0 || count > 10000 {
        return Err("receive count must be 1..10000".into());
    }
    let token = std::env::var("PERI_SUBSCRIBE_TOKEN")?;
    let (_endpoint, connection) = connect(&args[0], &args[1], &args[2]).await?;
    let session =
        peri_native::subscribe_authenticated(connection, hello(), &args[3], &token, SETUP).await?;
    let mut subscriber = Subscriber::new(session, SETUP).await?;
    let track = subscriber
        .catalog()
        .tracks
        .iter()
        .find(|track| track.codec == "video/av1")
        .ok_or("no AV1 video track")?
        .clone();
    let id = track.id;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[4])?;
    let mut writer = IvfWriter::new(file, track)?;
    println!("PERI_RECEIVE_READY");
    let mut written = 0;
    // 同时限制总对象数，避免无视频的音频流让固定帧数录制无限等待。
    for _ in 0..count.saturating_mul(16) {
        let frame = subscriber.recv(SETUP).await?;
        if frame.track != id {
            continue;
        }
        writer.push(&frame)?;
        written += 1;
        if written == count {
            break;
        }
    }
    if written != count {
        return Err("video recording frame limit".into());
    }
    writer.finish()?;
    println!("PERI_RECEIVE_DONE {written}");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("init") if args.len() == 2 => init(&args[1]),
        Some("relay") if args.len() == 5 => relay(&args[1..]).await,
        Some("publish") if (7..=8).contains(&args.len()) => publish(&args[1..]).await,
        Some("live") if args.len() == 7 => live::run(&args[1..]).await,
        Some("receive") if args.len() == 7 => receive(&args[1..]).await,
        Some("play") if (6..=8).contains(&args.len()) => playback::play(&args[1..]).await,
        Some("--help") | None => {
            println!(
                "peri init DIR\nperi relay BIND CERT.der KEY.key RESOURCE\nperi publish ADDRESS SERVER_NAME TRUST.der RESOURCE INPUT.ivf (--all-intra|--gop) [--loop]\nperi receive ADDRESS SERVER_NAME TRUST.der RESOURCE OUTPUT.ivf COUNT\nperi play ADDRESS SERVER_NAME TRUST.der RESOURCE COUNT [--headless] [--fifo]\nperi live ADDRESS SERVER_NAME TRUST.der RESOURCE WORKER.py COUNT\nCredentials: PERI_PUBLISH_TOKEN / PERI_SUBSCRIBE_TOKEN"
            );
            Ok(())
        }
        _ => Err("invalid arguments; use --help".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publisher_lease_is_exclusive_and_released_on_drop() {
        let slot = Arc::new(Mutex::new(Slot::default()));
        let lease = Lease::claim(slot.clone()).unwrap();
        assert!(Lease::claim(slot.clone()).is_err());
        drop(lease);
        assert!(Lease::claim(slot).is_ok());
    }
}
