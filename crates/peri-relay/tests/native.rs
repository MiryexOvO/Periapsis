use peri_relay::{Limits, Relay};
#[path = "../../peri-native/examples/support/playback_check.rs"]
mod playback_check;
use peri_native::{ALPN, Error, Object};
use peri_types::{Extension, Hello, PERI_VERSION, Profile};
use std::{sync::Arc, time::Duration};
#[path = "../../peri-native/examples/support/mod.rs"]
mod support;
use support::pair;

type TestError = Box<dyn std::error::Error + Send + Sync>;
const BUDGET: Duration = Duration::from_secs(3);

#[tokio::test]
async fn send_window_retains_byte_credit_until_receipts_are_released() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (mut publisher, _subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/window", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/window", BUDGET)
    )?;
    assert!(publisher.set_send_limits(33, 2 * 1024 * 1024).is_err());
    publisher.set_send_limits(32, 2 * 1024 * 1024)?;
    let object = |sequence| Object {
        sequence,
        timestamp_us: 0,
        duration_us: 16667,
        independent: true,
        payload: vec![42; 400000],
    };
    let mut held = Vec::new();
    for sequence in 1..=5 {
        held.push(publisher.start_send(&object(sequence), BUDGET).await?);
    }
    assert!(publisher.set_send_limits(32, 2 * 1024 * 1024).is_err());
    assert!(matches!(
        publisher
            .start_send(&object(6), Duration::from_millis(30))
            .await,
        Err(Error::Timeout)
    ));
    drop(held.pop());
    held.push(publisher.start_send(&object(7), BUDGET).await?);
    drop(held);
    publisher.set_send_limits(8, 8 * 1024 * 1024)?;
    Ok(())
}

/// 真实QUIC逐次省略每一片，验证协商后的分片精确保真与150ms本地重组预算。
/// 这是应用层片丢失夹具，不是随机UDP丢包或正式Relay播放性能实验。
#[tokio::test]
async fn repair_single_erasure_crosses_negotiated_quic() -> Result<(), TestError> {
    use peri_native::repair::{self, Admission, Codec, Fragment, Limits};
    let pair = pair(ALPN).await?;
    let mut offered = hello();
    offered.extensions.push(repair::capability(true));
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(
            pair.incoming.clone(),
            offered.clone(),
            "live/repair",
            BUDGET
        ),
        peri_native::subscribe(pair.outgoing.clone(), offered, "live/repair", BUDGET)
    )?;
    let codec = Codec::new(publisher.agreement().unwrap())?;
    let mut assembler = Codec::new(subscriber.agreement().unwrap())?.assembler(Limits::default())?;
    let mut max_elapsed = 0;
    for omitted in 0..5 {
        let sequence = omitted as u64 + 1;
        let original = Object {
            sequence,
            timestamp_us: sequence * 66667,
            duration_us: 66667,
            independent: true,
            payload: (0..2000)
                .map(|i| ((i * i + 17 * i + sequence as usize) % 251) as u8)
                .collect(),
        };
        let fragments = codec.split(&original, 512)?;
        assert_eq!(fragments.len(), 5);
        let selected: Vec<_> = fragments
            .iter()
            .enumerate()
            .rev()
            .filter(|(index, _)| *index != omitted)
            .map(|(index, f)| f.to_object(sequence * 100 + index as u64 + 1))
            .collect::<Result<_, _>>()?;
        let started = std::time::Instant::now();
        let deadline = started + Duration::from_millis(150);
        let send = async {
            let mut pending = Vec::new();
            for object in &selected {
                pending.push(publisher.start_send(object, BUDGET).await?);
            }
            for receipt in pending {
                receipt.finish().await?;
            }
            Ok::<_, TestError>(())
        };
        let receive = async {
            let mut output = None;
            for _ in 0..selected.len() {
                let fragment = Fragment::from_object(subscriber.recv(BUDGET).await?)?;
                if let Admission::Complete { object, repaired } =
                    assembler.push(fragment, std::time::Instant::now(), deadline)?
                {
                    assert_eq!(repaired, omitted < 4);
                    output = Some(object);
                }
            }
            Ok::<_, TestError>(output)
        };
        let (_, got) = tokio::try_join!(send, receive)?;
        max_elapsed = max_elapsed.max(started.elapsed().as_micros());
        assert_eq!(got, Some(original));
        assert_eq!(assembler.reserved_bytes(), 0);
        let late = fragments[omitted].to_object(sequence * 100 + 99)?;
        let (_, late) = tokio::try_join!(publisher.send(&late, BUDGET), subscriber.recv(BUDGET))?;
        assert_eq!(
            assembler.push(
                Fragment::from_object(late)?,
                std::time::Instant::now(),
                std::time::Instant::now() + Duration::from_millis(150)
            )?,
            Admission::Stale
        );
    }
    println!("PERI_REPAIR_FIXTURE cases=5 max_elapsed_us={max_elapsed} local_budget_ms=150");
    Ok(())
}

#[tokio::test]
async fn optional_repair_falls_back_and_required_repair_rejects_legacy_offer()
-> Result<(), TestError> {
    use peri_native::repair::{self, Codec};
    for required in [false, true] {
        let pair = pair(ALPN).await?;
        let mut offered = hello();
        offered.extensions.push(repair::capability(required));
        let (sender, receiver) = tokio::join!(
            peri_native::accept(pair.incoming.clone(), hello(), "live/repair", BUDGET),
            peri_native::subscribe(pair.outgoing.clone(), offered, "live/repair", BUDGET)
        );
        if required {
            assert!(sender.is_err());
            assert!(receiver.is_err());
            continue;
        }
        let sender = sender?;
        let mut receiver = receiver?;
        assert!(Codec::new(sender.agreement().unwrap()).is_err());
        assert!(Codec::new(receiver.agreement().unwrap()).is_err());
        let object = Object {
            sequence: 1,
            timestamp_us: 0,
            duration_us: 66667,
            independent: true,
            payload: vec![42],
        };
        let (_, got) = tokio::try_join!(sender.send(&object, BUDGET), receiver.recv(BUDGET))?;
        assert_eq!(got, object);
    }
    Ok(())
}

#[tokio::test]
async fn frequent_ack_requests_cross_real_quic_and_preserve_objects() -> Result<(), TestError> {
    let pair = support::pair_with_ack(ALPN, true).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
    )?;
    for sequence in 1..=8 {
        let object = Object {
            sequence,
            timestamp_us: sequence * 66667,
            duration_us: 66667,
            independent: sequence == 1,
            payload: vec![sequence as u8; 8192],
        };
        let (_, received) =
            tokio::try_join!(publisher.send(&object, BUDGET), subscriber.recv(BUDGET))?;
        assert_eq!(received, object);
    }
    tokio::time::timeout(BUDGET, async {
        loop {
            let sender = publisher.transport_stats();
            let receiver = subscriber.transport_stats();
            if sender.frame_tx.ack_frequency > 0
                && sender.frame_rx.ack_frequency > 0
                && receiver.frame_tx.ack_frequency > 0
                && receiver.frame_rx.ack_frequency > 0
            {
                assert!(receiver.frame_tx.acks > 0);
                assert!(receiver.udp_tx.bytes > 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    Ok(())
}

/// 外层取消不能退回默认STOP码；三种串行接口都须使用局部放弃码1。
#[tokio::test]
async fn serial_receive_cancellation_stops_one_and_keeps_next_object() -> Result<(), TestError> {
    for mode in 0..3 {
        let pair = pair(ALPN).await?;
        let (publisher, mut subscriber) = tokio::try_join!(
            peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
            peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
        )?;
        let mut partial = pair.incoming.open_uni().await?;
        partial.write_all(&[0]).await?;
        let read = async {
            match mode {
                0 => subscriber.recv(BUDGET).await.map(Some),
                1 => {
                    subscriber
                        .recv_live(BUDGET, Duration::from_millis(800))
                        .await
                }
                _ => {
                    subscriber
                        .recv_recovery(BUDGET, Duration::from_millis(800))
                        .await
                }
            }
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(30), read)
                .await
                .is_err()
        );
        assert_eq!(
            tokio::time::timeout(BUDGET, partial.stopped()).await??,
            Some(1u32.into())
        );
        let object = Object {
            sequence: 2,
            timestamp_us: 0,
            duration_us: 66667,
            independent: true,
            payload: vec![42],
        };
        let (_, got) = tokio::try_join!(publisher.send(&object, BUDGET), subscriber.recv(BUDGET))?;
        assert_eq!(got, object);
    }
    Ok(())
}

#[tokio::test]
async fn retained_receive_rejects_observed_bad_headers_and_releases_slots() -> Result<(), TestError>
{
    let pair = pair(ALPN).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
    )?;
    let mut blocked = pair.incoming.open_uni().await?;
    blocked.write_all(&[0]).await?;
    let mut invalid = pair.incoming.open_uni().await?;
    let mut header = vec![0; 25];
    header[20] = 2;
    invalid.write_all(&header).await?;
    invalid.finish()?;
    assert!(matches!(
        subscriber
            .recv_retained(BUDGET, Duration::from_millis(500), true)
            .await,
        Err(Error::Protocol("object header"))
    ));
    assert_eq!(
        tokio::time::timeout(BUDGET, blocked.stopped()).await??,
        Some(1u32.into())
    );
    let key = Object {
        sequence: 3,
        timestamp_us: 0,
        duration_us: 66667,
        independent: true,
        payload: vec![42],
    };
    let (_, received) = tokio::try_join!(publisher.send(&key, BUDGET), subscriber.recv(BUDGET))?;
    assert_eq!(received, key);
    Ok(())
}

/// 参考帧先于关键帧完成；胜出时必须保留更晚流的完整负载和部分负载。
#[tokio::test]
async fn retained_receive_preserves_successors_in_order() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (_publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
    )?;
    let object = |sequence, independent| Object {
        sequence,
        timestamp_us: sequence * 66667,
        duration_us: 66667,
        independent,
        payload: vec![sequence as u8; 100],
    };
    let mut old = pair.incoming.open_uni().await?;
    old.write_all(&[0]).await?;
    let key = object(2, true);
    let key_bytes = key.encode_packet()?;
    let mut key_stream = pair.incoming.open_uni().await?;
    key_stream.write_all(&key_bytes[..30]).await?;
    let reference = object(3, false);
    let mut complete = pair.incoming.open_uni().await?;
    complete.write_all(&reference.encode_packet()?).await?;
    complete.finish()?;
    let partial_object = object(4, false);
    let partial_bytes = partial_object.encode_packet()?;
    let mut partial = pair.incoming.open_uni().await?;
    partial.write_all(&partial_bytes[..30]).await?;
    let fill_key = async {
        tokio::time::sleep(Duration::from_millis(40)).await;
        key_stream.write_all(&key_bytes[30..]).await?;
        key_stream.finish()?;
        Ok::<_, TestError>(())
    };
    let (_, received) = tokio::try_join!(fill_key, async {
        Ok::<_, TestError>(
            subscriber
                .recv_retained(BUDGET, Duration::from_millis(500), true)
                .await?,
        )
    })?;
    assert_eq!(received, Some(key));
    assert_eq!(old.stopped().await?, Some(1u32.into()));
    assert!(matches!(
        subscriber.recv(BUDGET).await,
        Err(Error::Protocol("pending retained receive"))
    ));
    assert_eq!(
        subscriber
            .recv_retained(BUDGET, Duration::from_millis(150), false)
            .await?,
        Some(reference)
    );
    partial.write_all(&partial_bytes[30..]).await?;
    partial.finish()?;
    assert_eq!(
        subscriber
            .recv_retained(BUDGET, Duration::from_millis(150), false)
            .await?,
        Some(partial_object)
    );
    assert_eq!(complete.stopped().await?, None);
    assert_eq!(partial.stopped().await?, None);
    Ok(())
}

/// 跨调用不刷新驻留期限；外层取消、格式错误及清空均须释放保留窗口。
#[tokio::test]
async fn retained_receive_expires_and_cancels_retained_streams() -> Result<(), TestError> {
    for scenario in 0..3 {
        let pair = pair(ALPN).await?;
        let (_publisher, mut subscriber) = tokio::try_join!(
            peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
            peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
        )?;
        let key = Object {
            sequence: 1,
            timestamp_us: 0,
            duration_us: 66667,
            independent: true,
            payload: vec![42; 100],
        };
        let bytes = key.encode_packet()?;
        let mut key_stream = pair.incoming.open_uni().await?;
        key_stream.write_all(&bytes[..30]).await?;
        let mut next = pair.incoming.open_uni().await?;
        if scenario == 2 {
            let expired = Object {
                sequence: 2,
                independent: false,
                ..key.clone()
            };
            next.write_all(&expired.encode_packet()?).await?;
            next.finish()?;
        } else {
            next.write_all(&[0]).await?;
        }
        let fill_key = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            key_stream.write_all(&bytes[30..]).await?;
            key_stream.finish()?;
            Ok::<_, TestError>(())
        };
        let (_, received) = tokio::try_join!(fill_key, async {
            Ok::<_, TestError>(
                subscriber
                    .recv_retained(BUDGET, Duration::from_millis(150), true)
                    .await?,
            )
        })?;
        assert_eq!(received, Some(key));
        if scenario == 1 {
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(20),
                    subscriber.recv_retained(BUDGET, Duration::from_millis(800), false)
                )
                .await
                .is_err()
            );
        } else {
            tokio::time::sleep(Duration::from_millis(160)).await;
            assert_eq!(
                subscriber
                    .recv_retained(BUDGET, Duration::from_millis(800), false)
                    .await?,
                None
            );
        }
        let stopped = tokio::time::timeout(BUDGET, next.stopped()).await??;
        assert_eq!(
            stopped,
            if scenario == 2 {
                None
            } else {
                Some(1u32.into())
            }
        );
        subscriber.clear_retained_receive();
        assert!(matches!(
            subscriber.recv(Duration::from_millis(10)).await,
            Err(Error::Timeout)
        ));
    }
    Ok(())
}

/// 三条旧流停在头部，第四条完整独立帧无需等待旧流驻留到期。
#[tokio::test]
async fn concurrent_recovery_bypasses_stalled_headers_and_stops_losers() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
    )?;
    let mut stalled = Vec::new();
    for _ in 0..3 {
        let mut stream = pair.incoming.open_uni().await?;
        stream.write_all(&[0]).await?;
        stalled.push(stream);
    }
    let key = Object {
        sequence: 4,
        timestamp_us: 0,
        duration_us: 66667,
        independent: true,
        payload: vec![42; 100],
    };
    let receipt = publisher.start_send(&key, BUDGET).await?;
    let received = tokio::time::timeout(
        Duration::from_millis(250),
        subscriber.recv_recovery_concurrent(BUDGET, Duration::from_millis(800)),
    )
    .await??;
    assert_eq!(received, Some(key));
    for stream in stalled {
        assert_eq!(
            tokio::time::timeout(BUDGET, stream.stopped()).await??,
            Some(1u32.into())
        );
    }
    receipt.finish().await?;
    Ok(())
}

/// 四槽已满不接受第五条；超时/外层取消均同步STOP，后续正常读取不受污染。
#[tokio::test]
async fn concurrent_recovery_caps_slots_and_cleans_up_on_cancellation() -> Result<(), TestError> {
    for cancel in [false, true] {
        let pair = pair(ALPN).await?;
        let (publisher, mut subscriber) = tokio::try_join!(
            peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
            peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
        )?;
        let mut stalled = Vec::new();
        for _ in 0..4 {
            let mut stream = pair.incoming.open_uni().await?;
            stream.write_all(&[0]).await?;
            stalled.push(stream);
        }
        let key = Object {
            sequence: 5,
            timestamp_us: 0,
            duration_us: 66667,
            independent: true,
            payload: vec![43],
        };
        let receipt = publisher.start_send(&key, BUDGET).await?;
        if cancel {
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(50),
                    subscriber.recv_recovery_concurrent(BUDGET, Duration::from_millis(800))
                )
                .await
                .is_err()
            );
        } else {
            assert!(matches!(
                subscriber
                    .recv_recovery_concurrent(Duration::from_millis(50), Duration::from_millis(800))
                    .await,
                Err(Error::Timeout)
            ));
        }
        for stream in stalled {
            assert_eq!(
                tokio::time::timeout(BUDGET, stream.stopped()).await??,
                Some(1u32.into())
            );
        }
        assert_eq!(subscriber.recv(BUDGET).await?, key);
        receipt.finish().await?;
    }
    Ok(())
}

#[tokio::test]
async fn concurrent_recovery_preserves_framing_errors_and_residence() -> Result<(), TestError> {
    for scenario in 0..6 {
        let pair = pair(ALPN).await?;
        let (_publisher, mut subscriber) = tokio::try_join!(
            peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
            peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
        )?;
        let mut stream = pair.incoming.open_uni().await?;
        let mut header = vec![0; 25];
        header[20] = 1;
        header[21..25]
            .copy_from_slice(&(if scenario == 0 { u32::MAX } else { 3u32 }).to_be_bytes());
        stream.write_all(&header).await?;
        match scenario {
            0 | 1 => {
                stream.write_all(&[1]).await?;
                stream.finish()?;
            }
            2 => stream.reset(1u32.into())?,
            3 => stream.reset(2u32.into())?,
            5 => {
                stream.write_all(&[1, 2, 3]).await?;
            } // 完整负载但无FIN仍不得交付。
            _ => {}
        }
        let result = subscriber
            .recv_recovery_concurrent(BUDGET, Duration::from_millis(30))
            .await;
        match scenario {
            0 => assert!(matches!(result, Err(Error::Protocol("object header")))),
            1 | 3 => assert!(matches!(result, Err(Error::Io(_)))),
            _ => assert_eq!(result?, None),
        }
    }
    Ok(())
}
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
    use peri_native::media::{Catalog, Frame, FrameKind, Publisher, Subscriber, Track, TrackKind};
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
    // An abandoned publisher object must not terminate relay ingestion.
    // 发布端局部取消对象后，Relay 必须继续接收新的完整关键帧。
    let mut abandoned = incoming.outgoing.open_uni().await?;
    abandoned.write_all(&[0]).await?;
    abandoned.reset(1u32.into())?;
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
    assert!(second_viewer.request_keyframe(99).await.is_err());
    // 清理先前取消对象的反馈，避免把旧请求误当作本次定位请求。
    while source.poll_recovery()?.is_some() {}
    tokio::time::sleep(Duration::from_millis(220)).await;
    // 让接入端停在半个媒体对象上。恢复必须无需下一帧即可往返，且唤醒不能丢掉已读前缀。
    let pending = frame(7, FrameKind::Key, vec![]);
    let bytes = pending.to_object(first_viewer.catalog())?.encode_packet()?;
    let mut partial = incoming.outgoing.open_uni().await?;
    partial.write_all(&bytes[..30]).await?;
    tokio::time::sleep(Duration::from_millis(30)).await;
    second_viewer.request_keyframe_after(1, 6).await?;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(request) = source.poll_recovery()?
                && request.after_sequence == 6
            {
                assert_eq!(request.track, 1);
                assert_eq!(
                    request.viewer, 1,
                    "Relay must assign the actual second subscriber scope"
                );
                break Ok::<_, TestError>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??;
    partial.write_all(&bytes[30..]).await?;
    partial.finish()?;
    assert_eq!(first_viewer.recv(BUDGET).await?, pending);
    assert_eq!(second_viewer.recv(BUDGET).await?, pending);
    first_viewer
        .send_report(peri_native::ReceiverReport {
            track: 1,
            viewer: 999,
            sample: 1,
            sequence: 6,
            media_us: 200000,
            buffered_ms: 0,
            queued_bytes: 0,
            lateness_ms: 100,
            dropped: 1,
            recoveries: 1,
        })
        .await?;
    for sequence in 21..=24 {
        let unit = frame(sequence, FrameKind::Key, vec![]);
        source.send(&unit, BUDGET).await?;
        assert_eq!(first_viewer.recv(BUDGET).await?, unit);
        assert_eq!(second_viewer.recv(BUDGET).await?, unit);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        source.poll_report()?.is_none(),
        "multi-viewer telemetry must not become a global single-viewer target"
    );

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
            let mut preface = *b"PERI\x00\x02\x00\x00";
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
    let old = self::pair(b"peri-dev-01").await?;
    assert!(matches!(
        peri_native::subscribe(old.outgoing.clone(), hello(), "live/test", BUDGET).await,
        Err(Error::Protocol("ALPN mismatch"))
    ));
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
#[tokio::test]
async fn keyframe_feedback_is_role_checked_and_coalesced() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET),
    )?;
    assert!(publisher.request_keyframe(1).await.is_err());
    assert!(subscriber.request_keyframe(0).await.is_err());
    for _ in 0..20 {
        subscriber.request_keyframe(1).await?;
    }
    let received = tokio::time::timeout(BUDGET, async {
        loop {
            if let Some(track) = publisher.poll_keyframe_request() {
                break track;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(received, 1);
    assert_eq!(publisher.poll_keyframe_request(), None);
    Ok(())
}
#[tokio::test]
async fn receiver_reports_enforce_role_bounds_and_reach_sender() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
    )?;
    let mut report = peri_native::ReceiverReport {
        track: 1,
        viewer: 0,
        sample: 1,
        sequence: 8,
        media_us: 466666,
        buffered_ms: 20,
        queued_bytes: 1024,
        lateness_ms: 15,
        dropped: 0,
        recoveries: 0,
    };
    assert!(publisher.send_report(report).await.is_err());
    report.lateness_ms = 60001;
    assert!(subscriber.send_report(report).await.is_err());
    report.lateness_ms = 15;
    subscriber.send_report(report).await?;
    let received = tokio::time::timeout(BUDGET, async {
        loop {
            if let Some(report) = publisher.poll_report() {
                break report;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(received.sequence, 8);
    assert_eq!(received.queued_bytes, 1024);
    assert!(received.valid());
    Ok(())
}

#[tokio::test]
async fn recovery_skips_stalled_dependency_without_waiting_for_payload() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET),
    )?;
    // A valid dependent header advertises 1MiB but never sends its payload or FIN.
    let mut slow = pair.incoming.open_uni().await?;
    let mut header = vec![0; 25];
    header[21..25].copy_from_slice(&(1024u32 * 1024).to_be_bytes());
    slow.write_all(&header).await?;
    assert_eq!(
        tokio::time::timeout(
            Duration::from_millis(250),
            subscriber.recv_recovery(BUDGET, Duration::from_secs(1))
        )
        .await??,
        None
    );
    assert_eq!(
        tokio::time::timeout(BUDGET, slow.stopped()).await??,
        Some(1u32.into())
    );
    let object = Object {
        sequence: 2,
        timestamp_us: 0,
        duration_us: 20000,
        independent: true,
        payload: vec![42],
    };
    let (_, received) = tokio::try_join!(
        publisher.send(&object, BUDGET),
        subscriber.recv_recovery(BUDGET, Duration::from_secs(1))
    )?;
    assert_eq!(received, Some(object));
    // Header validation cannot be bypassed by marking an object dependent.
    header[21..25].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut invalid = pair.incoming.open_uni().await?;
    invalid.write_all(&header).await?;
    invalid.finish()?;
    assert!(matches!(
        subscriber
            .recv_recovery(BUDGET, Duration::from_secs(1))
            .await,
        Err(Error::Protocol("object header"))
    ));
    // Recovery mode must still reject malformed retained objects, including trailing data.
    header[20] = 1;
    header[21..25].copy_from_slice(&1u32.to_be_bytes());
    let mut trailing = pair.incoming.open_uni().await?;
    trailing.write_all(&header).await?;
    trailing.write_all(&[42, 0]).await?;
    trailing.finish()?;
    assert!(matches!(
        subscriber
            .recv_recovery(BUDGET, Duration::from_secs(1))
            .await,
        Err(Error::Protocol("object trailing data"))
    ));
    let mut unfinished = pair.incoming.open_uni().await?;
    unfinished.write_all(&header).await?;
    unfinished.write_all(&[42]).await?;
    assert_eq!(
        subscriber
            .recv_recovery(BUDGET, Duration::from_millis(30))
            .await?,
        None
    );
    assert_eq!(
        tokio::time::timeout(BUDGET, unfinished.stopped()).await??,
        Some(1u32.into())
    );
    assert!(subscriber.agreement().is_some());
    Ok(())
}

/// 同一AV1夹具通过原生PERI传输，输出交给共用的外部解码校验。
#[tokio::test]
#[ignore = "由native:interop:check提供AV1夹具和输出位置"]
async fn shared_av1_fixture_crosses_native() -> Result<(), TestError> {
    let (catalog, frames) = peri_native::av1::read_gop_ivf(
        &std::fs::read(std::env::var("PERI_AV1_FIXTURE")?)?,
        &std::fs::read(std::env::var("PERI_AV1_PROBE")?)?,
    )?;
    let pair = pair(ALPN).await?;
    let (publisher, mut subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
    )?;
    let mut playback = playback_check::PlaybackCheck::new(
        catalog.clone(),
        publisher.agreement().ok_or("missing agreement")?,
        std::env::var("PERI_NATIVE_RGB")?,
    )?;
    let mut writer = peri_native::av1::IvfWriter::new(
        std::fs::File::create(std::env::var("PERI_NATIVE_OUTPUT")?)?,
        catalog.tracks[0].clone(),
    )?;
    for expected in &frames {
        let object = expected.to_object(&catalog)?;
        let (_, object) =
            tokio::try_join!(publisher.send(&object, BUDGET), subscriber.recv(BUDGET))?;
        let restored = peri_native::media::Frame::from_object(object, &catalog)?;
        assert_eq!(&restored, expected);
        playback.render(restored.clone()).await?;
        writer.push(&restored)?;
    }
    assert_eq!(playback.frames, frames.len());
    writer.finish()?;
    println!("SHARED_AV1_NATIVE_FRAMES {}", frames.len());
    Ok(())
}

/// 新恢复字段必须跨真实控制流保真，不能只在本地策略单元测试中生效。
#[tokio::test]
async fn recovery_position_crosses_real_quic() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (publisher, subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
    )?;
    subscriber.request_keyframe_after(1, 42).await?;
    let request = tokio::time::timeout(BUDGET, async {
        loop {
            if let Some(request) = publisher.poll_recovery() {
                break request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(request.track, 1);
    assert_eq!(request.viewer, 0);
    assert_eq!(request.after_sequence, 42);
    assert!(request.id > 0);
    Ok(())
}

// 零预算不能开流；已收到ACK也不能让过期句柄重新成功。
// Zero budget opens no stream; an available ACK cannot revive an expired receipt.
#[tokio::test]
async fn object_deadline_covers_admission_and_delayed_receipt() -> Result<(), TestError> {
    let pair = pair(ALPN).await?;
    let (sender, mut receiver) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/deadline", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/deadline", BUDGET)
    )?;
    let object = Object {
        sequence: 1,
        timestamp_us: 0,
        duration_us: 66667,
        independent: true,
        payload: vec![42; 100],
    };
    assert!(matches!(
        sender.start_send(&object, Duration::ZERO).await,
        Err(Error::Timeout)
    ));
    assert!(matches!(
        sender.start_send(&object, Duration::MAX).await,
        Err(Error::Protocol("deadline overflow"))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), pair.outgoing.accept_uni())
            .await
            .is_err()
    );
    let budget = Duration::from_millis(200);
    let receipt = sender.start_send(&object, budget).await?;
    assert_eq!(receiver.recv(BUDGET).await?, object);
    tokio::time::sleep(budget + Duration::from_millis(20)).await;
    assert!(matches!(receipt.finish().await, Err(Error::Timeout)));
    // 超期句柄释放发送槽，后续对象仍能收发。
    // Expired receipts release capacity for subsequent objects.
    let next = Object {
        sequence: 2,
        ..object
    };
    let (_, got) = tokio::try_join!(sender.send(&next, BUDGET), receiver.recv(BUDGET))?;
    assert_eq!(got, next);
    Ok(())
}
