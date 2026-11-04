#[cfg(feature = "media-interop")]
#[path = "../../peri-native/examples/support/playback_check.rs"]
mod playback_check;
use peri_transport::{ALPN, moq};
use std::{sync::Arc, time::Duration};
use web_transport_quinn::quinn;

/// Verify actual encrypted UDP object delivery with certificate validation enabled.
/// 启用证书校验，通过真实 UDP 加密连接验证媒体对象完整性。
#[tokio::test]
async fn native_moqt22_delivers_an_object() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    tokio::time::timeout(Duration::from_secs(15), async {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
        let der = cert.cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![der.clone()], key.into())?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let server = quinn::Endpoint::server(
            quinn::ServerConfig::with_crypto(Arc::new(crypto)),
            "127.0.0.1:0".parse()?,
        )?;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(der)?;
        let mut tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut client = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
        client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
        )));

        let (published, pub_driver) =
            moq::origin::Producer::new(moq::origin::Config::new(moq::Hop::new(1).unwrap()));
        let (received, sub_driver) =
            moq::origin::Producer::new(moq::origin::Config::new(moq::Hop::new(2).unwrap()));
        let pub_task = tokio::spawn(moq::time::run(pub_driver));
        let sub_task = tokio::spawn(moq::time::run(sub_driver));
        let broadcast = published.create_broadcast("peri/native")?;
        let track = broadcast.create_track("probe", None)?;
        broadcast.announce(Default::default())?;
        let incoming = async {
            let connection = server.accept().await.ok_or("server closed")?.await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                peri_transport::accept(connection, &published).await?,
            )
        };
        let outgoing = async {
            let connection = client.connect(server.local_addr()?, "localhost")?.await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                peri_transport::connect(connection, received.clone()).await?,
            )
        };
        let (publisher, subscriber) = tokio::try_join!(incoming, outgoing)?;
        assert_eq!(subscriber.inner.version().as_str(), "moq-transport-22");
        assert_eq!(publisher.inner.version().as_str(), "moq-transport-22");
        let consumer = received.consume();
        consumer
            .routed("peri/native")
            .await
            .ok_or("missing broadcast route")?;
        let remote = consumer.request_broadcast("peri/native").await?;
        let subscription =
            moq::track::Subscription::default().with_start(moq::track::Position::group(0));
        let mut reader = remote.track("probe")?.subscribe(subscription).await?;
        track.used().await?;
        let payload: Vec<u8> = (0..65536).map(|i| (i % 251) as u8).collect();
        let mut group = track.append_group()?;
        group.write_frame(moq::Timestamp::ZERO, payload.clone())?;
        group.finish()?;
        let mut received_group = reader.recv_group().await?.ok_or("missing group")?;
        let frame = received_group.read_frame().await?.ok_or("missing frame")?;
        assert_eq!(frame.payload.as_ref(), payload.as_slice());
        #[cfg(feature = "media-interop")]
        {
            // 可选真实AV1夹具由验收脚本提供；普通回归使用小对象检查封装与GOP分组。
            use peri_native::media::{Catalog, Frame, FrameKind, Track, TrackKind};
            let fixture = std::env::var("PERI_AV1_FIXTURE").ok();
            let (catalog, frames) = if let Some(path) = fixture {
                peri_native::av1::read_gop_ivf(
                    &std::fs::read(path)?,
                    &std::fs::read(std::env::var("PERI_AV1_PROBE")?)?,
                )?
            } else {
                (
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
                    },
                    (1..=4)
                        .map(|sequence| Frame {
                            track: 1,
                            sequence,
                            timestamp_us: (sequence - 1) * 66667,
                            duration_us: 66667,
                            kind: if sequence % 2 == 1 {
                                FrameKind::Key
                            } else {
                                FrameKind::Reference
                            },
                            dependencies: if sequence % 2 == 1 {
                                vec![]
                            } else {
                                vec![sequence - 1]
                            },
                            payload: vec![sequence as u8; 32],
                        })
                        .collect(),
                )
            };
            let mut groups = Vec::new();
            for frame in &frames {
                if frame.kind == FrameKind::Key {
                    groups.push(Vec::new());
                }
                groups
                    .last_mut()
                    .ok_or("fixture must start with key")?
                    .push(frame);
            }
            let mut playback = if let Ok(path) = std::env::var("PERI_MOQ_RGB") {
                let hello = peri_types::Hello {
                    version: peri_types::PERI_VERSION,
                    profiles: vec![peri_types::Profile::Live],
                    codecs: vec!["video/av1".into()],
                    extensions: vec![],
                };
                let agreement = peri_session::negotiate(&hello, &hello)?;
                Some(playback_check::PlaybackCheck::new(
                    catalog.clone(),
                    &agreement,
                    path,
                )?)
            } else {
                None
            };
            let mut delivered = Vec::new();
            // 正确性回归逐组生产并收取，避免上游最新优先策略主动跳过旧组。
            // 这不是实时性能对照；实时播放器必须另行实现有界跨组调度。
            for frames_in_group in &groups {
                let mut group = track.append_group()?;
                for frame in frames_in_group {
                    group.write_frame(
                        moq::Timestamp::from_micros(frame.timestamp_us)?,
                        peri_transport::media_interop::encode_frame(frame, &catalog)?,
                    )?;
                }
                group.finish()?;
                let mut group = reader.recv_group().await?.ok_or("missing media group")?;
                while let Some(received) = group.read_frame().await? {
                    if delivered.len() >= frames.len() {
                        return Err("too many media frames".into());
                    }
                    let restored = peri_transport::media_interop::decode_frame(
                        received.payload.as_ref(),
                        &catalog,
                    )
                    .await?;
                    if let Some(playback) = playback.as_mut() {
                        playback.render(restored.clone()).await?;
                    }
                    delivered.push(restored);
                }
            }
            if let Some(playback) = &playback {
                assert_eq!(playback.frames, frames.len());
            }
            delivered.sort_by_key(|frame| frame.sequence);
            assert_eq!(delivered, frames);
            if let Ok(path) = std::env::var("PERI_MOQ_OUTPUT") {
                let mut writer = peri_native::av1::IvfWriter::new(
                    std::fs::File::create(path)?,
                    catalog.tracks[0].clone(),
                )?;
                for frame in &delivered {
                    writer.push(frame)?;
                }
                writer.finish()?;
            }
            let encoded = peri_transport::media_interop::encode_frame(&frames[0], &catalog)?;
            let mut trailing = encoded.clone();
            trailing.push(0);
            assert!(
                peri_transport::media_interop::decode_frame(&trailing, &catalog)
                    .await
                    .is_err()
            );
            assert!(
                peri_transport::media_interop::decode_frame(
                    &encoded[..encoded.len() - 1],
                    &catalog
                )
                .await
                .is_err()
            );
            let mut bad = encoded.clone();
            bad[20] ^= 1;
            assert!(
                peri_transport::media_interop::decode_frame(&bad, &catalog)
                    .await
                    .is_err()
            );
            let mut oversized = encoded;
            oversized[21..25].copy_from_slice(&u32::MAX.to_be_bytes());
            assert!(
                peri_transport::media_interop::decode_frame(&oversized, &catalog)
                    .await
                    .is_err()
            );
            // 在真实MoQT链路上让旧组停在半帧，再验证并发接收新组。
            use peri_transport::media_interop::{LiveEvent, LiveLimits, LiveReceiver};
            let mut live = LiveReceiver::new(reader, catalog.clone(), 1, LiveLimits::default())?;
            let mut stalled_group = track.append_group()?;
            let stalled = stalled_group.create_frame(moq::frame::Info {
                size: 1000,
                timestamp: moq::Timestamp::ZERO,
            })?;
            let mut fresh = frames[0].clone();
            fresh.sequence = frames.last().ok_or("empty fixture")?.sequence + 1;
            fresh.timestamp_us = frames.last().ok_or("empty fixture")?.timestamp_us + 66667;
            let mut next_group = track.append_group()?;
            next_group.write_frame(
                moq::Timestamp::from_micros(fresh.timestamp_us)?,
                peri_transport::media_interop::encode_frame(&fresh, &catalog)?,
            )?;
            next_group.finish()?;
            let packet = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    match live.recv().await.ok_or("live receiver closed")?? {
                        LiveEvent::Frame(packet) => {
                            break Ok::<_, Box<dyn std::error::Error + Send + Sync>>(packet);
                        }
                        LiveEvent::Gap { .. } => {}
                    }
                }
            })
            .await??;
            assert_eq!(packet.frame(), &fresh);
            // 相同调度/解码器继续处理恢复关键帧；额外输出独立文件，不改变30帧完整性基准。
            if let Ok(path) = std::env::var("PERI_MOQ_LIVE_RGB") {
                let hello = peri_types::Hello {
                    version: peri_types::PERI_VERSION,
                    profiles: vec![peri_types::Profile::Live],
                    codecs: vec!["video/av1".into()],
                    extensions: vec![],
                };
                let agreement = peri_session::negotiate(&hello, &hello)?;
                let mut playback =
                    playback_check::PlaybackCheck::new(catalog.clone(), &agreement, path)?;
                let deadline = packet.deadline;
                playback
                    .render_before(packet.into_frame(), deadline)
                    .await?;
                assert_eq!(playback.frames, 1);
            } else {
                drop(packet);
            }
            drop(stalled);
            drop(live);
            println!("SHARED_AV1_MOQ_FRAMES {}", delivered.len());
        }
        drop((subscriber, publisher));
        pub_task.abort();
        sub_task.abort();
        client.close(0u32.into(), b"done");
        server.close(0u32.into(), b"done");
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    Ok(())
}
