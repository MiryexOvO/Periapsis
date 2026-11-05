//! 对象写入与传输确认；释放未完成句柄会RESET(1)。
//! Object writes and transport receipts; dropping unfinished work issues RESET(1).
use crate::{Error, Object, deadline::Deadline, trace, transport, wire};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

// 状态同时决定取消诊断和清理行为，避免完成标记与原因字符串矛盾。
// State controls both cancellation diagnostics and cleanup.
#[derive(Clone, Copy)]
enum State {
    Writing,
    AwaitingAck,
    Complete,
    WriteFailed,
    WriteExpired,
    AckFailed,
    AckExpired,
    PeerStopped,
}
impl State {
    fn reason(self) -> &'static str {
        match self {
            Self::Writing => "write_cancelled",
            Self::AwaitingAck => "ack_wait_cancelled",
            Self::Complete => "complete",
            Self::WriteFailed => "write_error",
            Self::WriteExpired => "write_deadline",
            Self::AckFailed => "ack_error",
            Self::AckExpired => "ack_deadline",
            Self::PeerStopped => "peer_stop",
        }
    }
}
struct PendingSend {
    stream: quinn::SendStream,
    sequence: u64,
    state: State,
}
impl Drop for PendingSend {
    fn drop(&mut self) {
        if !matches!(self.state, State::Complete) {
            trace(
                "object_send_cancel",
                serde_json::json!({"stream":self.stream.id().index(),"sequence":self.sequence,"reason":self.state.reason()}),
            );
            let _ = self.stream.reset(1u32.into());
        }
    }
}

/// A queued object whose transport acknowledgement is still pending.
/// Dropping it cancels only this object stream; this is never a decode receipt.
/// 句柄持有发送槽；丢弃时取消本流，传输确认不等于解码确认。
pub struct ObjectSend {
    pending: PendingSend,
    _slot: tokio::sync::OwnedSemaphorePermit,
    deadline: Deadline,
}
impl ObjectSend {
    /// Await transport acknowledgement within the original budget and release the slot.
    /// 在原始期限内等待传输确认，然后释放发送槽。
    pub async fn finish(mut self) -> Result<(), Error> {
        let outcome = self
            .deadline
            .run(async {
                if let Some(code) = self.pending.stream.stopped().await.map_err(|error| {
                    self.pending.state = State::AckFailed;
                    transport(error)
                })? {
                    self.pending.state = State::PeerStopped;
                    return Err(if code == 1u32.into() {
                        Error::ObjectAborted
                    } else {
                        Error::Protocol("object rejected by peer")
                    });
                }
                Ok(())
            })
            .await;
        if outcome.is_err() {
            self.pending.state = State::AckExpired;
        }
        outcome??;
        self.pending.state = State::Complete;
        Ok(())
    }
}

pub(crate) async fn start(
    connection: &quinn::Connection,
    slots: Arc<tokio::sync::Semaphore>,
    object: &Object,
    deadline: Deadline,
) -> Result<ObjectSend, Error> {
    let slot = deadline
        .run(slots.acquire_owned())
        .await?
        .map_err(transport)?;
    let bytes = wire::encode_object(object)?;
    // 打开流后立即安装清理保护器，包括优先级设置失败和跨越截止时间的情况。
    // Guard the stream immediately, including priority errors and expiry after opening.
    let mut pending = deadline
        .run(async {
            let stream = connection.open_uni().await.map_err(transport)?;
            let pending = PendingSend {
                stream,
                sequence: object.sequence,
                state: State::Writing,
            };
            pending
                .stream
                .set_priority(if object.independent { 10 } else { 0 })
                .map_err(transport)?;
            Ok::<_, Error>(pending)
        })
        .await??;
    deadline
        .run(async {
            AsyncWriteExt::write_all(&mut pending.stream, &bytes)
                .await
                .map_err(|error| {
                    let stopped = matches!(
                        error.get_ref().and_then(|cause| cause.downcast_ref::<quinn::WriteError>()),
                        Some(quinn::WriteError::Stopped(code)) if *code == 1u32.into()
                    );
                    if stopped {
                        pending.state = State::PeerStopped;
                        Error::ObjectAborted
                    } else {
                        pending.state = State::WriteFailed;
                        Error::Io(error)
                    }
                })?;
            pending.stream.finish().map_err(|error| {
                pending.state = State::WriteFailed;
                transport(error)
            })?;
            Ok::<_, Error>(())
        })
        .await
        .inspect_err(|_| {
            pending.state = State::WriteExpired;
        })??;
    pending.state = State::AwaitingAck;
    Ok(ObjectSend {
        pending,
        _slot: slot,
        deadline,
    })
}
