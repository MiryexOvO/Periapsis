use peri_native::{ALPN, Error, Object, quinn};
use peri_types::{Extension, Hello, PERI_VERSION, Profile};
use std::{sync::Arc, time::Duration};

type TestError = Box<dyn std::error::Error + Send + Sync>;
const BUDGET: Duration = Duration::from_secs(3);
#[tokio::test]
async fn media_catalog_and_dependency_frames_cross_real_quic() -> Result<(), TestError> {
    use peri_native::media::{
        Catalog, DecodeBuffer, Event, Frame, FrameKind, Publisher, Subscriber, Track, TrackKind,
    };
    let pair = pair(ALPN).await?;
    let (publisher, subscriber) = tokio::try_join!(
        peri_native::accept(pair.incoming.clone(), hello(), "live/media", BUDGET),
        peri_native::subscribe(pair.outgoing.clone(), hello(), "live/media", BUDGET),
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

struct Pair {
    client: quinn::Endpoint,
    server: quinn::Endpoint,
    outgoing: quinn::Connection,
    incoming: quinn::Connection,
}
impl Drop for Pair {
    fn drop(&mut self) {
        self.client.close(0u32.into(), b"test finished");
        self.server.close(0u32.into(), b"test finished");
    }
}
async fn pair(alpn: &[u8]) -> Result<Pair, TestError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let der = cert.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key.into())?;
    tls.alpn_protocols = vec![alpn.to_vec()];
    let server = quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
        )),
        "127.0.0.1:0".parse()?,
    )?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der)?;
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![alpn.to_vec()];
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    )));
    let (incoming, outgoing) = tokio::try_join!(
        async { Ok::<_, TestError>(server.accept().await.ok_or("server closed")?.await?) },
        async { Ok::<_, TestError>(client.connect(server.local_addr()?, "localhost")?.await?) },
    )?;
    Ok(Pair {
        server,
        client,
        incoming,
        outgoing,
    })
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
