//! Native MOQT-22 adapter over authenticated QUIC connections.
//! 基于已认证 QUIC 连接的原生 MOQT-22 适配器，复用上游协议实现。

pub use moq_net as moq;
use std::time::Instant;
use tokio::task::JoinHandle;
use web_transport_quinn::{Session as Transport, quinn};

pub const ALPN: &[u8] = b"moqt-22";

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
}
impl Drop for Session {
    fn drop(&mut self) {
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
    let (inner, driver) = moq::Server::new()
        .with_versions(version().into())
        .with_publisher(origin)
        .accept(Instant::now(), Transport::raw(connection))
        .await?;
    Ok(Session {
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
    let (inner, driver) = moq::Client::new()
        .with_versions(version().into())
        .with_subscriber(origin)
        .connect(Instant::now(), Transport::raw(connection))
        .await?;
    Ok(Session {
        inner,
        driver: tokio::spawn(moq::time::run(driver)),
    })
}
