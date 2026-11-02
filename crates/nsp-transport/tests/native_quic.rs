use nsp_transport::{ALPN, moq};
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
        let broadcast = published.create_broadcast("nsp/native")?;
        let track = broadcast.create_track("probe", None)?;
        broadcast.announce(Default::default())?;
        let incoming = async {
            let connection = server.accept().await.ok_or("server closed")?.await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                nsp_transport::accept(connection, &published).await?,
            )
        };
        let outgoing = async {
            let connection = client.connect(server.local_addr()?, "localhost")?.await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                nsp_transport::connect(connection, received.clone()).await?,
            )
        };
        let (publisher, subscriber) = tokio::try_join!(incoming, outgoing)?;
        assert_eq!(subscriber.inner.version().as_str(), "moq-transport-22");
        assert_eq!(publisher.inner.version().as_str(), "moq-transport-22");
        let consumer = received.consume();
        consumer.routed("nsp/native").await?;
        let remote = consumer.request_broadcast("nsp/native").await?;
        let subscription =
            moq::track::Subscription::default().with_start(moq::track::Position::group(0));
        let mut reader = remote.track("probe")?.subscribe(subscription).await?;
        track.used().await?;
        let payload = vec![0x5a_u8; 65536];
        let mut group = track.append_group()?;
        group.write_frame(moq::Timestamp::ZERO, payload.clone())?;
        group.finish()?;
        let mut received_group = reader.recv_group().await?.ok_or("missing group")?;
        let frame = received_group.read_frame().await?.ok_or("missing frame")?;
        assert_eq!(frame.payload.as_ref(), payload.as_slice());
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
