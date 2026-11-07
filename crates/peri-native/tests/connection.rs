use peri_native::{
    connection::{PublishCode, parse_server},
    quinn,
};
use peri_types::{Hello, PERI_VERSION, Profile};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
type Error = Box<dyn std::error::Error + Send + Sync>;

#[test]
fn code_is_address_independent_bounded_and_round_trips() -> Result<(), Error> {
    let code = PublishCode::new(
        b"certificate",
        "p".repeat(64),
        "localhost".into(),
        "live/av1".into(),
    )?;
    let text = code.encode()?;
    assert!(text.len() < 200);
    let parsed = PublishCode::decode(&text)?;
    assert_eq!(parsed.token, code.token);
    assert_eq!(parsed.fingerprint, code.fingerprint);
    assert_eq!(parsed.resource, code.resource);
    assert_eq!(parse_server("peri://43.248.128.243:23927")?.port(), 23927);
    assert_eq!(parse_server("peri://127.0.0.1:4444")?.port(), 4444);
    assert_eq!(parse_server("peri://[::1]:4444")?.port(), 4444);
    for invalid in [
        "peri://0.0.0.0:4444",
        "peri://127.0.0.1:0",
        "https://127.0.0.1:4444",
        "peri://127.0.0.1",
        "peri://127.0.0.1:4444/path",
    ] {
        assert!(parse_server(invalid).is_err());
    }
    for i in 0..text.len() {
        assert!(PublishCode::decode(&text[..i]).is_err());
    }
    assert!(PublishCode::decode(&format!("{text}AAAA")).is_err());
    assert!(PublishCode::decode(&"x".repeat(2000)).is_err());
    Ok(())
}

async fn authenticate(wrong_pin: bool, wrong_token: bool) -> Result<(bool, bool, bool), Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let cert = generated.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der());
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key.into())?;
    tls.alpn_protocols = vec![peri_native::ALPN.to_vec()];
    let server = quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
        )),
        "127.0.0.1:0".parse()?,
    )?;
    let mut code = PublishCode::new(
        cert.as_ref(),
        if wrong_token {
            "x".repeat(64)
        } else {
            "p".repeat(64)
        },
        "localhost".into(),
        "live/test".into(),
    )?;
    if wrong_pin {
        code.fingerprint[0] ^= 1;
    }
    let code = PublishCode::decode(&code.encode()?)?;
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(code.client_config()?)?,
    )));
    let reached_auth = Arc::new(AtomicBool::new(false));
    let budget = Duration::from_secs(3);
    let hello = || Hello {
        version: PERI_VERSION,
        profiles: vec![Profile::Live],
        codecs: vec!["video/av1".into()],
        extensions: vec![],
    };
    let server_run = async {
        let connection = server.accept().await.ok_or("missing connection")?.await?;
        reached_auth.store(true, Ordering::SeqCst);
        let creds = peri_native::auth::Credentials::new("p".repeat(64), "s".repeat(64))?;
        peri_native::accept_authenticated(connection, hello(), "live/test", &creds, budget)
            .await
            .map_err(|e| -> Error { Box::new(e) })
    };
    let client_run = async {
        let connection = client.connect(server.local_addr()?, &code.name)?.await?;
        peri_native::publish_authenticated(connection, hello(), &code.resource, &code.token, budget)
            .await
            .map_err(|e| -> Error { Box::new(e) })
    };
    let (a, b) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server_run, client_run)
    })
    .await?;
    let result = (a.is_ok(), b.is_ok(), reached_auth.load(Ordering::SeqCst));
    if wrong_token && !wrong_pin {
        assert!(
            b.as_ref()
                .err()
                .is_some_and(|e| e.to_string().contains("PERI_AUTH_DENIED"))
        );
    }
    client.close(0u32.into(), b"test complete");
    server.close(0u32.into(), b"test complete");
    Ok(result)
}
#[tokio::test]
async fn pinned_server_accepts_valid_code_over_real_quic() -> Result<(), Error> {
    assert_eq!(authenticate(false, false).await?, (true, true, true));
    Ok(())
}
#[tokio::test]
async fn wrong_server_identity_fails_before_credentials() -> Result<(), Error> {
    assert_eq!(authenticate(true, false).await?, (false, false, false));
    Ok(())
}
#[tokio::test]
async fn wrong_publishing_credential_is_rejected() -> Result<(), Error> {
    let (server, client, reached) = authenticate(false, true).await?;
    assert!(!server && !client && reached);
    Ok(())
}
