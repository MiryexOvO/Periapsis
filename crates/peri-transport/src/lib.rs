//! Native MOQT-22 adapter over authenticated QUIC connections.
//! 基于已认证 QUIC 连接的原生 MOQT-22 适配器，复用上游协议实现。

pub use moq_net as moq;
mod compat;
#[cfg(feature = "media-interop")]
pub mod media_interop;
pub mod peer;
use std::time::Instant;
use tokio::task::JoinHandle;
use web_transport_quinn::{Session as Transport, quinn};

pub const ALPN: &[u8] = b"moqt-22";

/// Accept MOQT-22 on an already authorized HTTP/3 WebTransport session.
/// 在已完成访问授权的 HTTP/3 WebTransport 会话上接受 MOQT-22。
pub async fn accept_webtransport(
    transport: Transport,
    origin: &moq::origin::Producer,
) -> Result<Session, moq::Error> {
    let connection = std::ops::Deref::deref(&transport).clone();
    let (inner, driver) = moq::Server::new()
        .with_versions(version().into())
        .with_publisher(origin)
        .accept(Instant::now(), compat::Compat(transport))
        .await?;
    Ok(Session {
        inner,
        driver: tokio::spawn(moq::time::run(driver)),
        connection,
    })
}

fn version() -> moq::Version {
    "moq-transport-22"
        .parse()
        .expect("pinned backend supports draft-22")
}

/// Own the protocol driver for the lifetime of a native session.
/// 原生会话持有协议驱动，释放会话时取消驱动，避免后台任务泄漏。
pub struct Session {
    pub inner: moq::Session,
    driver: JoinHandle<moq::Error>,
    connection: quinn::Connection,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"PERI session closed");
        self.inner.abort(moq::Error::Closed);
        self.driver.abort();
    }
}

/// Accept a QUIC peer and publish the supplied origin over MOQT-22.
/// 在 QUIC 连接上接受对端，并通过 MOQT-22 发布给定的媒体源。
pub async fn accept(
    connection: quinn::Connection,
    origin: &moq::origin::Producer,
) -> Result<Session, moq::Error> {
    let connection_handle = connection.clone();
    let (inner, driver) = moq::Server::new()
        .with_versions(version().into())
        .with_publisher(origin)
        .accept(Instant::now(), compat::Compat(Transport::raw(connection)))
        .await?;
    Ok(Session {
        connection: connection_handle,
        inner,
        driver: tokio::spawn(moq::time::run(driver)),
    })
}

/// Subscribe over a QUIC connection using the pinned MOQT version.
/// 在 QUIC 连接上订阅媒体源，协商范围固定为 MOQT-22。
pub async fn connect(
    connection: quinn::Connection,
    origin: moq::origin::Producer,
) -> Result<Session, moq::Error> {
    let connection_handle = connection.clone();
    let (inner, driver) = moq::Client::new()
        .with_versions(version().into())
        .with_subscriber(origin)
        .connect(Instant::now(), compat::Compat(Transport::raw(connection)))
        .await?;
    Ok(Session {
        connection: connection_handle,
        inner,
        driver: tokio::spawn(moq::time::run(driver)),
    })
}

/// Establish a bidirectional native MOQT connection for PERI control exchange.
/// 建立双向原生 MOQT 连接，供 PERI 控制轨道交换使用。
pub async fn duplex(
    connection: quinn::Connection,
    origin: moq::origin::Producer,
    server: bool,
) -> Result<Session, moq::Error> {
    let handle = connection.clone();
    let transport = compat::Compat(Transport::raw(connection));
    let (inner, driver) = if server {
        moq::Server::new()
            .with_versions(version().into())
            .with_origin(origin)
            .accept(Instant::now(), transport)
            .await?
    } else {
        moq::Client::new()
            .with_versions(version().into())
            .with_origin(origin)
            .connect(Instant::now(), transport)
            .await?
    };
    Ok(Session {
        inner,
        driver: tokio::spawn(moq::time::run(driver)),
        connection: handle,
    })
}
