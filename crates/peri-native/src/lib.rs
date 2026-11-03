//! Experimental PERI native subscriber/publisher over QUIC.
//! 实验原生 PERI 发布订阅：独立 ALPN、控制封装和对象流，不提供公网鉴权。

pub mod media;
mod wire;
use peri_types::{Agreement, Hello};
pub use quinn;
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
use tokio::io::AsyncWriteExt;

pub const ALPN: &[u8] = b"peri-dev-01";
pub const MAX_OBJECT_BYTES: usize = wire::MAX_PAYLOAD;

#[derive(Debug)]
pub enum Error {
    Protocol(&'static str),
    Transport(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Timeout,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PERI native: {self:?}")
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
fn transport(error: impl fmt::Display) -> Error {
    Error::Transport(error.to_string())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Control {
    #[serde(rename = "peri.subscribe")]
    Subscribe { resource: String, hello: Hello },
    #[serde(rename = "peri.accept")]
    Accept { hello: Hello, agreement: Agreement },
    #[serde(rename = "peri.ack")]
    Ack,
}

/// One encoded media unit; timestamps share the publisher's media timeline.
/// 单个编码媒体单元；时间戳属于发布端媒体时间轴，不用于推断跨端单向延迟。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub sequence: u64,
    pub timestamp_us: u64,
    pub duration_us: u32,
    pub independent: bool,
    pub payload: Vec<u8>,
}

struct ConnectionGuard(quinn::Connection);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"PERI session closed");
    }
}

struct PendingSend {
    stream: quinn::SendStream,
    complete: bool,
}
impl Drop for PendingSend {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.stream.reset(1u32.into());
        }
    }
}

/// Hold one negotiated resource and its control stream for the connection lifetime.
/// 每连接协商一个资源，保留控制流；释放时关闭连接，调用方必须独占连接所有权。
pub struct Session {
    guard: ConnectionGuard,
    agreement: Agreement,
    publisher: bool,
    send_slots: tokio::sync::Semaphore,
    _control: (quinn::SendStream, quinn::RecvStream),
}
impl Session {
    /// Return the agreement only while the transport remains connected.
    /// 仅在传输尚未观察到关闭时返回协商结果。
    pub fn agreement(&self) -> Option<&Agreement> {
        self.guard
            .0
            .close_reason()
            .is_none()
            .then_some(&self.agreement)
    }

    /// Send a single object on its own unidirectional stream within a time budget.
    /// 在独立单向流发送一个对象；超时中止该流，调用方决定后续依赖恢复。
    pub async fn send(&self, object: &Object, budget: Duration) -> Result<(), Error> {
        if !self.publisher {
            return Err(Error::Protocol("subscriber cannot publish"));
        }
        let deadline = tokio::time::Instant::now() + budget;
        let _slot = tokio::time::timeout_at(deadline, self.send_slots.acquire())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(transport)?;
        let bytes = wire::encode_object(object)?;
        let stream = tokio::time::timeout_at(deadline, self.guard.0.open_uni())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(transport)?;
        let mut pending = PendingSend {
            stream,
            complete: false,
        };
        tokio::time::timeout_at(deadline, async {
            AsyncWriteExt::write_all(&mut pending.stream, &bytes).await?;
            pending.stream.finish().map_err(transport)?;
            if pending.stream.stopped().await.map_err(transport)?.is_some() {
                return Err(Error::Protocol("object rejected by peer"));
            }
            pending.complete = true;
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    /// Receive one complete object with bounded allocation and a caller time budget.
    /// 按调用方预算收取完整对象，长度在分配前校验；应用负责序号与依赖处理。
    pub async fn recv(&mut self, budget: Duration) -> Result<Object, Error> {
        if self.publisher {
            return Err(Error::Protocol("publisher cannot subscribe"));
        }
        tokio::time::timeout(budget, async {
            let mut stream = self.guard.0.accept_uni().await.map_err(transport)?;
            wire::read_object(&mut stream).await
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
}

fn validate_local(
    hello: &Hello,
    resource: &str,
    connection: &quinn::Connection,
) -> Result<(), Error> {
    let handshake = connection
        .handshake_data()
        .ok_or(Error::Protocol("missing TLS handshake"))?;
    let tls = handshake
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .map_err(|_| Error::Protocol("unsupported TLS backend"))?;
    if tls.protocol.as_deref() != Some(ALPN) {
        return Err(Error::Protocol("ALPN mismatch"));
    }
    if resource.is_empty()
        || resource.len() > 256
        || !resource
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        return Err(Error::Protocol("invalid resource"));
    }
    peri_session::validate(hello).map_err(transport)?;
    // 本阶段没有扩展执行能力，尤其不得宣称底层已启用多路径。
    if !hello.extensions.is_empty() {
        return Err(Error::Protocol("local extensions not implemented"));
    }
    Ok(())
}

/// Subscribe to one resource on an exclusively owned, authenticated QUIC connection.
/// 在独占的已认证 QUIC 连接上订阅一个资源；身份与访问授权由调用方负责。
pub async fn subscribe(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    budget: Duration,
) -> Result<Session, Error> {
    let guard = ConnectionGuard(connection);
    validate_local(&hello, resource, &guard.0)?;
    tokio::time::timeout(budget, async {
        let (mut send, mut recv) = guard.0.open_bi().await.map_err(transport)?;
        AsyncWriteExt::write_all(&mut send, wire::PREFACE).await?;
        wire::write_control(
            &mut send,
            &Control::Subscribe {
                resource: resource.into(),
                hello: hello.clone(),
            },
        )
        .await?;
        let Control::Accept {
            hello: remote,
            agreement,
        } = wire::read_control(&mut recv).await?
        else {
            return Err(Error::Protocol("expected acceptance"));
        };
        let expected = peri_session::negotiate(&remote, &hello).map_err(transport)?;
        if agreement != expected {
            return Err(Error::Protocol("agreement mismatch"));
        }
        wire::write_control(&mut send, &Control::Ack).await?;
        Ok(Session {
            guard,
            agreement,
            publisher: false,
            send_slots: tokio::sync::Semaphore::new(8),
            _control: (send, recv),
        })
    })
    .await
    .map_err(|_| Error::Timeout)?
}

/// Serve one explicitly authorized resource; activation requires the peer's ACK.
/// 提供调用方明确授权的单一资源；收到对端 ACK 后才允许发布对象。
pub async fn accept(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    budget: Duration,
) -> Result<Session, Error> {
    let guard = ConnectionGuard(connection);
    validate_local(&hello, resource, &guard.0)?;
    tokio::time::timeout(budget, async {
        let (mut send, mut recv) = guard.0.accept_bi().await.map_err(transport)?;
        let mut preface = [0; 8];
        recv.read_exact(&mut preface).await.map_err(transport)?;
        if &preface != wire::PREFACE {
            return Err(Error::Protocol("invalid preface or wire version"));
        }
        let Control::Subscribe {
            resource: requested,
            hello: remote,
        } = wire::read_control(&mut recv).await?
        else {
            return Err(Error::Protocol("expected subscription"));
        };
        if requested != resource {
            return Err(Error::Protocol("resource denied"));
        }
        let agreement = peri_session::negotiate(&hello, &remote).map_err(transport)?;
        wire::write_control(
            &mut send,
            &Control::Accept {
                hello,
                agreement: agreement.clone(),
            },
        )
        .await?;
        if !matches!(wire::read_control(&mut recv).await?, Control::Ack) {
            return Err(Error::Protocol("expected ACK"));
        }
        Ok(Session {
            guard,
            agreement,
            publisher: true,
            send_slots: tokio::sync::Semaphore::new(8),
            _control: (send, recv),
        })
    })
    .await
    .map_err(|_| Error::Timeout)?
}
