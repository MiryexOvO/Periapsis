//! Local codec adapter pipes for a paired AV1/Opus program.
//! AV1/Opus 节目的本地编解码适配管道。
use super::{Error, SETUP, connect, hello};
use peri_native::media::{Catalog, Frame, FrameKind, Publisher, Subscriber, TrackKind};
use std::io::{Read, Write};

fn catalog_profile(c: &Catalog) -> Result<(), Error> {
    if c.tracks.len() != 2
        || !c
            .tracks
            .iter()
            .any(|t| t.id == 1 && t.kind == TrackKind::Video && t.codec == "video/av1")
        || !c
            .tracks
            .iter()
            .any(|t| t.id == 2 && t.kind == TrackKind::Audio && t.codec == "audio/opus")
    {
        return Err("expected AV1 track 1 and Opus track 2".into());
    }
    Ok(())
}

pub async fn run(args: &[String], publish: bool) -> Result<(), Error> {
    peri_native::trace_to_stderr();
    let (_endpoint, connection) = connect(&args[0], &args[1], &args[2]).await?;
    if publish {
        let mut input = std::io::stdin().lock();
        let catalog: Catalog = serde_json::from_slice(&blob(&mut input, 16384)?)?;
        catalog_profile(&catalog)?;
        let token = std::env::var("PERI_PUBLISH_TOKEN")?;
        let session =
            peri_native::publish_authenticated(connection, hello(), &args[3], &token, SETUP)
                .await?;
        let sender = Publisher::new(session, catalog, SETUP).await?;
        let mut sequence = 0;
        let mut previous_video = 0;
        loop {
            let mut tag = [0];
            if input.read(&mut tag)? == 0 {
                break;
            }
            let mut clock = [0; 12];
            input.read_exact(&mut clock)?;
            let payload = blob(&mut input, 1024 * 1024 - 68)?;
            let (track, kind) = match tag[0] {
                1 => (1, FrameKind::Key),
                2 => (1, FrameKind::Reference),
                3 => (2, FrameKind::Audio),
                _ => return Err("invalid packet kind".into()),
            };
            sequence += 1;
            let dependencies = if kind == FrameKind::Reference {
                vec![previous_video]
            } else {
                vec![]
            };
            if track == 1 {
                previous_video = sequence;
            }
            let frame = Frame {
                track,
                sequence,
                kind,
                dependencies,
                payload,
                timestamp_us: u64::from_be_bytes(clock[..8].try_into()?),
                duration_us: u32::from_be_bytes(clock[8..].try_into()?),
            };
            sender.send(&frame, SETUP).await?;
            let _ = sender.poll_recovery()?;
            let _ = sender.poll_report()?;
        }
    } else {
        let token = std::env::var("PERI_SUBSCRIBE_TOKEN")?;
        let session =
            peri_native::subscribe_authenticated(connection, hello(), &args[3], &token, SETUP)
                .await?;
        let mut receiver = Subscriber::new(session, SETUP).await?;
        catalog_profile(receiver.catalog())?;
        let mut out = std::io::stdout().lock();
        let catalog = serde_json::to_vec(receiver.catalog())?;
        out.write_all(&(catalog.len() as u32).to_be_bytes())?;
        out.write_all(&catalog)?;
        out.flush()?;
        let mut previous_video = None;
        receiver.request_keyframe_after(1, 0).await?;
        loop {
            let f = receiver.recv(SETUP).await?;
            // Resume only at a decodable video boundary. / 仅从可解码的视频边界恢复。
            let tag = match f.kind {
                FrameKind::Key => 1,
                FrameKind::Reference if previous_video.is_some_and(|id| f.dependencies == [id]) => {
                    2
                }
                FrameKind::Audio if previous_video.is_some() => 3,
                _ => {
                    if f.track == 1 {
                        previous_video = None;
                    }
                    continue;
                }
            };
            if f.track == 1 {
                previous_video = Some(f.sequence);
            }
            out.write_all(&[tag])?;
            out.write_all(&f.timestamp_us.to_be_bytes())?;
            out.write_all(&f.duration_us.to_be_bytes())?;
            out.write_all(&(f.payload.len() as u32).to_be_bytes())?;
            out.write_all(&f.payload)?;
            out.flush()?;
        }
    }
    Ok(())
}

fn blob(input: &mut impl Read, max: usize) -> Result<Vec<u8>, Error> {
    let mut length = [0; 4];
    input.read_exact(&mut length)?;
    let size = u32::from_be_bytes(length) as usize;
    if size == 0 || size > max {
        return Err("invalid adapter packet length".into());
    }
    let mut bytes = vec![0; size];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

