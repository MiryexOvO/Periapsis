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
    let coalesced = match std::env::var("PERI_SWITCH_POLICY").as_deref() {
        Ok("coalesced") => true,
        Ok("legacy") | Err(_) => false,
        _ => return Err("PERI_SWITCH_POLICY must be legacy or coalesced".into()),
    };
    let mut command =
        Command::new(std::env::var("PERI_PYTHON").unwrap_or_else(|_| "python".into()));
    command.env("PERI_ENCODER_POOL", if coalesced { "1" } else { "0" });
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
        let mut requests = Vec::<peri_native::RecoveryRequest>::new();
        let mut progress = peri_native::adaptation::RecoveryProgress::default();
        let suppress = match std::env::var("PERI_RECOVERY_SUPPRESS").as_deref() {
            Ok("1") => true,
            Ok("0") | Err(_) => false,
            _ => return Err("PERI_RECOVERY_SUPPRESS must be 0 or 1".into()),
        };
        // 等待时间属于整批需求；同作用域合并时请求ID可能更新，不能解释为该ID的往返耗时。
        let mut pending_at = std::time::Instant::now();
        let mut schedule = peri_native::adaptation::SwitchSchedule::default();
        let recovery_ms: u64 = std::env::var("PERI_RECOVERY_MS")
            .unwrap_or_else(|_| "1000".into())
            .parse()?;
        let mut recovery_pacer =
            peri_native::adaptation::RecoveryPacer::new(Duration::from_millis(recovery_ms))
                .ok_or("PERI_RECOVERY_MS must be 300..1000")?;
        let mut last_forced: Option<u64> = None;
        for index in 0..count {
            tokio::time::sleep_until(started + Duration::from_micros(index * 1_000_000 / 15)).await;
            if let Some(report) = publisher.poll_report()? {
                peri_native::trace("source_report", report);
                progress.observe(report, std::time::Instant::now());
                if adaptive && let Some(rate) = controller.update(report, std::time::Instant::now())
                {
                    desired_rate = rate;
                }
            }
            while let Some(received) = publisher.poll_recovery()? {
                peri_native::trace("source_request", received);
                if suppress && progress.obsolete(received, std::time::Instant::now()) {
                    peri_native::trace("source_request_stale", received);
                    continue;
                }
                if requests.is_empty() {
                    pending_at = std::time::Instant::now();
                }
                if let Some(old) = requests
                    .iter_mut()
                    .find(|r| r.track == received.track && r.viewer == received.viewer)
                {
                    let position = if old.after_sequence == 0 || received.after_sequence == 0 {
                        0
                    } else {
                        old.after_sequence.max(received.after_sequence)
                    };
                    *old = received;
                    old.after_sequence = position;
                } else if requests.len() < 8 {
                    requests.push(received);
                }
            }
            if suppress {
                requests.retain(|request| {
                    if progress.obsolete(*request, std::time::Instant::now()) {
                        peri_native::trace("source_request_stale", request);
                        false
                    } else {
                        true
                    }
                });
            }
            if requests.is_empty() {
                recovery_pacer.clear();
            } else {
                recovery_pacer.request();
            }
            let recovery = recovery_pacer.ready(std::time::Instant::now());
            let rate = if coalesced {
                schedule.rate_for(index, applied_rate, desired_rate, recovery)
            } else if last_forced.is_none_or(|last| index - last >= 15) {
                desired_rate
            } else {
                applied_rate
            };
            let switching = rate != applied_rate;
            let force = recovery || switching;
            peri_native::trace("source_input", serde_json::json!({"sequence":index+1}));
            let encoding_started = std::time::Instant::now();
            let exchange = async {
                input.write_u64(index).await?;
                input.write_u8(u8::from(force)).await?;
                input.write_u32(rate).await?;
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
            schedule.encoded(index, key, switching);
            if force {
                applied_rate = rate;
                peri_native::trace(
                    "encoder_response",
                    serde_json::json!({"request":requests.first().copied(),"coalesced_requests":requests.len(),"sequence":index+1,"target_bps":applied_rate,"bytes":payload.len(),"pending_wait_ms":requests.first().map(|_|pending_at.elapsed().as_millis()),"encoding_ms":encoding_started.elapsed().as_millis(),"switching":switching,"policy":if coalesced {"coalesced"} else {"legacy"}}),
                );
                requests.clear();
                recovery_pacer.encoded(std::time::Instant::now());
                if recovery {
                    println!("PERI_KEYFRAME_RESPONSE {} {}", index + 1, payload.len());
                }
                if switching {
                    println!(
                        "PERI_RATE_SWITCH {} {} {}",
                        index + 1,
                        applied_rate,
                        payload.len()
                    );
                }
                last_forced = Some(index);
            }
            println!(
                "PERI_ENCODED {} {} {} {}",
                index + 1,
                payload.len(),
                applied_rate,
                encoding_started.elapsed().as_micros()
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
