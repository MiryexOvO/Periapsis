//! 修复适配器的真实QUIC测试；不把对象还原当作解码或弱网性能证据。
//! Real QUIC adapter tests; object recovery is not decoding or network performance evidence.
#[path = "../examples/support/mod.rs"]
mod support;
use peri_native::{
    ALPN, Error, Object, Session,
    media::{Catalog, Frame, FrameKind, Publisher, Subscriber, Track, TrackKind},
    repair::{self, Codec},
};
use peri_types::{Hello, PERI_VERSION, Profile};
use std::{sync::Arc, time::Duration};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const BUDGET: Duration = Duration::from_secs(3);
fn hello(repair: bool) -> Hello {
    Hello {
        version: PERI_VERSION,
        profiles: vec![Profile::Live],
        codecs: vec!["video/av1".into()],
        extensions: if repair {
            vec![repair::capability(false)]
        } else {
            vec![]
        },
    }
}
fn catalog() -> Catalog {
    Catalog {
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
    }
}
fn frame(sequence: u64, size: usize) -> Frame {
    Frame {
        track: 1,
        sequence,
        timestamp_us: sequence * 66667,
        duration_us: 66667,
        kind: FrameKind::Key,
        dependencies: vec![],
        payload: vec![sequence as u8; size],
    }
}
async fn sessions() -> Result<(support::Pair, Session, Session)> {
    let pair = support::pair(ALPN).await?;
    let (sender, receiver) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(true), "live/repair", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(true), "live/repair", BUDGET)
    )?;
    Ok((pair, sender, receiver))
}
async fn raw_catalog(sender: &Session, receiver: Session) -> Result<Subscriber> {
    let mut payload = b"PCAT01".to_vec();
    payload.extend_from_slice(&serde_json::to_vec(&catalog())?);
    let object = Object {
        sequence: 0,
        timestamp_us: 0,
        duration_us: 0,
        independent: true,
        payload,
    };
    let (_, subscriber) = tokio::try_join!(
        sender.send(&object, BUDGET),
        Subscriber::new(receiver, BUDGET)
    )?;
    Ok(subscriber)
}
// 每个数据片及校验片逐一省略；新媒体接口只交付完整且校验通过的原帧。
// Omit each data/parity shard; the media API delivers only the verified original frame.
#[tokio::test]
async fn media_adapter_recovers_each_single_erasure_and_expires_double_erasure() -> Result {
    let (_pair, sender, receiver) = sessions().await?;
    let codec = Codec::new(sender.agreement().unwrap())?;
    let mut subscriber = raw_catalog(&sender, receiver).await?;
    for omitted in 0..4 {
        let original = frame(omitted + 1, 2100);
        let parts = codec.split(&original.to_object(&catalog())?, 1024)?;
        assert_eq!(parts.len(), 4);
        let send = async {
            for (index, part) in parts.into_iter().enumerate() {
                if index as u64 != omitted {
                    sender
                        .send(&part.to_object(index as u64 + 1)?, BUDGET)
                        .await?;
                }
            }
            Ok::<_, Error>(())
        };
        let (_, got) = tokio::try_join!(send, subscriber.recv(BUDGET))?;
        assert_eq!(got, original);
    }
    let parts = codec.split(&frame(10, 2100).to_object(&catalog())?, 1024)?;
    for part in parts.into_iter().skip(2) {
        sender.send(&part.to_object(1)?, BUDGET).await?;
    }
    assert_eq!(
        subscriber
            .recv_live(BUDGET, Duration::from_millis(150))
            .await?,
        None
    );
    let original = frame(11, 100);
    let object = original.to_object(&catalog())?;
    let (_, got) = tokio::try_join!(sender.send(&object, BUDGET), subscriber.recv(BUDGET))?;
    assert_eq!(got, original);
    Ok(())
}
// 取消四条半流后必须STOP(1)，后续媒体不可被旧状态污染。
// Cancelling four partial streams must issue STOP(1) and preserve later media.
#[tokio::test]
async fn media_adapter_cancellation_stops_partial_streams() -> Result {
    let (pair, sender, receiver) = sessions().await?;
    let mut subscriber = raw_catalog(&sender, receiver).await?;
    let mut stalled = Vec::new();
    for _ in 0..4 {
        let mut stream = pair.incoming.open_uni().await?;
        stream.write_all(&[0]).await?;
        stalled.push(stream);
    }
    // 第五条完整流必须留在QUIC队列，不能越过已占满的四个槽。
    // The fifth complete stream must remain queued behind the four admitted slots.
    let original = frame(1, 100);
    let object = original.to_object(&catalog())?;
    let receipt = sender.start_send(&object, BUDGET).await?;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            subscriber.recv_live(BUDGET, Duration::from_millis(150))
        )
        .await
        .is_err()
    );
    for stream in stalled {
        assert_eq!(
            tokio::time::timeout(BUDGET, stream.stopped()).await??,
            Some(1u32.into())
        );
    }
    assert_eq!(subscriber.recv(BUDGET).await?, original);
    receipt.finish().await?;
    Ok(())
}
// 正式Relay两跳接入；下游分别协商修复和旧端回退，包含尺寸回退。
// Exercise both Relay hops with repair and legacy viewers, including size fallback.
#[tokio::test]
async fn media_adapter_crosses_relay_and_legacy_viewer() -> Result {
    for downstream_repair in [true, false] {
        let incoming = support::pair(ALPN).await?;
        let (source, ingest) = tokio::try_join!(
            peri_native::publish(
                incoming.outgoing.clone(),
                hello(true),
                "live/repair",
                BUDGET
            ),
            peri_native::accept_publish(
                incoming.incoming.clone(),
                hello(true),
                "live/repair",
                BUDGET
            )
        )?;
        let (source, ingest) = tokio::try_join!(
            Publisher::new(source, catalog(), BUDGET),
            Subscriber::new(ingest, BUDGET)
        )?;
        let relay = Arc::new(peri_native::relay::Relay::new(
            catalog(),
            peri_native::relay::Limits::default(),
        )?);
        let hub = relay.clone();
        let ingest_task = tokio::spawn(async move { hub.ingest(ingest, BUDGET).await });
        let outgoing = support::pair(ALPN).await?;
        let (server, viewer) = tokio::try_join!(
            peri_native::accept(
                outgoing.incoming.clone(),
                hello(true),
                "live/repair",
                BUDGET
            ),
            peri_native::subscribe(
                outgoing.outgoing.clone(),
                hello(downstream_repair),
                "live/repair",
                BUDGET
            )
        )?;
        let hub = relay.clone();
        let serve_task = tokio::spawn(async move { hub.serve(server, BUDGET).await });
        let mut viewer = Subscriber::new(viewer, BUDGET).await?;
        // 发布与观看独立推进，避免逐帧等待观看端掩盖Relay发送重叠时的乱序。
        // Drive source and viewer independently so overlapping relay sends expose reordering.
        let frames: Vec<_> = [100, 2100, 16000, 40000]
            .into_iter()
            .chain([2100; 12])
            .enumerate()
            .map(|(index, size)| frame(index as u64 + 1, size))
            .collect();
        let publish = async {
            for frame in &frames {
                source.send(frame, BUDGET).await?;
                tokio::time::sleep(Duration::from_millis(67)).await;
            }
            Ok::<_, Error>(())
        };
        let watch = async {
            for frame in &frames {
                assert_eq!(viewer.recv(BUDGET).await?, *frame);
            }
            Ok::<_, Error>(())
        };
        tokio::try_join!(publish, watch)?;
        relay.close();
        ingest_task.abort();
        serve_task.abort();
        let _ = ingest_task.await;
        let _ = serve_task.await;
    }
    Ok(())
}

// 重组完整性失败必须报错，迟到片不能重新开启同一组；下一帧仍可交付。
// Integrity failure is fatal to the group; late shards cannot reopen it or poison the next frame.
#[tokio::test]
async fn media_adapter_rejects_corruption_and_replay() -> Result {
    let (_pair, sender, receiver) = sessions().await?;
    let codec = Codec::new(sender.agreement().unwrap())?;
    let mut subscriber = raw_catalog(&sender, receiver).await?;
    let original = frame(1, 2100);
    let parts = codec.split(&original.to_object(&catalog())?, 1024)?;
    let send = async {
        for part in parts.iter().take(3) {
            let mut bytes = part.encode();
            if part.index() == 0 {
                bytes[repair::HEADER_BYTES] ^= 1;
            }
            let bad = repair::Fragment::decode(&bytes)?.to_object(part.index() as u64 + 1)?;
            sender.send(&bad, BUDGET).await?;
        }
        Ok::<_, Error>(())
    };
    let (_, got) = tokio::join!(send, subscriber.recv(BUDGET));
    assert!(matches!(got, Err(Error::Protocol("repair integrity"))));
    for part in parts {
        sender.send(&part.to_object(100)?, BUDGET).await?;
    }
    let next = frame(2, 100);
    let object = next.to_object(&catalog())?;
    let (_, got) = tokio::try_join!(sender.send(&object, BUDGET), subscriber.recv(BUDGET))?;
    assert_eq!(got, next);
    Ok(())
}

// 观看端停止读取大帧时，反馈必须在该帧的驻留期限之前到达源端。
// A viewer stalled on a large frame must still forward feedback before the frame expires.
#[tokio::test]
async fn relay_forwards_feedback_while_object_write_is_blocked() -> Result {
    let upstream = support::pair(ALPN).await?;
    let (source, ingest) = tokio::try_join!(
        peri_native::publish(
            upstream.outgoing.clone(),
            hello(false),
            "live/repair",
            BUDGET
        ),
        peri_native::accept_publish(
            upstream.incoming.clone(),
            hello(false),
            "live/repair",
            BUDGET
        )
    )?;
    let (source, ingest) = tokio::try_join!(
        Publisher::new(source, catalog(), BUDGET),
        Subscriber::new(ingest, BUDGET)
    )?;
    let relay = Arc::new(peri_native::relay::Relay::new(
        catalog(),
        peri_native::relay::Limits::default(),
    )?);
    let hub = relay.clone();
    let ingest_task = tokio::spawn(async move { hub.ingest(ingest, BUDGET).await });
    let downstream = support::pair_with_stream_window(ALPN, false, Some(4096)).await?;
    let (server, viewer) = tokio::try_join!(
        peri_native::accept(
            downstream.incoming.clone(),
            hello(false),
            "live/repair",
            BUDGET
        ),
        peri_native::subscribe(
            downstream.outgoing.clone(),
            hello(false),
            "live/repair",
            BUDGET
        )
    )?;
    let hub = relay.clone();
    let serve_task = tokio::spawn(async move { hub.serve(server, BUDGET).await });
    let viewer = Subscriber::new(viewer, BUDGET).await?;
    source.send(&frame(1, 256 * 1024), BUDGET).await?;
    let mut blocked = tokio::time::timeout(BUDGET, downstream.outgoing.accept_uni()).await??;
    let mut header = [0; 25];
    tokio::time::timeout(BUDGET, blocked.read_exact(&mut header)).await??;
    assert_eq!(u64::from_be_bytes(header[..8].try_into()?), 1);
    viewer.request_keyframe_after(1, 77).await?;
    let forwarded = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            if let Some(request) = source.poll_recovery()?
                && request.after_sequence == 77
            {
                return Ok::<_, Error>(request);
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    blocked.stop(1u32.into())?;
    relay.close();
    ingest_task.abort();
    serve_task.abort();
    let _ = ingest_task.await;
    let _ = serve_task.await;
    assert_eq!(forwarded??.after_sequence, 77);
    Ok(())
}

// 关键帧保留原优先级；可保护参考帧的校验片与数据片同批发出。
// Preserve keyframe priority and send a reference frame's parity in its first flight.
#[tokio::test]
async fn publisher_uses_one_flight_or_raw_stream() -> Result {
    let (_pair, sender, mut receiver) = sessions().await?;
    let publisher = Publisher::new(sender, catalog(), BUDGET).await?;
    assert_eq!(receiver.recv(BUDGET).await?.sequence, 0);
    for (sequence, size, kind) in [
        (1, 2100, FrameKind::Key),
        (2, 2100, FrameKind::Reference),
        (3, 6000, FrameKind::Reference),
    ] {
        let mut original = frame(sequence, size);
        original.kind = kind;
        if kind == FrameKind::Reference {
            original.dependencies = vec![sequence - 1];
        }
        publisher.send(&original, BUDGET).await?;
        if sequence == 2 {
            let mut indices = Vec::new();
            for _ in 0..4 {
                let part = repair::Fragment::from_object(receiver.recv(BUDGET).await?)?;
                assert_eq!(part.group(), sequence);
                indices.push(part.index());
            }
            indices.sort_unstable();
            assert_eq!(indices, [0, 1, 2, 3]);
        } else {
            assert_eq!(
                receiver.recv(BUDGET).await?,
                original.to_object(&catalog())?
            );
        }
    }
    Ok(())
}
