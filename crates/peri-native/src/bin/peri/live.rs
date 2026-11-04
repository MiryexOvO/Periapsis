//! Live synthetic video encoded by a persistent AV1 encoder worker.
use super::{Error, SETUP, connect, hello};
use peri_native::media::{Catalog, Frame, FrameKind, Publisher, Track, TrackKind};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub async fn run(args: &[String]) -> Result<(), Error> {
    let count: u64 = args[5].parse()?;
    if !(1..=13500).contains(&count) {
        return Err("live count must be 1..13500".into());
    }
    let mut command =
        Command::new(std::env::var("PERI_PYTHON").unwrap_or_else(|_| "python".into()));
    command
        .arg(&args[4])
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut worker = command.spawn()?;
    let mut input = worker.stdin.take().ok_or("worker input")?;
    let mut output = worker.stdout.take().ok_or("worker output")?;
    let operation = async {
        let token = std::env::var("PERI_PUBLISH_TOKEN")?;
        let (_endpoint, connection) = connect(&args[0], &args[1], &args[2]).await?;
        let session =
            peri_native::publish_authenticated(connection, hello(), &args[3], &token, SETUP)
                .await?;
        let catalog = Catalog {
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
        };
        let publisher = Publisher::new(session, catalog, SETUP).await?;
        println!("PERI_PUBLISH_READY");
        let started = tokio::time::Instant::now();
        let adaptive = match std::env::var("PERI_ADAPTIVE").as_deref() {
            Ok("1") => true,
            Ok("0") | Err(_) => false,
            _ => return Err("PERI_ADAPTIVE must be 0 or 1".into()),
        };
        let mut controller =
            peri_native::adaptation::RateController::new(std::time::Instant::now());
        let mut desired_rate = 300000u32;
        let mut applied_rate = 300000u32;
        let mut request: Option<peri_native::RecoveryRequest> = None;
        let mut request_at = std::time::Instant::now();
        let mut pending = false;
        let mut last_forced: Option<u64> = None;
        for index in 0..count {
            tokio::time::sleep_until(started + Duration::from_micros(index * 1_000_000 / 15)).await;
            while let Some(received) = publisher.poll_recovery()? {
                peri_native::trace("source_request", received);
                request = Some(received);
                request_at = std::time::Instant::now();
                pending = true;
            }
            if let Some(report) = publisher.poll_report()? {
                peri_native::trace("source_report", report);
                if adaptive && let Some(rate) = controller.update(report, std::time::Instant::now())
                {
                    desired_rate = rate;
                }
            }
            if desired_rate != applied_rate {
                pending = true;
            }
            // Coalesce requests: at most one forced recovery frame per second.
            let force = pending && last_forced.is_none_or(|last| index - last >= 15);
            let encoding_started = std::time::Instant::now();
            let exchange = async {
                input.write_u64(index).await?;
                input.write_u8(u8::from(force)).await?;
                input
                    .write_u32(if force { desired_rate } else { applied_rate })
                    .await?;
                input.flush().await?;
                let size = output.read_u32().await? as usize;
                let pts = output.read_u64().await?;
                let key = output.read_u8().await?;
                if size == 0
                    || size > 1024 * 1024 - 68
                    || pts != index
                    || key > 1
                    || ((force || index == 0) && key != 1)
                {
                    return Err("invalid live encoder packet".into());
                }
                let mut payload = vec![0; size];
                output.read_exact(&mut payload).await?;
                Ok::<_, Error>((key == 1, payload))
            };
            let (key, payload) = tokio::time::timeout(Duration::from_secs(2), exchange).await??;
            if force {
                applied_rate = desired_rate;
                peri_native::trace(
                    "encoder_response",
                    serde_json::json!({"request":request,"sequence":index+1,"target_bps":applied_rate,"request_wait_ms":request.map(|_|request_at.elapsed().as_millis()),"encoding_ms":encoding_started.elapsed().as_millis()}),
                );
                request = None;
                pending = false;
                last_forced = Some(index);
                println!("PERI_KEYFRAME_RESPONSE {} {}", index + 1, payload.len());
            }
            println!(
                "PERI_ENCODED {} {} {}",
                index + 1,
                payload.len(),
                applied_rate
            );
            let frame = Frame {
                track: 1,
                sequence: index + 1,
                timestamp_us: index * 1_000_000 / 15,
                duration_us: 66667,
                kind: if key {
                    FrameKind::Key
                } else {
                    FrameKind::Reference
                },
                dependencies: if key { vec![] } else { vec![index] },
                payload,
            };
            publisher.send(&frame, SETUP).await?;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok::<_, Error>(())
    };
    let result =
        tokio::select! { result = operation => result, _ = tokio::signal::ctrl_c() => Ok(()) };
    let _ = worker.kill().await;
    let _ = worker.wait().await;
    result
}
