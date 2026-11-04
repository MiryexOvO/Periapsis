//! Native streaming AV1 decode; decoder output, rather than receipt, confirms frames.
use super::{Error, SETUP, connect, hello};
use peri_native::{
    av1::IvfWriter,
    media::{DecodeBuffer, Event, Frame, FrameKind, Subscriber, Track},
};
use std::{
    io::Cursor,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
};

fn command(name: &str, variable: &str) -> Command {
    let mut command = Command::new(std::env::var(variable).unwrap_or_else(|_| name.into()));
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // background helpers never open console windows
    command
}

pub async fn probe(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut process = command("ffprobe", "PERI_FFPROBE")
        .args([
            "-v",
            "error",
            "-f",
            "ivf",
            "-select_streams",
            "v:0",
            "-show_frames",
            "-show_entries",
            "frame=key_frame,pts,pkt_size,width,height",
            "-of",
            "json",
            "pipe:0",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = process.stdin.take().ok_or("probe stdin")?;
    let output = process.stdout.take().ok_or("probe stdout")?;
    let work = async {
        let send = async {
            input.write_all(bytes).await?;
            input.shutdown().await?;
            drop(input);
            Ok::<_, std::io::Error>(())
        };
        let read = async {
            let mut bytes = Vec::new();
            output.take(256 * 1024 + 1).read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        };
        let (_, bytes) = tokio::try_join!(send, read)?;
        if bytes.len() > 256 * 1024 || !process.wait().await?.success() {
            return Err("AV1 probe failed".into());
        }
        Ok::<_, Error>(bytes)
    };
    tokio::time::timeout(Duration::from_secs(15), work).await?
}

struct Decoder {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    pixels: Vec<u8>,
}
impl Decoder {
    async fn new(track: &Track) -> Result<Self, Error> {
        let width = track.width.ok_or("width")?;
        let height = track.height.ok_or("height")?;
        if u32::from(width) * u32::from(height) > 1920 * 1080 {
            return Err("playback limited to 1080p".into());
        }
        let mut child = command("ffmpeg", "PERI_FFMPEG")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-xerror",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-fpsprobesize",
                "0",
                "-threads",
                "1",
                "-flags",
                "low_delay",
                "-blocksize",
                "32",
                "-f",
                "ivf",
                "-i",
                "pipe:0",
                "-map",
                "0:v:0",
                "-fps_mode",
                "passthrough",
                "-threads",
                "1",
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-flush_packets",
                "1",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut input = child.stdin.take().ok_or("decoder stdin")?;
        let header = IvfWriter::new(Cursor::new(Vec::new()), track.clone())?
            .finish()?
            .into_inner();
        input.write_all(&header).await?;
        let output = child.stdout.take().ok_or("decoder stdout")?;
        Ok(Self {
            child,
            input,
            output,
            pixels: vec![0; usize::from(width) * usize::from(height) * 3],
        })
    }
    async fn decode(&mut self, frame: &Frame) -> Result<(), Error> {
        self.input
            .write_all(&(frame.payload.len() as u32).to_le_bytes())
            .await?;
        self.input
            .write_all(&frame.timestamp_us.to_le_bytes())
            .await?;
        self.input.write_all(&frame.payload).await?;
        self.input.flush().await?;
        self.output.read_exact(&mut self.pixels).await?;
        Ok(())
    }
    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

#[derive(Default)]
struct Stats {
    started: Option<Instant>,
    first_frame_ms: Option<u128>,
    decoded: u32,
    dropped: u32,
    recoveries: u32,
    reconnects: u32,
    last: Option<Instant>,
    max_gap: u128,
    freeze_ms: u128,
    freeze_count: u32,
    first_output: Option<Instant>,
    played_sequence: u64,
    media_us: u64,
    max_lag: u128,
    lateness: Vec<u128>,
}

async fn report_gap(
    subscriber: &Subscriber,
    track: u16,
    buffer: &DecodeBuffer,
    stats: &Stats,
) -> Result<(), Error> {
    subscriber
        .send_report(peri_native::ReceiverReport {
            track,
            viewer: 0,
            sample: 1,
            sequence: stats.played_sequence,
            media_us: stats.media_us,
            buffered_ms: buffer.buffered_ms(),
            queued_bytes: buffer.queued_bytes() as u32,
            lateness_ms: 0,
            dropped: u64::from(stats.dropped),
            recoveries: stats.recoveries,
        })
        .await?;
    Ok(())
}

pub async fn play(args: &[String]) -> Result<(), Error> {
    let mut stats = Stats {
        started: Some(Instant::now()),
        ..Stats::default()
    };
    let operation = async {
        loop {
            match play_once(args, &mut stats).await {
                Ok(()) => break,
                Err(error) => {
                    let transient = matches!(
                        error.downcast_ref::<peri_native::Error>(),
                        Some(
                            peri_native::Error::Io(_)
                                | peri_native::Error::Transport(_)
                                | peri_native::Error::Timeout
                        )
                    );
                    if stats.decoded == 0 || stats.reconnects >= 8 || !transient {
                        return Err(error);
                    }
                    stats.reconnects += 1;
                    println!("PERI_RECONNECT {}", stats.reconnects);
                    tokio::time::sleep(Duration::from_millis(
                        125 * u64::from(stats.reconnects).min(8),
                    ))
                    .await;
                }
            }
        }
        stats.lateness.sort_unstable();
        let percentile = |percent: usize| {
            stats
                .lateness
                .get((stats.lateness.len().saturating_sub(1) * percent) / 100)
                .copied()
                .unwrap_or(0)
        };
        // 连续性按首末实际输出的单调时间统计，包含期间的停顿和重连。
        let playback_ms = stats
            .first_output
            .zip(stats.last)
            .map(|(first, last)| last.duration_since(first).as_millis())
            .unwrap_or(0);
        println!(
            "PERI_PLAY_DONE {}",
            serde_json::json!({
                "playback_ms":playback_ms,"freeze_count":stats.freeze_count,
                "freeze_ratio":if playback_ms>0 {stats.freeze_ms as f64/playback_ms as f64} else {0.0},
                "output_fps":if playback_ms>0 {f64::from(stats.decoded.saturating_sub(1))*1000.0/playback_ms as f64} else {0.0},
                "first_frame_ms": stats.first_frame_ms, "decoded": stats.decoded, "dropped": stats.dropped, "recoveries": stats.recoveries,
                "reconnects": stats.reconnects, "max_gap_ms": stats.max_gap,
                "max_lateness_ms": stats.max_lag, "freeze_ms":stats.freeze_ms, "p50_lateness_ms": percentile(50), "p95_lateness_ms": percentile(95)
            })
        );
        Ok::<_, Error>(())
    };
    tokio::select! { result = operation => result, _ = tokio::signal::ctrl_c() => Ok(()) }
}

async fn play_once(args: &[String], stats: &mut Stats) -> Result<(), Error> {
    let count: u32 = args[4].parse()?;
    if !(1..=10000).contains(&count) || args[5..].iter().any(|a| a != "--headless" && a != "--fifo")
    {
        return Err("invalid playback arguments".into());
    }
    let headless = args[5..].iter().any(|a| a == "--headless");
    let fifo = args[5..].iter().any(|a| a == "--fifo");
    let token = std::env::var("PERI_SUBSCRIBE_TOKEN")?;
    let (_endpoint, connection) = connect(&args[0], &args[1], &args[2]).await?;
    let session =
        peri_native::subscribe_authenticated(connection, hello(), &args[3], &token, SETUP).await?;
    let agreement = session.agreement().ok_or("session closed")?.clone();
    let recovery_read_ms: u64 = std::env::var("PERI_RECOVERY_READ_MS")
        .unwrap_or_else(|_| "300".into())
        .parse()?;
    if !matches!(recovery_read_ms, 150 | 300) {
        return Err("PERI_RECOVERY_READ_MS must be 150 or 300".into());
    }
    // 恢复读取窗口受协商上限约束；正常播放仍使用150毫秒预算。
    let recovery_residence =
        Duration::from_millis(recovery_read_ms.min(u64::from(agreement.buffer.maximum_ms)));

    let mut subscriber = Subscriber::new(session, SETUP).await?;
    let track = subscriber
        .catalog()
        .tracks
        .iter()
        .find(|t| t.codec == "video/av1")
        .ok_or("no AV1")?
        .clone();
    // Late join must request an independent unit rather than wait for the natural GOP.
    subscriber.request_keyframe(track.id).await?;
    let mut buffer = DecodeBuffer::new(
        subscriber.catalog().clone(),
        &agreement,
        8 * 1024 * 1024,
        32,
    )?;
    let mut decoder = Decoder::new(&track).await?;
    let mut display = if headless {
        None
    } else {
        Some(
            command("ffplay", "PERI_FFPLAY")
                .args([
                    "-loglevel",
                    "error",
                    "-probesize",
                    "32",
                    "-analyzeduration",
                    "0",
                    "-fpsprobesize",
                    "0",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgb24",
                    "-video_size",
                    &format!("{}x{}", track.width.unwrap(), track.height.unwrap()),
                    "-framerate",
                    "15",
                    "-i",
                    "pipe:0",
                    "-autoexit",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        )
    };
    println!("PERI_PLAY_READY");
    let operation = async {
        let mut anchor: Option<(u64, Instant)> = None;
        let mut waiting_key = true;

        let mut previous = 0;
        let mut progress = Instant::now();
        while stats.decoded < count {
            if progress.elapsed() > Duration::from_secs(10) {
                return Err("decoder made no progress for 10 seconds".into());
            }
            // 参考链失效后跳过旧依赖负载，避免旧对象反复占满读取预算。
            let received = if waiting_key {
                subscriber.recv_recovery(SETUP, recovery_residence).await?
            } else {
                subscriber
                    .recv_live(SETUP, Duration::from_millis(150))
                    .await?
            };
            let frame = match received {
                Some(frame) => frame,
                None => {
                    stats.dropped += 1;
                    subscriber.request_keyframe(track.id).await?;
                    report_gap(&subscriber, track.id, &buffer, stats).await?;
                    if !waiting_key {
                        buffer.recover(track.id);
                        waiting_key = true;
                        stats.recoveries += 1;
                        subscriber.request_keyframe(track.id).await?;
                        report_gap(&subscriber, track.id, &buffer, stats).await?;
                        decoder.stop().await;
                        decoder = Decoder::new(&track).await?;
                    }
                    continue;
                }
            };
            if frame.kind == FrameKind::Key {
                peri_native::trace(
                    "key_received",
                    serde_json::json!({"sequence":frame.sequence}),
                );
            }
            if frame.track != track.id {
                continue;
            }
            if waiting_key && frame.kind != FrameKind::Key {
                // The control layer rate-limits retries, including initial join.
                subscriber.request_keyframe(track.id).await?;
                report_gap(&subscriber, track.id, &buffer, stats).await?;
                stats.dropped += 1;
                continue;
            }
            if waiting_key {
                anchor = Some((
                    frame.timestamp_us,
                    Instant::now() + Duration::from_millis(120),
                ));
                waiting_key = false;
            }
            if frame.sequence <= previous {
                return Err("playback requires ordered media".into());
            }
            if previous != 0
                && frame.dependencies.iter().any(|id| *id > previous)
                && frame.kind == FrameKind::Reference
            {
                previous = frame.sequence;
                buffer.recover(track.id);
                waiting_key = true;
                stats.recoveries += 1;
                stats.dropped += 1;
                subscriber.request_keyframe(track.id).await?;
                report_gap(&subscriber, track.id, &buffer, stats).await?;
                decoder.stop().await;
                decoder = Decoder::new(&track).await?;
                continue;
            }
            previous = frame.sequence;
            let (pts, origin) = anchor.ok_or("playback clock")?;
            let target = origin
                .checked_add(Duration::from_micros(
                    frame
                        .timestamp_us
                        .checked_sub(pts)
                        .ok_or("timeline reversal")?,
                ))
                .ok_or("timeline overflow")?;
            if target > Instant::now() + Duration::from_secs(1) {
                return Err("playback timeline gap".into());
            }
            let now = Instant::now();
            buffer.push(
                frame,
                now,
                if fifo {
                    now + Duration::from_millis(agreement.buffer.maximum_ms.into())
                } else {
                    target + Duration::from_millis(120)
                },
            )?;
            while let Some(event) = buffer.next(Instant::now()) {
                match event {
                    Event::Decode(frame) => {
                        let budget = if fifo {
                            Duration::from_secs(2)
                        } else {
                            (target + Duration::from_millis(120))
                                .saturating_duration_since(Instant::now())
                        };
                        if !matches!(
                            tokio::time::timeout(budget, decoder.decode(&frame)).await,
                            Ok(Ok(()))
                        ) {
                            buffer.recover(track.id);
                            waiting_key = true;
                            stats.recoveries += 1;
                            subscriber.request_keyframe(track.id).await?;
                            report_gap(&subscriber, track.id, &buffer, stats).await?;
                            decoder.stop().await;
                            decoder = Decoder::new(&track).await?;
                            break;
                        }
                        if buffer.decoded(frame.sequence, Instant::now()).is_err() {
                            waiting_key = true;
                            stats.recoveries += 1;
                            subscriber.request_keyframe(track.id).await?;
                            report_gap(&subscriber, track.id, &buffer, stats).await?;
                            decoder.stop().await;
                            decoder = Decoder::new(&track).await?;
                            break;
                        }
                        if frame.kind == FrameKind::Key {
                            peri_native::trace(
                                "key_decoded",
                                serde_json::json!({"sequence":frame.sequence,"media_us":frame.timestamp_us}),
                            );
                        }

                        if target > Instant::now() {
                            tokio::time::sleep_until(target.into()).await;
                        }
                        if let Some(display) = display.as_mut() {
                            tokio::time::timeout(
                                Duration::from_secs(2),
                                display
                                    .stdin
                                    .as_mut()
                                    .ok_or("display stdin")?
                                    .write_all(&decoder.pixels),
                            )
                            .await
                            // 显示管道超时与协议接收超时分开标识，保留故障而不放宽期限。
                            .map_err(|_| "display pipe write exceeded 2 seconds")??;
                        }
                        let shown = Instant::now();
                        stats.first_output.get_or_insert(shown);
                        if stats.first_frame_ms.is_none() {
                            stats.first_frame_ms = stats
                                .started
                                .map(|start| shown.duration_since(start).as_millis());
                        }
                        stats.max_lag = stats
                            .max_lag
                            .max(shown.saturating_duration_since(target).as_millis());
                        if let Some(last) = stats.last {
                            let gap = shown.duration_since(last).as_millis();
                            if gap > 250 {
                                stats.freeze_count += 1;
                                stats.freeze_ms += gap.saturating_sub(67);
                            }
                            stats.max_gap =
                                stats.max_gap.max(shown.duration_since(last).as_millis());
                        }
                        stats.last = Some(shown);
                        stats
                            .lateness
                            .push(shown.saturating_duration_since(target).as_millis());
                        progress = shown;
                        peri_native::trace(
                            "frame_output",
                            serde_json::json!({"sequence":frame.sequence}),
                        );
                        stats.decoded += 1;
                        stats.played_sequence = frame.sequence;
                        stats.media_us = frame.timestamp_us;
                        subscriber
                            .send_report(peri_native::ReceiverReport {
                                track: track.id,
                                viewer: 0,
                                sample: 1,
                                sequence: frame.sequence,
                                media_us: frame.timestamp_us,
                                buffered_ms: buffer.buffered_ms(),
                                queued_bytes: buffer.queued_bytes() as u32,
                                lateness_ms: shown
                                    .saturating_duration_since(target)
                                    .as_millis()
                                    .min(60000) as u32,
                                dropped: u64::from(stats.dropped),
                                recoveries: stats.recoveries,
                            })
                            .await?;
                        println!(
                            "PERI_FRAME {} {} {}",
                            frame.sequence,
                            frame.timestamp_us,
                            shown.saturating_duration_since(target).as_millis()
                        );
                    }
                    Event::Drop { .. } => {
                        stats.dropped += 1;
                    }
                    Event::Recover { .. } => {
                        stats.recoveries += 1;
                        subscriber.request_keyframe(track.id).await?;
                        report_gap(&subscriber, track.id, &buffer, stats).await?;
                        waiting_key = true;
                        decoder.stop().await;
                        decoder = Decoder::new(&track).await?;
                    }
                }
            }
        }
        Ok::<_, Error>(())
    };
    let result = operation.await;
    decoder.stop().await;
    if let Some(display) = display.as_mut() {
        let _ = display.kill().await;
        let _ = display.wait().await;
    }
    result
}
