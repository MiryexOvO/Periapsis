use peri_transport::{ALPN, moq};
use std::{sync::Arc, time::Duration};
use web_transport_quinn::quinn;

/// Verify actual encrypted UDP object delivery with certificate validation enabled.
/// 启用证书校验，通过真实 UDP 加密连接验证媒体对象完整性。
#[derive(Clone, Copy)]
enum Scenario {
    Success,
    Incompatible,
    Missing,
}

async fn round(mode: Scenario) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
                peri_transport::duplex(connection, published.clone(), true).await?,
            )
        };
        let outgoing = async {
            let connection = client.connect(server.local_addr()?, "localhost")?.await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                peri_transport::duplex(connection, received.clone(), false).await?,
            )
        };
        let (publisher, subscriber) = tokio::try_join!(incoming, outgoing)?;
        assert_eq!(subscriber.inner.version().as_str(), "moq-transport-22");
        assert_eq!(publisher.inner.version().as_str(), "moq-transport-22");
        let id = "0123456789abcdef0123456789abcdef".to_string();
        let hello = || peri_types::Hello {
            version: peri_types::PERI_VERSION,
            profiles: vec![peri_types::Profile::Live],
            codecs: vec!["application/octet-stream".into()],
            extensions: vec![],
        };
        if matches!(mode, Scenario::Missing) {
            let result = peri_transport::peer::negotiate(
                subscriber,
                &received,
                peri_session::handshake::Role::Initiator,
                id,
                hello(),
                Duration::from_millis(100),
            )
            .await;
            assert!(result.is_err());
            drop(publisher);
            pub_task.abort();
            sub_task.abort();
            return Ok(());
        }
        let mut remote_hello = hello();
        if matches!(mode, Scenario::Incompatible) {
            remote_hello.codecs = vec!["video/av1".into()];
        }
        let (publisher, subscriber) = tokio::join!(
            peri_transport::peer::negotiate(
                publisher,
                &published,
                peri_session::handshake::Role::Responder,
                id.clone(),
                remote_hello,
                Duration::from_secs(1)
            ),
            peri_transport::peer::negotiate(
                subscriber,
                &received,
                peri_session::handshake::Role::Initiator,
                id,
                hello(),
                Duration::from_secs(1)
            )
        );
        if matches!(mode, Scenario::Incompatible) {
            assert!(publisher.is_err());
            assert!(subscriber.is_err());
            pub_task.abort();
            sub_task.abort();
            return Ok(());
        }
        let publisher = publisher?;
        let subscriber = subscriber?;
        assert_eq!(publisher.agreement()?, subscriber.agreement()?);
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
        publisher.session().inner.abort(moq::Error::Closed);
        subscriber.session().inner.closed().await;
        assert!(subscriber.agreement().is_err());
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

#[tokio::test]
async fn native_peri_negotiates_before_object_delivery()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    round(Scenario::Success).await
}

#[tokio::test]
async fn native_peri_rejects_incompatible_capabilities()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    round(Scenario::Incompatible).await
}

#[tokio::test]
async fn native_peri_times_out_without_a_control_peer()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    round(Scenario::Missing).await
}
