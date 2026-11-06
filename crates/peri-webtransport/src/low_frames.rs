//! Extract AVC/AAC packets from the local encoder tee. No FLV crosses the network.
//! 从本机编码器的复用输出读取 AVC/AAC 裸包；浏览器线上只收轨道配置和编码帧。
use crate::{Error, hub::Hub};
use peri_native::media::{Catalog, Frame, FrameKind, Track, TrackKind};
use std::sync::Arc;
use tokio::{io::AsyncReadExt, process::ChildStdout, sync::watch};
pub fn spawn(stdout: ChildStdout) -> Hub {
    let (tx, rx) = watch::channel(None);
    tokio::spawn(async move {
        if let Err(e) = run(stdout, tx).await {
            eprintln!("720p encoder output ended: {e}");
        }
    });
    rx
}
fn u24(b: &[u8]) -> u32 {
    u32::from_be_bytes([0, b[0], b[1], b[2]])
}
async fn run(mut input: ChildStdout, tx: crate::hub::Announce) -> Result<(), Error> {
    let mut header = [0; 13];
    input.read_exact(&mut header).await?;
    if &header[..3] != b"FLV" || header[5..9] != 9u32.to_be_bytes() {
        return Err("invalid encoder header".into());
    }
    let mut frames = None;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut sequence = 0;
    let mut previous = 0;
    loop {
        let mut h = [0; 11];
        input.read_exact(&mut h).await?;
        let size = u24(&h[1..4]) as usize;
        if size > 1024 * 1024 {
            return Err("encoder tag exceeds limit".into());
        }
        let mut data = vec![0; size];
        input.read_exact(&mut data).await?;
        if input.read_u32().await? != size as u32 + 11 {
            return Err("invalid tag boundary".into());
        }
        let timestamp = (u24(&h[4..7]) | (u32::from(h[7]) << 24)) as u64 * 1000;
        let (track, kind, offset, duration) = match h[0] {
            9 if data.len() > 5 && data[0] & 15 == 7 => {
                if data[1] == 0 {
                    video = data[5..].to_vec();
                    continue;
                }
                if data[1] != 1 {
                    continue;
                }
                if data[2..5] != [0, 0, 0] {
                    return Err("unexpected reordered 720p frame".into());
                }
                (
                    1,
                    if data[0] >> 4 == 1 {
                        FrameKind::Key
                    } else {
                        FrameKind::Reference
                    },
                    5,
                    16667,
                )
            }
            8 if data.len() > 2 && data[0] >> 4 == 10 => {
                if data[1] == 0 {
                    audio = data[2..].to_vec();
                    continue;
                }
                if data[1] != 1 {
                    continue;
                }
                (2, FrameKind::Audio, 2, 21333)
            }
            _ => continue,
        };
        if frames.is_none() {
            if video.len() < 4 || audio.is_empty() || kind != FrameKind::Key {
                continue;
            }
            let catalog = Catalog {
                tracks: vec![
                    Track {
                        id: 1,
                        kind: TrackKind::Video,
                        codec: format!("avc1.{:02x}{:02x}{:02x}", video[1], video[2], video[3]),
                        init: video.clone(),
                        width: None,
                        height: None,
                        channels: None,
                        sample_rate: None,
                    },
                    Track {
                        id: 2,
                        kind: TrackKind::Audio,
                        codec: "mp4a.40.2".into(),
                        init: audio.clone(),
                        width: None,
                        height: None,
                        channels: Some(2),
                        sample_rate: Some(48000),
                    },
                ],
            };
            frames = Some(crate::hub::Feed::announce(Arc::new(catalog), &tx));
        }
        sequence += 1;
        let dependencies = if kind == FrameKind::Reference {
            vec![previous]
        } else {
            vec![]
        };
        if track == 1 {
            previous = sequence;
        }
        frames.as_ref().expect("catalog ready").push(Frame {
            track,
            kind,
            sequence,
            timestamp_us: timestamp,
            duration_us: duration,
            dependencies,
            payload: data.split_off(offset),
        });
    }
}
