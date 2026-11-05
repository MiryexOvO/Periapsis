//! Experimental PERI native subscriber/publisher over QUIC.
//! 实验原生 PERI 发布订阅：独立 ALPN、控制封装、对象流与角色凭据 API。

//!
//! Third-party publishers provide encoded media through public types. The caller owns
//! capture, encoding and the authenticated QUIC connection; the library owns framing.
//! ```no_run
//! use peri_native::{Error, Session, media::{Catalog, Frame, Publisher}};
//! use std::time::Duration;
//! async fn publish_encoded(session: Session, catalog: Catalog, frames: Vec<Frame>) -> Result<(), Error> {
//!     let publisher = Publisher::new(session, catalog, Duration::from_secs(5)).await?;
//!     for frame in frames {
//!         if let Some(request) = publisher.poll_recovery()? {
//!             // An application should route this request to its own encoder.
//!             let _requested_track = request.track;
//!         }
//!         let _receiver_status = publisher.poll_report()?;
//!         publisher.send(&frame, Duration::from_millis(300)).await?;
//!     }
//!     Ok(())
//! }
//! ```
pub mod adaptation;
pub mod auth;
pub mod av1;
mod feedback;
mod receive;
pub use feedback::{ReceiverReport, RecoveryRequest, trace};
pub mod media;
pub mod playback;
pub mod relay;
mod wire;
use peri_types::{Agreement, Hello};
pub use quinn;
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
use tokio::io::AsyncWriteExt;

pub const ALPN: &[u8] = b"peri-dev-02";
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
    Keyframe {
        track: u16,
        origin: u32,
        id: u64,
        viewer: u64,
        after_sequence: u64,
    },
    #[serde(rename = "peri.receiver_report")]
    ReceiverReport { report: ReceiverReport },
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

impl Object {
    /// 编码有界对象包，复用原生封装校验，供实验适配器使用。
    pub fn encode_packet(&self) -> Result<Vec<u8>, Error> {
        wire::encode_object(self)
    }
    /// 解码完整对象包，拒绝超长、截断和尾随数据；不代表解码器确认。
    pub async fn decode_packet(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_OBJECT_BYTES + 25 {
            return Err(Error::Protocol("object packet limit"));
        }
        wire::read_object(&mut &bytes[..]).await
    }
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
    sequence: u64,
    reason: &'static str,
}
impl Drop for PendingSend {
    fn drop(&mut self) {
        if !self.complete {
            trace(
                "object_send_cancel",
                serde_json::json!({"stream":self.stream.id().index(),"sequence":self.sequence,"reason":self.reason}),
            );
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
        let outcome = tokio::time::timeout_at(self.deadline, async {
            if let Some(code) = self.pending.stream.stopped().await.map_err(|error| {
                self.pending.reason = "ack_error";
                transport(error)
            })? {
                self.pending.reason = "peer_stop";
                return Err(if code == 1u32.into() {
                    Error::ObjectAborted
                } else {
                    Error::Protocol("object rejected by peer")
                });
            }
            self.pending.complete = true;
            Ok(())
        })
        .await;
        if outcome.is_err() {
            self.pending.reason = "ack_deadline";
        }
        outcome.map_err(|_| Error::Timeout)?
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
    retained: receive::Retained,
}
impl Session {
    /// 当前连接的本地QUIC计数快照；不是媒体解码确认，也不代表远端报告的生成时刻。
    pub fn transport_stats(&self) -> quinn::ConnectionStats {
        self.guard.0.stats()
    }
    pub async fn request_keyframe(&self, track: u16) -> Result<(), Error> {
        if self.publisher {
            return Err(Error::Protocol("publisher cannot request keyframe"));
        }
        self.request_keyframe_after(track, 0).await
    }
    /// 声明最后正常输出的位置；0表示未知，不能据此抑制恢复。
    pub async fn request_keyframe_after(
        &self,
        track: u16,
        after_sequence: u64,
    ) -> Result<(), Error> {
        if self.publisher {
            return Err(Error::Protocol("publisher cannot request keyframe"));
        }
        self.feedback
            .request(RecoveryRequest::after(track, 0, after_sequence))
            .await
    }
    pub fn poll_keyframe_request(&self) -> Option<u16> {
        self.feedback.poll().map(|request| request.track)
    }

    pub fn poll_recovery(&self) -> Option<RecoveryRequest> {
        self.feedback.poll()
    }
    pub fn poll_report(&self) -> Option<ReceiverReport> {
        self.feedback.report()
    }
    pub async fn forward_recovery(&self, request: RecoveryRequest) -> Result<(), Error> {
        if self.publisher {
            return Err(Error::Protocol("invalid feedback role"));
        }
        self.feedback.request(request).await
    }
    pub async fn send_report(&self, report: ReceiverReport) -> Result<(), Error> {
        if self.publisher {
            return Err(Error::Protocol("invalid feedback role"));
        }
        self.feedback.send_report(report).await
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
    /// 预算包含排队和确认等待；句柄持有有界发送槽，调用方必须完成或释放。
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
        // Independent objects unblock decoding. Prioritize their unsent bytes
        // over dependent media without changing congestion control or deadlines.
        // 独立对象可恢复解码，优先调度其未发送字节；拥塞控制和截止时间仍有效。
        stream
            .set_priority(if object.independent { 10 } else { 0 })
            .map_err(transport)?;
        let mut pending = PendingSend {
            stream,
            complete: false,
            sequence: object.sequence,
            reason: "write_cancelled",
        };
        tokio::time::timeout_at(deadline, async {
            AsyncWriteExt::write_all(&mut pending.stream, &bytes).await.map_err(|error| {
                if matches!(error.get_ref().and_then(|e| e.downcast_ref::<quinn::WriteError>()), Some(quinn::WriteError::Stopped(code)) if *code == 1u32.into()) {
                    pending.reason="peer_stop";
                    Error::ObjectAborted
                } else { pending.reason="write_error"; Error::Io(error) }
            })?;
            pending.stream.finish().map_err(transport)?;
            Ok::<_, Error>(())
        })
        .await
        .map_err(|_| {pending.reason="write_deadline"; Error::Timeout})??;
        pending.reason = "ack_wait_cancelled";
        Ok(ObjectSend {
            pending,
            _slot: slot,
            deadline,
        })
    }

    /// Live receive: bound an accepted object's residence separately from idle waiting.
    /// None denotes a local expiry or an explicit RESET(1), never malformed framing.
    /// 空闲等待与对象驻留分别限时；None仅表示超时或RESET(1)，格式错误仍返回错误。
    pub async fn recv_live(
        &mut self,
        idle: Duration,
        residence: Duration,
    ) -> Result<Option<Object>, Error> {
        self.recv_live_selected(idle, residence, false).await
    }

    /// Recovery receive: discard dependent objects after a validated header,
    /// without allocating or waiting for their payload. None also means skipped.
    /// Retained independent objects still require complete framing and FIN.
    /// 恢复时先校验头部，依赖对象不分配负载并主动取消；None也包含主动跳过。
    /// 保留的独立对象必须完整读取并收到FIN，不能把半帧交给解码器。
    pub async fn recv_recovery(
        &mut self,
        idle: Duration,
        residence: Duration,
    ) -> Result<Option<Object>, Error> {
        self.recv_live_selected(idle, residence, true).await
    }

    /// 恢复阶段四路并发读取，交付首个完整独立对象并STOP(1)其他已接受流。
    /// 仅适用于所有依赖轨道均在恢复的情况；交付顺序不是流序号顺序。
    /// idle为本次调用总预算，取消会同步停止所有已接受流，负载分配上限4MiB。
    pub async fn recv_recovery_concurrent(
        &mut self,
        idle: Duration,
        residence: Duration,
    ) -> Result<Option<Object>, Error> {
        if self.retained.active() {
            return Err(Error::Protocol("pending retained receive"));
        }
        if self.publisher {
            return Err(Error::Protocol("publisher cannot subscribe"));
        }
        if residence.is_zero() || residence > Duration::from_secs(1) {
            return Err(Error::Protocol("object residence"));
        }
        receive::recovery(&self.guard.0, idle, residence).await
    }

    /// 四槽有序保留候选：恢复时选择完整独立对象，保留其后的流；正常时按开流顺序交付。
    /// 要求单资源发布者按递增对象序号开流。所有依赖轨道须共同恢复，不用于混合活跃轨道。
    /// 完整对象与读取共用四槽/4MiB额度，原始期限不刷新；取消及错误清空在途状态。
    pub async fn recv_retained(
        &mut self,
        total: Duration,
        residence: Duration,
        recovering: bool,
    ) -> Result<Option<Object>, Error> {
        if self.publisher {
            return Err(Error::Protocol("publisher cannot subscribe"));
        }
        if residence.is_zero() || residence > Duration::from_secs(1) {
            return Err(Error::Protocol("object residence"));
        }
        receive::retained(
            &self.guard.0,
            &mut self.retained,
            total,
            residence,
            recovering,
        )
        .await
    }
    /// 显式停止保留的接收流后才能切换其他接收API；保留已交付序号防止倒退。
    pub fn clear_retained_receive(&mut self) {
        self.retained.clear();
    }

    async fn recv_live_selected(
        &mut self,
        idle: Duration,
        residence: Duration,
        independent_only: bool,
    ) -> Result<Option<Object>, Error> {
        if self.retained.active() {
            return Err(Error::Protocol("pending retained receive"));
        }
        if self.publisher {
            return Err(Error::Protocol("publisher cannot subscribe"));
        }
        if residence.is_zero() || residence > Duration::from_secs(1) {
            return Err(Error::Protocol("object residence"));
        }
        let stream = tokio::time::timeout(idle, self.guard.0.accept_uni())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(transport)?;
        receive::serial(stream, residence, independent_only).await
    }

    /// Receive one complete object with bounded allocation and a caller time budget.
    /// 按调用方预算收取完整对象，长度在分配前校验；应用负责序号与依赖处理。
    pub async fn recv(&mut self, budget: Duration) -> Result<Object, Error> {
        self.recv_shared(budget).await
    }

    // 仅供内部单接收循环与反馈转发并行借用，公开API仍用&mut限制接收所有权。
    pub(crate) async fn recv_shared(&self, budget: Duration) -> Result<Object, Error> {
        if self.retained.active() {
            return Err(Error::Protocol("pending retained receive"));
        }
        if self.publisher {
            return Err(Error::Protocol("publisher cannot subscribe"));
        }
        tokio::time::timeout(budget, async {
            let stream = self.guard.0.accept_uni().await.map_err(transport)?;
            receive::strict(stream).await
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
            retained: receive::Retained::default(),
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
            retained: receive::Retained::default(),
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
