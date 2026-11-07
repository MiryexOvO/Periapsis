//! Real OBS recovery probe; discards one reference locally and records a fresh GOP.
//! 真实 OBS 恢复探针：本地舍弃一个参考帧，记录恢复后的独立 GOP。
use peri_native::{
    av1::IvfWriter,
    media::{FrameKind, Subscriber},
    quinn,
};
use peri_types::{Hello, PERI_VERSION, Profile};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error + Send + Sync>;
const BUDGET: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: obs_recovery address cert resource output.ivf".into());
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add(std::fs::read(&args[1])?.into())?;
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![peri_native::ALPN.to_vec()];
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    )));
    let conn =
        tokio::time::timeout(BUDGET, endpoint.connect(args[0].parse()?, "localhost")?).await??;
    let hello = Hello {
        version: PERI_VERSION,
        profiles: vec![Profile::Live],
        codecs: vec!["video/av1".into(), "audio/opus".into()],
        extensions: vec![],
    };
    let session = peri_native::subscribe_authenticated(
        conn,
        hello,
        &args[2],
        &std::env::var("PERI_SUBSCRIBE_TOKEN")?,
        BUDGET,
    )
    .await?;
    let mut sub = Subscriber::new(session, BUDGET).await?;
    let track = sub
        .catalog()
        .tracks
        .iter()
        .find(|t| t.id == 1)
        .ok_or("video missing")?
        .clone();
    let mut count = 0;
    let mut last_key_pts = 0;
    let lost = loop {
        let frame = sub.recv(BUDGET).await?;
        if frame.track != 1 {
            continue;
        }
        if frame.kind == FrameKind::Key {
            last_key_pts = frame.timestamp_us;
        }
        count += 1;
        // Wait away from startup, then break the local decoder's reference chain.
        // 远离启动时刻后模拟本地参考链丢失，避免将启动关键帧误判为恢复。
        if count >= 90 && frame.kind == FrameKind::Reference {
            break frame;
        }
    };
    // Negative control: an intact connection must not spontaneously emit a key here.
    // 阴性对照：先观察一秒，确认当前时段没有周期或其他自发关键帧。
    let quiet = Instant::now();
    while quiet.elapsed() < Duration::from_secs(1) {
        let frame = sub.recv(BUDGET).await?;
        if frame.track == 1 && frame.kind == FrameKind::Key {
            return Err("unexpected keyframe during no-request control".into());
        }
    }
    let request = peri_native::RecoveryRequest::after(1, 0, lost.sequence);
    let start = Instant::now();
    sub.forward_recovery(request).await?;
    let recovered = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let frame = sub.recv(BUDGET).await?;
            if frame.track == 1 && frame.kind == FrameKind::Key && frame.sequence > lost.sequence {
                return Ok::<_, Error>(frame);
            }
        }
    })
    .await??;
    let latency = start.elapsed().as_millis();
    if recovered.timestamp_us - last_key_pts >= 10_000_000 {
        return Err("keyframe was not ahead of the 30-second periodic interval".into());
    }
    let sequence = recovered.sequence;
    let mut writer = IvfWriter::new(std::fs::File::create(&args[3])?, track)?;
    writer.push(&recovered)?;
    let mut recorded = 1;
    while recorded < 30 {
        let frame = sub.recv(BUDGET).await?;
        if frame.track == 1 {
            writer.push(&frame)?;
            recorded += 1;
        }
    }
    writer.finish()?;
    println!(
        "OBS_RECOVERY_RESULT {}",
        serde_json::json!({"origin":request.origin,
        "id":request.id,"after_sequence":lost.sequence,"key_sequence":sequence,
        "receive_ms":latency,"recorded_frames":recorded,"periodic_keyint_seconds":30,
        "no_request_control_ms":1000})
    );
    endpoint.close(0u32.into(), b"probe finished");
    Ok(())
}
