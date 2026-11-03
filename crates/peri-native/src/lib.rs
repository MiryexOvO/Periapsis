//! Experimental PERI native subscriber/publisher over QUIC.
//! 实验原生 PERI 发布订阅：独立 ALPN、控制封装和对象流，不提供公网鉴权。

pub mod auth;
pub mod av1;
mod feedback;
pub mod media;
pub mod relay;
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
    /// The peer cancelled this object with application error code 1.
    ObjectAborted,
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
    Subscribe {
        resource: String,
        hello: Hello,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization: Option<String>,
    },
    #[serde(rename = "peri.publish")]
    Publish {
        resource: String,
        hello: Hello,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization: Option<String>,
    },
    #[serde(rename = "peri.accept")]
    Accept { hello: Hello, agreement: Agreement },
    #[serde(rename = "peri.ack")]
    Ack,
    #[serde(rename = "peri.keyframe")]
    Keyframe { track: u16 },
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

/// A queued object whose transport acknowledgement is still pending.
/// Dropping it cancels only this object stream; this is never a decode receipt.
pub struct ObjectSend {
    pending: PendingSend,
    _slot: tokio::sync::OwnedSemaphorePermit,
    deadline: tokio::time::Instant,
}
impl ObjectSend {
    /// Await transport acknowledgement within the original budget and release the slot.
    pub async fn finish(mut self) -> Result<(), Error> {
        tokio::time::timeout_at(self.deadline, async {
            if let Some(code) = self.pending.stream.stopped().await.map_err(transport)? {
                return Err(if code == 1u32.into() {
                    Error::ObjectAborted
                } else {
                    Error::Protocol("object rejected by peer")
                });
            }
            self.pending.complete = true;
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
}

/// Hold one negotiated resource and its control stream for the connection lifetime.
/// 每连接协商一个资源，保留控制流；释放时关闭连接，调用方必须独占连接所有权。
pub struct Session {
    guard: ConnectionGuard,
    agreement: Agreement,
    publisher: bool,
    send_slots: std::sync::Arc<tokio::sync::Semaphore>,
    feedback: feedback::Feedback,
}
impl Session {
    pub async fn request_keyframe(&self, track: u16) -> Result<(), Error> {
        if self.publisher {
            return Err(Error::Protocol("publisher cannot request keyframe"));
        }
        self.feedback.request(track).await
    }
    pub fn poll_keyframe_request(&self) -> Option<u16> {
        self.feedback.poll()
    }

    /// Indicate whether this endpoint may send media on the session.
    /// 表明本端是否具有该会话的媒体发送角色。
    pub fn can_send(&self) -> bool {
        self.publisher
    }

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
        self.start_send(object, budget).await?.finish().await
    }

    /// Queue bytes in stream order and retain a bounded slot until ACK or cancellation.
    /// The budget includes both queuing and acknowledgement; callers must finish or drop.
    pub async fn start_send(&self, object: &Object, budget: Duration) -> Result<ObjectSend, Error> {
        if !self.publisher {
            return Err(Error::Protocol("subscriber cannot publish"));
        }
        let deadline = tokio::time::Instant::now() + budget;
        let slot = tokio::time::timeout_at(deadline, self.send_slots.clone().acquire_owned())
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
            AsyncWriteExt::write_all(&mut pending.stream, &bytes).await.map_err(|error| {
                if matches!(error.get_ref().and_then(|e| e.downcast_ref::<quinn::WriteError>()), Some(quinn::WriteError::Stopped(code)) if *code == 1u32.into()) {
                    Error::ObjectAborted
                } else { Error::Io(error) }
            })?;
            pending.stream.finish().map_err(transport)?;
            Ok::<_, Error>(())
        })
        .await
        .map_err(|_| Error::Timeout)??;
        Ok(ObjectSend {
            pending,
            _slot: slot,
            deadline,
        })
    }

    /// Live receive: bound an accepted object's residence separately from idle waiting.
    /// None denotes a local expiry or an explicit RESET(1), never malformed framing.
    pub async fn recv_live(
        &mut self,
        idle: Duration,
        residence: Duration,
    ) -> Result<Option<Object>, Error> {
        if self.publisher {
            return Err(Error::Protocol("publisher cannot subscribe"));
        }
        if residence.is_zero() || residence > Duration::from_secs(1) {
            return Err(Error::Protocol("object residence"));
        }
        let mut stream = tokio::time::timeout(idle, self.guard.0.accept_uni())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(transport)?;
        match tokio::time::timeout(residence, wire::read_object(&mut stream)).await {
            Err(_) => {
                let _ = stream.stop(1u32.into());
                Ok(None)
            }
            Ok(Err(Error::Io(error))) if matches!(error.get_ref().and_then(|e| e.downcast_ref::<quinn::ReadError>()), Some(quinn::ReadError::Reset(code)) if *code == 1u32.into()) => {
                Ok(None)
            }
            Ok(result) => result.map(Some),
        }
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
    initiate(connection, hello, resource, budget, false, None).await
}

/// Push one resource to an explicitly authorized ingest endpoint.
/// 向调用方明确授权的接入端主动发布一个资源。
pub async fn publish(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    budget: Duration,
) -> Result<Session, Error> {
    initiate(connection, hello, resource, budget, true, None).await
}

async fn initiate(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    budget: Duration,
    publishing: bool,
    authorization: Option<&str>,
) -> Result<Session, Error> {
    let guard = ConnectionGuard(connection);
    validate_local(&hello, resource, &guard.0)?;
    tokio::time::timeout(budget, async {
        let (mut send, mut recv) = guard.0.open_bi().await.map_err(transport)?;
        AsyncWriteExt::write_all(&mut send, wire::PREFACE).await?;
        wire::write_control(
            &mut send,
            &if publishing {
                Control::Publish {
                    resource: resource.into(),
                    hello: hello.clone(),
                    authorization: authorization.map(str::to_owned),
                }
            } else {
                Control::Subscribe {
                    resource: resource.into(),
                    hello: hello.clone(),
                    authorization: authorization.map(str::to_owned),
                }
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
        let feedback = feedback::Feedback::new(send, recv, guard.0.clone(), publishing);
        Ok(Session {
            guard,
            agreement,
            publisher: publishing,
            send_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            feedback,
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
    respond(connection, hello, resource, budget, Some(false), None).await
}

/// Accept a push publisher on an exclusively owned authorized QUIC connection.
/// 在独占且已授权的 QUIC 连接上接收主动发布，不能用于接收订阅请求。
pub async fn accept_publish(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    budget: Duration,
) -> Result<Session, Error> {
    respond(connection, hello, resource, budget, Some(true), None).await
}

async fn respond(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    budget: Duration,
    publishing: Option<bool>,
    credentials: Option<&auth::Credentials>,
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
        let message: Control = wire::read_control(&mut recv).await?;
        let (requested, remote, authorization, requested_publish) = match message {
            Control::Publish {
                resource,
                hello,
                authorization,
            } => (resource, hello, authorization, true),
            Control::Subscribe {
                resource,
                hello,
                authorization,
            } => (resource, hello, authorization, false),
            _ => return Err(Error::Protocol("unexpected connection role")),
        };
        if publishing.is_some_and(|expected| expected != requested_publish) {
            return Err(Error::Protocol("unexpected connection role"));
        }
        if let Some(credentials) = credentials {
            credentials.authorize(requested_publish, authorization.as_deref())?;
        }
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
        let feedback = feedback::Feedback::new(send, recv, guard.0.clone(), !requested_publish);
        Ok(Session {
            guard,
            agreement,
            publisher: !requested_publish,
            send_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            feedback,
        })
    })
    .await
    .map_err(|_| Error::Timeout)?
}

/// Publish with an explicit bearer credential on the protected endpoint.
/// 使用显式持有者凭据向受保护接入端发布。
pub async fn publish_authenticated(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    authorization: &str,
    budget: Duration,
) -> Result<Session, Error> {
    auth::validate_token(authorization)?;
    initiate(
        connection,
        hello,
        resource,
        budget,
        true,
        Some(authorization),
    )
    .await
}

/// Subscribe using a role-scoped bearer credential.
/// 使用限定订阅角色的持有者凭据接收媒体。
pub async fn subscribe_authenticated(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    authorization: &str,
    budget: Duration,
) -> Result<Session, Error> {
    auth::validate_token(authorization)?;
    initiate(
        connection,
        hello,
        resource,
        budget,
        false,
        Some(authorization),
    )
    .await
}

/// Authorize the requested role and resource before activating the session.
/// 在激活会话前授权请求角色与资源；失败不回退到无鉴权接口。
pub async fn accept_authenticated(
    connection: quinn::Connection,
    hello: Hello,
    resource: &str,
    credentials: &auth::Credentials,
    budget: Duration,
) -> Result<Session, Error> {
    respond(connection, hello, resource, budget, None, Some(credentials)).await
}
