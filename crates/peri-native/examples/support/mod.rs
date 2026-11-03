//! Local TLS fixtures shared by the example and integration tests.
//! 示例与集成测试共用的本地 TLS 测试设施，不用于公网部署。
use peri_native::quinn;
use std::sync::Arc;

pub struct Pair {
    pub client: quinn::Endpoint,
    pub server: quinn::Endpoint,
    pub outgoing: quinn::Connection,
    pub incoming: quinn::Connection,
}
impl Drop for Pair {
    fn drop(&mut self) {
        self.client.close(0u32.into(), b"test finished");
        self.server.close(0u32.into(), b"test finished");
    }
}
pub async fn pair(alpn: &[u8]) -> Result<Pair, Box<dyn std::error::Error + Send + Sync>> {
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
        async {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                server.accept().await.ok_or("server closed")?.await?,
            )
        },
        async {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                client.connect(server.local_addr()?, "localhost")?.await?,
            )
        },
    )?;
    Ok(Pair {
        server,
        client,
        incoming,
        outgoing,
    })
}
