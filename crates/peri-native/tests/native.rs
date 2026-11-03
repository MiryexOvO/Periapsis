use peri_native::{ALPN, Error, Object};
use peri_types::{Extension, Hello, PERI_VERSION, Profile};
use std::{sync::Arc, time::Duration};
#[path = "../examples/support/mod.rs"]
mod support;
use support::pair;

type TestError = Box<dyn std::error::Error + Send + Sync>;
const BUDGET: Duration = Duration::from_secs(3);
#[tokio::test]
async fn protected_endpoint_enforces_bearer_role_and_rejects_anonymous_clients()
-> Result<(), TestError> {
    let publish_token = "p".repeat(48);
    let subscribe_token = "s".repeat(48);
    let credentials =
        peri_native::auth::Credentials::new(publish_token.clone(), subscribe_token.clone())?;
    for scenario in 0..5 {
        let pair = pair(ALPN).await?;
        let client = async {
            match scenario {
                0 => {
                    peri_native::publish_authenticated(
                        pair.outgoing.clone(),
                        hello(),
                        "live/auth",
                        &publish_token,
                        BUDGET,
                    )
                    .await
                }
                1 => {
                    peri_native::subscribe_authenticated(
                        pair.outgoing.clone(),
                        hello(),
                        "live/auth",
                        &subscribe_token,
                        BUDGET,
                    )
                    .await
                }
                2 => {
                    peri_native::publish_authenticated(
                        pair.outgoing.clone(),
                        hello(),
                        "live/auth",
                        &subscribe_token,
                        BUDGET,
                    )
                    .await
                }
                3 => {
                    peri_native::subscribe_authenticated(
                        pair.outgoing.clone(),
                        hello(),
                        "live/auth",
                        &publish_token,
                        BUDGET,
                    )
                    .await
                }
                _ => {
                    peri_native::subscribe(pair.outgoing.clone(), hello(), "live/auth", BUDGET)
                        .await
                }
            }
        };
        let (server, client) = tokio::join!(
            peri_native::accept_authenticated(
                pair.incoming.clone(),
                hello(),
                "live/auth",
                &credentials,
                BUDGET
            ),
            client
        );
        if scenario < 2 {
            let server = server?;
            let client = client?;
            assert_eq!(server.can_send(), scenario == 1);
            assert_ne!(server.can_send(), client.can_send());
        } else {
            assert!(server.is_err());
            assert!(client.is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn push_publish_relay_fans_out_and_late_join_waits_for_key() -> Result<(), TestError> {
    use peri_native::{
        media::{Catalog, Frame, FrameKind, Publisher, Subscriber, Track, TrackKind},
        relay::{Limits, Relay},
    };
    let incoming = pair(ALPN).await?;
    let (source, ingest) = tokio::try_join!(
        peri_native::publish(incoming.outgoing.clone(), hello(), "live/push", BUDGET),
        peri_native::accept_publish(incoming.incoming.clone(), hello(), "live/push", BUDGET),
    )?;
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
    let (source, ingest) = tokio::try_join!(
        Publisher::new(source, catalog.clone(), BUDGET),
        Subscriber::new(ingest, BUDGET)
    )?;
    let relay = Arc::new(Relay::new(
        catalog,
        Limits {
            residence: Duration::from_secs(1),
            ..Limits::default()
        },
    )?);
    let hub = relay.clone();
    let ingest_task = tokio::spawn(async move { hub.ingest(ingest, BUDGET).await });
    let first = pair(ALPN).await?;
    let (server, viewer) = tokio::try_join!(
        peri_native::accept(first.incoming.clone(), hello(), "live/push", BUDGET),
        peri_native::subscribe(first.outgoing.clone(), hello(), "live/push", BUDGET)
    )?;
    let hub = relay.clone();
    let first_task = tokio::spawn(async move { hub.serve(server, BUDGET).await });
    let mut first_viewer = Subscriber::new(viewer, BUDGET).await?;
    let frame = |sequence, kind, dependencies| Frame {
        track: 1,
        sequence,
        timestamp_us: sequence * 33333,
        duration_us: 33333,
        kind,
        dependencies,
        payload: vec![sequence as u8; 100],
    };
    let key = frame(1, FrameKind::Key, vec![]);
    source.send(&key, BUDGET).await?;
    assert_eq!(first_viewer.recv(BUDGET).await?, key);
    let second = pair(ALPN).await?;
    let (server, viewer) = tokio::try_join!(
        peri_native::accept(second.incoming.clone(), hello(), "live/push", BUDGET),
        peri_native::subscribe(second.outgoing.clone(), hello(), "live/push", BUDGET)
    )?;
    let hub = relay.clone();
    let second_task = tokio::spawn(async move { hub.serve(server, BUDGET).await });
    let mut second_viewer = Subscriber::new(viewer, BUDGET).await?;
    assert_eq!(relay.viewers(), 2);
    let delta = frame(2, FrameKind::Reference, vec![1]);
    source.send(&delta, BUDGET).await?;
    assert_eq!(first_viewer.recv(BUDGET).await?, delta);
    let recovery = frame(3, FrameKind::Key, vec![]);
    source.send(&recovery, BUDGET).await?;
    assert_eq!(first_viewer.recv(BUDGET).await?, recovery);
    assert_eq!(second_viewer.recv(BUDGET).await?, recovery);
    // A subscriber cancels one large object while its healthy sibling continues.
    let mut cancelled = frame(4, FrameKind::Key, vec![]);
    cancelled.payload = vec![7; 1024 * 1024 - 68];
    source.send(&cancelled, BUDGET).await?;
    let mut raw = tokio::time::timeout(BUDGET, second.outgoing.accept_uni()).await??;
    raw.stop(1u32.into())?;
    assert_eq!(first_viewer.recv(BUDGET).await?, cancelled);
    let recovered = frame(5, FrameKind::Key, vec![]);
    source.send(&recovered, BUDGET).await?;
    assert_eq!(first_viewer.recv(BUDGET).await?, recovered);
    assert_eq!(second_viewer.recv(BUDGET).await?, recovered);
    assert_eq!(relay.viewers(), 2);
    let resumed = frame(6, FrameKind::Reference, vec![5]);
    source.send(&resumed, BUDGET).await?;
    assert_eq!(first_viewer.recv(BUDGET).await?, resumed);
    assert_eq!(second_viewer.recv(BUDGET).await?, resumed);
    drop(source);
    assert!(tokio::time::timeout(BUDGET, ingest_task).await??.is_err());
    assert!(tokio::time::timeout(BUDGET, first_task).await??.is_err());
    assert!(tokio::time::timeout(BUDGET, second_task).await??.is_err());
    assert_eq!(relay.viewers(), 0);
    Ok(())
}

#[tokio::test]
async fn push_requests_cannot_enter_a_subscribe_only_endpoint() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, server) = tokio::join!(
        peri_native::publish(pair.outgoing.clone(), hello(), "live/push", BUDGET),
        peri_native::accept(pair.incoming.clone(), hello(), "live/push", BUDGET)
    );
    assert!(publisher.is_err());
    assert!(matches!(
        server,
        Err(Error::Protocol("unexpected connection role"))
    ));
    Ok(())
}

#[tokio::test]
async fn media_catalog_and_dependency_frames_cross_real_quic() -> Result<(), TestError> {
    use peri_native::media::{
        Catalog, DecodeBuffer, Event, Frame, FrameKind, Publisher, Subscriber, Track, TrackKind,
    };
    let pair = pair(ALPN).await?;
    let mut capabilities = hello();
    capabilities.codecs.push("audio/opus".into());
    let (publisher, subscriber) = tokio::try_join!(
        peri_native::accept(
            pair.incoming.clone(),
            capabilities.clone(),
            "live/media",
            BUDGET
        ),
        peri_native::subscribe(pair.outgoing.clone(), capabilities, "live/media", BUDGET),
    )?;
    let agreement = subscriber.agreement().unwrap().clone();
    let catalog = Catalog {
        tracks: vec![Track {
            id: 1,
            kind: TrackKind::Video,
            codec: "video/av1".into(),
            init: vec![0x81, 0x00],
            width: Some(1920),
            height: Some(1080),
            sample_rate: None,
            channels: None,
        }],
    };
    let mut catalog = catalog;
    catalog.tracks.push(Track {
        id: 2,
        kind: TrackKind::Audio,
        codec: "audio/opus".into(),
        init: vec![],
        width: None,
        height: None,
        sample_rate: Some(48000),
        channels: Some(2),
    });
    let (publisher, mut subscriber) = tokio::try_join!(
        Publisher::new(publisher, catalog.clone(), BUDGET),
        Subscriber::new(subscriber, BUDGET)
    )?;
    assert_eq!(subscriber.catalog(), &catalog);
    let mut buffer = DecodeBuffer::new(catalog, &agreement, 1024 * 1024, 8)?;
    for sequence in [2, 1] {
        let frame = Frame {
            track: 1,
            sequence,
            timestamp_us: sequence * 33333,
            duration_us: 33333,
            kind: if sequence == 1 {
                FrameKind::Key
            } else {
                FrameKind::Reference
            },
            dependencies: if sequence == 1 { vec![] } else { vec![1] },
            payload: vec![sequence as u8; 4096],
        };
        let (_, received) =
            tokio::try_join!(publisher.send(&frame, BUDGET), subscriber.recv(BUDGET))?;
        assert_eq!(received, frame);
        let now = std::time::Instant::now();
        buffer.push(received, now, now + Duration::from_millis(200))?;
    }
    let now = std::time::Instant::now();
    assert!(matches!(
        buffer.next(now),
        Some(Event::Decode(Frame { sequence: 1, .. }))
    ));
    assert!(buffer.next(now).is_none());
    buffer.decoded(1, now)?;
    assert!(matches!(
        buffer.next(now),
        Some(Event::Decode(Frame { sequence: 2, .. }))
    ));
    let audio = Frame {
        track: 2,
        sequence: 3,
        timestamp_us: 0,
        duration_us: 20000,
        kind: FrameKind::Audio,
        dependencies: vec![],
        payload: vec![42; 100],
    };
    let (_, received) = tokio::try_join!(publisher.send(&audio, BUDGET), subscriber.recv(BUDGET))?;
    assert_eq!(received, audio);
    buffer.push(received, now, now + Duration::from_millis(200))?;
    assert!(matches!(
        buffer.next(now),
        Some(Event::Decode(Frame {
            track: 2,
            kind: FrameKind::Audio,
            ..
        }))
    ));
    Ok(())
}

#[tokio::test]
async fn objects_can_be_sent_concurrently_without_global_stream_ordering() -> Result<(), TestError>
{
    let pair = pair(ALPN).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET),
    )?;
    let objects: Vec<_> = (0..3)
        .map(|sequence| Object {
            sequence,
            timestamp_us: sequence * 20000,
            duration_us: 20000,
            independent: true,
            payload: vec![sequence as u8; 65536],
        })
        .collect();
    let send = async {
        tokio::try_join!(
            publisher.send(&objects[0], BUDGET),
            publisher.send(&objects[1], BUDGET),
            publisher.send(&objects[2], BUDGET)
        )?;
        Ok::<_, Error>(())
    };
    let receive = async {
        let mut received = Vec::new();
        for _ in 0..3 {
            received.push(subscriber.recv(BUDGET).await?);
        }
        received.sort_by_key(|object| object.sequence);
        Ok::<_, Error>(received)
    };
    let (_, received) = tokio::try_join!(send, receive)?;
    assert_eq!(received, objects);
    Ok(())
}

#[tokio::test]
async fn raw_peer_cannot_bypass_extension_preface_length_or_ack_validation() -> Result<(), TestError>
{
    for scenario in 0..4 {
        let pair = pair(ALPN).await?;
        let server = peri_native::accept(
            pair.incoming.clone(),
            hello(),
            "live/test",
            Duration::from_millis(100),
        );
        let raw = async {
            let (mut send, mut recv) = pair.outgoing.open_bi().await?;
            let mut preface = *b"PERI\x00\x01\x00\x00";
            if scenario == 0 {
                preface[5] = 99;
            }
            send.write_all(&preface).await?;
            if scenario == 1 {
                send.write_all(&16385_u32.to_be_bytes()).await?;
            } else {
                let mut capabilities = hello();
                if scenario == 2 {
                    capabilities.extensions.push(Extension {
                        id: "peri.mpquic".into(),
                        version: 1,
                        required: true,
                    });
                }
                let bytes = serde_json::to_vec(
                    &serde_json::json!({ "type": "peri.subscribe", "resource": "live/test", "hello": capabilities }),
                )?;
                send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
                send.write_all(&bytes).await?;
            }
            // 保持控制流存活，等到拒绝或超时，最后一种情况故意不发 ACK。
            let _ = recv.read_to_end(16388).await;
            Ok::<_, TestError>(())
        };
        let (result, _) = tokio::join!(server, raw);
        if scenario == 3 {
            assert!(matches!(result, Err(Error::Timeout)));
        } else {
            assert!(result.is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn subscriber_recomputes_and_rejects_fabricated_agreement() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let subscriber = peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET);
    let raw = async {
        let (mut send, mut recv) = pair.incoming.accept_bi().await?;
        let mut preface = [0; 8];
        recv.read_exact(&mut preface).await?;
        let mut length = [0; 4];
        recv.read_exact(&mut length).await?;
        let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
        recv.read_exact(&mut bytes).await?;
        let mut agreement = peri_session::negotiate(&hello(), &hello())?;
        agreement.buffer.maximum_ms += 1;
        let response = serde_json::to_vec(
            &serde_json::json!({ "type": "peri.accept", "hello": hello(), "agreement": agreement }),
        )?;
        send.write_all(&(response.len() as u32).to_be_bytes())
            .await?;
        send.write_all(&response).await?;
        let _ = recv.read_to_end(16388).await;
        Ok::<_, TestError>(())
    };
    let (result, _) = tokio::join!(subscriber, raw);
    assert!(matches!(result, Err(Error::Protocol("agreement mismatch"))));
    Ok(())
}

fn hello() -> Hello {
    Hello {
        version: PERI_VERSION,
        profiles: vec![Profile::Live],
        codecs: vec!["video/av1".into()],
        extensions: vec![],
    }
}

#[tokio::test]
async fn independent_protocol_negotiates_and_transfers_exact_objects() -> Result<(), TestError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let pair = pair(ALPN).await?;
        let (publisher, mut subscriber) = tokio::try_join!(
            peri_native::accept(pair.incoming.clone(), hello(), "live/game/video", BUDGET),
            peri_native::subscribe(pair.outgoing.clone(), hello(), "live/game/video", BUDGET),
        )?;
        assert_eq!(publisher.agreement(), subscriber.agreement());
        assert_eq!(publisher.agreement().unwrap().profile, Profile::Live);
        for sequence in 0..3 {
            let object = Object {
                sequence,
                timestamp_us: sequence * 33333,
                duration_us: 33333,
                independent: sequence == 0,
                payload: (0..65536).map(|i| (i % 251) as u8).collect(),
            };
            let (_, received) =
                tokio::try_join!(publisher.send(&object, BUDGET), subscriber.recv(BUDGET))?;
            assert_eq!(received, object);
        }
        let mut publisher = publisher;
        assert!(publisher.recv(BUDGET).await.is_err());
        assert!(
            subscriber
                .send(
                    &Object {
                        sequence: 3,
                        timestamp_us: 0,
                        duration_us: 0,
                        independent: true,
                        payload: vec![1]
                    },
                    BUDGET
                )
                .await
                .is_err()
        );
        drop(subscriber);
        tokio::time::timeout(BUDGET, pair.incoming.closed()).await?;
        assert!(publisher.agreement().is_none());
        Ok::<_, TestError>(())
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn rejects_resource_codec_version_and_required_extension_mismatches() -> Result<(), TestError>
{
    for scenario in 0..4 {
        let pair = pair(ALPN).await?;
        let mut remote = hello();
        let resource = if scenario == 0 {
            "live/denied"
        } else {
            "live/allowed"
        };
        if scenario == 1 {
            remote.codecs = vec!["video/vp9".into()];
        }
        if scenario == 2 {
            remote.version = 99;
        }
        // 原始远端可声明未知扩展；本地 API 禁止宣称尚未实现的扩展。
        if scenario == 3 {
            remote.extensions.push(Extension {
                id: "peri.mpquic".into(),
                version: 1,
                required: true,
            });
        }
        let (server, client) = tokio::join!(
            peri_native::accept(pair.incoming.clone(), hello(), "live/allowed", BUDGET),
            peri_native::subscribe(pair.outgoing.clone(), remote, resource, BUDGET),
        );
        assert!(server.is_err());
        assert!(client.is_err());
    }
    Ok(())
}

#[tokio::test]
async fn enforces_alpn_and_handshake_deadline() -> Result<(), TestError> {
    let pair = pair(b"other-protocol").await?;
    assert!(matches!(
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET).await,
        Err(Error::Protocol("ALPN mismatch"))
    ));
    let pair = self::pair(ALPN).await?;
    assert!(matches!(
        peri_native::accept(
            pair.incoming.clone(),
            hello(),
            "live/test",
            Duration::from_millis(30)
        )
        .await,
        Err(Error::Timeout)
    ));
    tokio::time::timeout(BUDGET, pair.outgoing.closed()).await?;
    Ok(())
}

#[tokio::test]
async fn truncated_slow_object_is_stopped_and_next_object_remains_receivable()
-> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET),
    )?;
    let mut malicious = pair.incoming.open_uni().await?;
    malicious.write_all(&[0]).await?;
    assert!(matches!(
        subscriber.recv(Duration::from_millis(30)).await,
        Err(Error::Timeout)
    ));
    assert!(
        tokio::time::timeout(BUDGET, malicious.stopped())
            .await??
            .is_some()
    );
    let object = Object {
        sequence: 1,
        timestamp_us: 0,
        duration_us: 20000,
        independent: true,
        payload: vec![42],
    };
    let (_, received) = tokio::try_join!(publisher.send(&object, BUDGET), subscriber.recv(BUDGET))?;
    assert_eq!(received, object);
    Ok(())
}
#[tokio::test]
async fn live_object_expiry_and_reset_are_local_but_malformed_framing_is_not()
-> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET),
    )?;
    let mut slow = pair.incoming.open_uni().await?;
    slow.write_all(&[0]).await?;
    assert_eq!(
        subscriber
            .recv_live(BUDGET, Duration::from_millis(30))
            .await?,
        None
    );
    assert_eq!(
        tokio::time::timeout(BUDGET, slow.stopped()).await??,
        Some(1u32.into())
    );
    for code in [1u32, 2] {
        let mut cancelled = pair.incoming.open_uni().await?;
        cancelled.write_all(&[0]).await?;
        cancelled.reset(code.into())?;
        let result = subscriber
            .recv_live(BUDGET, Duration::from_millis(100))
            .await;
        if code == 1 {
            assert_eq!(result?, None);
        } else {
            assert!(matches!(result, Err(Error::Io(_))));
        }
    }
    let mut invalid = pair.incoming.open_uni().await?;
    let mut header = vec![0; 25];
    header[20] = 2;
    invalid.write_all(&header).await?;
    invalid.finish()?;
    assert!(matches!(
        subscriber
            .recv_live(BUDGET, Duration::from_millis(100))
            .await,
        Err(Error::Protocol("object header"))
    ));
    let object = Object {
        sequence: 9,
        timestamp_us: 0,
        duration_us: 20000,
        independent: true,
        payload: vec![42],
    };
    let (_, received) = tokio::try_join!(
        publisher.send(&object, BUDGET),
        subscriber.recv_live(BUDGET, Duration::from_millis(100))
    )?;
    assert_eq!(received, Some(object));
    assert!(subscriber.agreement().is_some());
    Ok(())
}

#[tokio::test]
async fn queued_object_receipts_bound_concurrency_and_release_slots() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET),
    )?;
    let mut receipts = Vec::new();
    for sequence in 1..=8 {
        let object = Object {
            sequence,
            timestamp_us: sequence * 20000,
            duration_us: 20000,
            independent: true,
            payload: vec![42],
        };
        receipts.push(publisher.start_send(&object, BUDGET).await?);
        assert_eq!(subscriber.recv(BUDGET).await?, object);
    }
    let ninth = Object {
        sequence: 9,
        timestamp_us: 180000,
        duration_us: 20000,
        independent: true,
        payload: vec![43],
    };
    assert!(matches!(
        publisher
            .start_send(&ninth, Duration::from_millis(30))
            .await,
        Err(Error::Timeout)
    ));
    receipts.remove(0).finish().await?;
    let pending = publisher.start_send(&ninth, BUDGET).await?;
    assert_eq!(subscriber.recv(BUDGET).await?, ninth);
    pending.finish().await?;
    for receipt in receipts {
        receipt.finish().await?;
    }
    Ok(())
}
