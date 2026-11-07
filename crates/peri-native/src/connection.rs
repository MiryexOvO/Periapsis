//! Address-independent publish codes with explicit certificate pinning.
//! 推流码包含凭据和证书指纹，不包含地址，不要求用户管理证书文件。
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::Arc};

const PREFIX: &str = "peri1_";
type Error = Box<dyn std::error::Error + Send + Sync>;

// Intentionally no Debug: the credential must not enter logs.
pub struct PublishCode {
    pub fingerprint: [u8; 32],
    pub token: String,
    pub name: String,
    pub resource: String,
}
impl PublishCode {
    pub fn new(cert: &[u8], token: String, name: String, resource: String) -> Result<Self, Error> {
        let code = Self {
            fingerprint: Sha256::digest(cert).into(),
            token,
            name,
            resource,
        };
        code.validate()?;
        Ok(code)
    }
    fn validate(&self) -> Result<(), Error> {
        crate::auth::validate_token(&self.token)?;
        ServerName::try_from(self.name.clone()).map_err(|_| "invalid server identity")?;
        if self.name.len() > 253
            || self.resource.is_empty()
            || self.resource.len() > 256
            || !self
                .resource
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
        {
            return Err("invalid publish code fields".into());
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<String, Error> {
        self.validate()?;
        let mut bytes = self.fingerprint.to_vec();
        for text in [&self.token, &self.name, &self.resource] {
            bytes.extend_from_slice(&(text.len() as u16).to_be_bytes());
            bytes.extend_from_slice(text.as_bytes());
        }
        Ok(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
    }
    pub fn decode(text: &str) -> Result<Self, Error> {
        if text.len() > 1024 {
            return Err("推流码过长 / Publish code too long".into());
        }
        let encoded = text
            .trim()
            .strip_prefix(PREFIX)
            .ok_or("请粘贴服务器生成的 peri1_ 推流码 / Expected generated publish code")?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "推流码格式错误 / Invalid publish code")?;
        let fingerprint = bytes.get(..32).ok_or("invalid publish code")?.try_into()?;
        let mut rest = &bytes[32..];
        let mut read = || -> Result<String, Error> {
            let size = u16::from_be_bytes(rest.get(..2).ok_or("invalid publish code")?.try_into()?)
                as usize;
            rest = &rest[2..];
            let text =
                std::str::from_utf8(rest.get(..size).ok_or("invalid publish code")?)?.to_owned();
            rest = &rest[size..];
            Ok(text)
        };
        let code = Self {
            fingerprint,
            token: read()?,
            name: read()?,
            resource: read()?,
        };
        if !rest.is_empty() {
            return Err("unexpected publish code data".into());
        }
        code.validate()?;
        Ok(code)
    }
    pub fn client_config(&self) -> Result<rustls::ClientConfig, Error> {
        self.validate()?;
        let mut tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedCertificate(self.fingerprint)))
            .with_no_client_auth();
        tls.alpn_protocols = vec![crate::ALPN.to_vec()];
        Ok(tls)
    }
}

pub fn parse_server(text: &str) -> Result<SocketAddr, Error> {
    let addr: SocketAddr = text
        .trim()
        .strip_prefix("peri://")
        .ok_or("服务器格式应为 peri://IP:端口 / Expected peri://IP:port")?
        .parse()
        .map_err(|_| "服务器格式应为 peri://IP:端口 / Expected peri://IP:port")?;
    if addr.ip().is_unspecified() || addr.ip().is_multicast() || addr.port() == 0 {
        return Err("请填写可访问的服务器 IP 和公网端口 / Invalid server address".into());
    }
    Ok(addr)
}

#[derive(Debug)]
struct PinnedCertificate([u8; 32]);
impl ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // The exact leaf certificate is the trust anchor conveyed out of band in the code.
        // TLS CertificateVerify below still proves possession of its private key.
        let actual: [u8; 32] = Sha256::digest(cert.as_ref()).into();
        if actual != self.0 {
            return Err(rustls::Error::General(
                "服务器身份不匹配，请确认地址或重新取得推流码 / Server identity mismatch".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
