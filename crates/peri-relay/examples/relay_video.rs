//! Reproducible AV1 encode -> native publish -> relay -> two receivers -> decode check.
//! 可复现 AV1 编码、原生发布、双接收端转发与解码校验，仅用于本机验证。

#[path = "../../peri-native/examples/support/mod.rs"]
mod support;
use peri_native::{
    ALPN,
    media::{Catalog, Frame, Publisher, Subscriber},
};
use peri_relay::{Limits, Relay};
use peri_types::{Hello, PERI_VERSION, Profile};
use std::{path::Path, process::Command, sync::Arc, time::Duration};

type Error = Box<dyn std::error::Error + Send + Sync>;
const BUDGET: Duration = Duration::from_secs(5);
fn hello() -> Hello {
    Hello {
        version: PERI_VERSION,
        profiles: vec![Profile::Live],
        codecs: vec!["video/av1".into()],
        extensions: vec![],
    }
}

fn ffmpeg(args: &[&str]) -> Result<Vec<u8>, Error> {
    let mut command =
        Command::new(std::env::var_os("PERI_FFMPEG").unwrap_or_else(|| "ffmpeg".into()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.args(args).output()?;
    if !output.status.success() {
        return Err(format!("FFmpeg failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(output.stdout)
}

fn write_received_ivf(path: &Path, catalog: &Catalog, frames: &[Frame]) -> Result<(), Error> {
    let mut bytes = b"DKIF\x00\x00\x20\x00AV01".to_vec();
    bytes.extend_from_slice(
        &catalog.tracks[0]
            .width
            .ok_or("missing width")?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &catalog.tracks[0]
            .height
            .ok_or("missing height")?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&1_000_000_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    for frame in frames {
        bytes.extend_from_slice(&(frame.payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&frame.timestamp_us.to_le_bytes());
        bytes.extend_from_slice(&frame.payload);
    }
    std::fs::write(path, bytes)?;
    Ok(())
}
fn decoded_checksums(path: &Path) -> Result<Vec<String>, Error> {
    let bytes = ffmpeg(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        path.to_str().ok_or("invalid path")?,
        "-map",
        "0:v:0",
        "-vsync",
        "0",
        "-f",
        "framemd5",
        "pipe:1",
    ])?;
    Ok(String::from_utf8(bytes)?
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.rsplit(',').next().unwrap_or("").trim().to_owned())
        .collect())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    std::fs::create_dir_all("dist/native-relay")?;
    let input = Path::new("dist/native-relay/source.ivf");
    ffmpeg(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x180:rate=15",
        "-t",
        "2",
        "-c:v",
        "libaom-av1",
        "-strict",
        "-2",
        "-cpu-used",
        "8",
        "-g",
        "1",
        "-crf",
        "40",
        "-b:v",
        "0",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "ivf",
        input.to_str().ok_or("invalid input path")?,
    ])?;
    let (catalog, frames) = peri_native::av1::read_all_intra_ivf(&std::fs::read(input)?)?;
    let upstream = support::pair(ALPN).await?;
    let (source, ingest) = tokio::try_join!(
        peri_native::publish(upstream.outgoing.clone(), hello(), "live/av1", BUDGET),
        peri_native::accept_publish(upstream.incoming.clone(), hello(), "live/av1", BUDGET)
    )?;
    let (source, ingest) = tokio::try_join!(
        Publisher::new(source, catalog.clone(), BUDGET),
        Subscriber::new(ingest, BUDGET)
    )?;
    let relay = Arc::new(Relay::new(catalog.clone(), Limits::default())?);
    let hub = relay.clone();
    let ingest_task = tokio::spawn(async move { hub.ingest(ingest, BUDGET).await });
    let mut endpoints = Vec::new();
    let mut workers = Vec::new();
    let mut receivers = Vec::new();
    for _ in 0..2 {
        let endpoint = support::pair(ALPN).await?;
        let (server, viewer) = tokio::try_join!(
            peri_native::accept(endpoint.incoming.clone(), hello(), "live/av1", BUDGET),
            peri_native::subscribe(endpoint.outgoing.clone(), hello(), "live/av1", BUDGET)
        )?;
        let hub = relay.clone();
        workers.push(tokio::spawn(async move { hub.serve(server, BUDGET).await }));
        let mut subscriber = Subscriber::new(viewer, BUDGET).await?;
        if subscriber.catalog() != &catalog {
            return Err("relay catalog changed".into());
        }
        let count = frames.len();
        receivers.push(tokio::spawn(async move {
            let mut received = Vec::new();
            for _ in 0..count {
                received.push(subscriber.recv(BUDGET).await?);
            }
            Ok::<_, peri_native::Error>(received)
        }));
        endpoints.push(endpoint);
    }
    let started = tokio::time::Instant::now();
    for frame in &frames {
        tokio::time::sleep_until(started + Duration::from_micros(frame.timestamp_us)).await;
        source.send(frame, BUDGET).await?;
    }
    let mut received_sets = Vec::new();
    for receiver in receivers {
        received_sets.push(receiver.await??);
    }
    drop(source);
    let _ = tokio::time::timeout(BUDGET, ingest_task).await??;
    for worker in workers {
        let _ = tokio::time::timeout(BUDGET, worker).await??;
    }
    let expected = decoded_checksums(input)?;
    if expected.len() != frames.len() {
        return Err("source decode frame count mismatch".into());
    }
    for (index, received) in received_sets.iter().enumerate() {
        if received != &frames {
            return Err("encoded media changed across relay".into());
        }
        let path = format!("dist/native-relay/viewer-{}.ivf", index + 1);
        write_received_ivf(Path::new(&path), &catalog, received)?;
        if decoded_checksums(Path::new(&path))? != expected {
            return Err("decoded pixels changed across relay".into());
        }
        println!(
            "Verified viewer {}: {} AV1 frames, encoded payloads and decoded checksums match",
            index + 1,
            received.len()
        );
    }
    println!("Native PERI publish -> relay -> two viewers verified; files: dist/native-relay");
    drop(endpoints);
    Ok(())
}
