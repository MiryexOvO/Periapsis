//! 恢复阶段的有界并发对象读取；不创建后台任务，不负责媒体依赖调度。
use crate::{Error, Object, transport, wire};
use std::{future::Future, pin::Pin, task::Poll, time::Duration};

// 关键帧胜出、错误或调用future取消时，都使用约定的局部放弃码，不能默认STOP(0)。
struct PendingRead {
    stream: quinn::RecvStream,
    progress: wire::ReadProgress,
    reason: &'static str,
}
impl Drop for PendingRead {
    fn drop(&mut self) {
        // 正常完成不重复输出；其余原因均为本地观察，不能单凭RESET推断远端触发原因。
        if self.reason != "complete" {
            crate::trace(
                "object_read_end",
                serde_json::json!({"stream":self.stream.id().index(),"sequence":self.progress.sequence,"phase":self.progress.phase,"reason":self.reason}),
            );
        }
        let _ = self.stream.stop(1u32.into());
    }
}
type Read = Pin<Box<dyn Future<Output = Result<Option<Object>, Error>> + Send + Sync>>;
pub(crate) async fn serial(
    stream: quinn::RecvStream,
    residence: Duration,
    independent_only: bool,
) -> Result<Option<Object>, Error> {
    read(stream, residence, independent_only).await
}
pub(crate) async fn strict(stream: quinn::RecvStream) -> Result<Object, Error> {
    let mut pending = PendingRead {
        stream,
        progress: wire::ReadProgress::default(),
        reason: "cancelled",
    };
    let result =
        wire::read_object_observed(&mut pending.stream, false, &mut pending.progress).await;
    pending.reason = if result.is_ok() {
        "complete"
    } else {
        "read_error"
    };
    result?.ok_or(Error::Protocol("object selection"))
}
fn read(stream: quinn::RecvStream, residence: Duration, independent_only: bool) -> Read {
    let mut pending = PendingRead {
        stream,
        progress: wire::ReadProgress::default(),
        reason: "cancelled",
    };
    let deadline = tokio::time::Instant::now() + residence;
    Box::pin(async move {
        let result = match tokio::time::timeout_at(
            deadline,
            wire::read_object_observed(
                &mut pending.stream,
                independent_only,
                &mut pending.progress,
            ),
        )
        .await
        {
            Err(_) => {
                pending.reason = "residence_timeout";
                Ok(None)
            }
            Ok(Err(Error::Io(error))) if matches!(error.get_ref().and_then(|e|e.downcast_ref::<quinn::ReadError>()),Some(quinn::ReadError::Reset(code)) if *code==1u32.into()) =>
            {
                pending.reason = "peer_reset_1";
                Ok(None)
            }
            Ok(result) => {
                pending.reason = match &result {
                    Ok(Some(_)) => "complete",
                    Ok(None) => "selected_out",
                    Err(_) => "read_error",
                };
                result
            }
        };
        // 显式保留整个保护器到future结束，确保取消与终结原因都由同一个Drop处理。
        drop(pending);
        result
    })
}

// 每个流最多分配1MiB负载，共4个槽；尚未接受的流仍由调用方QUIC接收窗口约束。
// 返回时直接释放future即可同步STOP所有未完成流，无异步任务残留或隐藏完成队列。
pub(crate) async fn recovery(
    connection: &quinn::Connection,
    idle: Duration,
    residence: Duration,
) -> Result<Option<Object>, Error> {
    let mut reads: Vec<Read> = Vec::new();
    let deadline = tokio::time::Instant::now() + idle;
    loop {
        // 已就绪流持续到达时也必须遵守总预算，不能被biased分支持续饿死计时器。
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Timeout);
        }
        tokio::select! {
            biased;
            result = std::future::poll_fn(|cx| {
                for index in 0..reads.len() {
                    if let Poll::Ready(result) = reads[index].as_mut().poll(cx) {
                        drop(reads.swap_remove(index));
                        return Poll::Ready(result);
                    }
                }
                Poll::Pending
            }), if !reads.is_empty() => {
                match result? {
                    Some(object) => return Ok(Some(object)),
                    None if reads.is_empty() => return Ok(None),
                    None => {},
                }
            },
            stream = connection.accept_uni(), if reads.len()<4 => {
                reads.push(read(stream.map_err(transport)?,residence,true));
            },
            // 总调用预算不因跳过对象而延长；每个流另有接受时起算的驻留上限。
            _ = tokio::time::sleep_until(deadline) => return Err(Error::Timeout),
        }
    }
}

// 槽包含读取中或完整待交付对象，两者共用四槽额度；完整结果不另设无界缓存。
struct Slot {
    stream: u64,
    read: Option<Read>,
    result: Option<Result<Option<Object>, Error>>,
    deadline: tokio::time::Instant,
}
#[derive(Default)]
pub(crate) struct Retained {
    slots: Vec<Slot>,
    last: Option<u64>,
}
impl Retained {
    pub(crate) fn active(&self) -> bool {
        !self.slots.is_empty()
    }
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
    }
}
// 成功交付才跨调用保留；超时、格式错误及调用future取消时统一释放所有流。
struct RetainGuard<'a> {
    state: &'a mut Retained,
    keep: bool,
}
impl Drop for RetainGuard<'_> {
    fn drop(&mut self) {
        if !self.keep {
            self.state.clear();
        }
    }
}
pub(crate) async fn retained(
    connection: &quinn::Connection,
    state: &mut Retained,
    total: Duration,
    residence: Duration,
    recovering: bool,
) -> Result<Option<Object>, Error> {
    let mut guard = RetainGuard { state, keep: false };
    let deadline = tokio::time::Instant::now() + total;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(Error::Timeout);
        }
        // 已完成但未交付的帧也遵守原始驻留期限；解码等待不能刷新它们的寿命。
        for slot in &mut guard.state.slots {
            if now >= slot.deadline && !matches!(slot.result, Some(Err(_) | Ok(None))) {
                crate::trace(
                    "retained_slot_expired",
                    serde_json::json!({"stream":slot.stream,"sequence":slot.result.as_ref().and_then(|r|r.as_ref().ok()).and_then(|o|o.as_ref()).map(|o|o.sequence),"completed":slot.read.is_none()}),
                );
                slot.read = None;
                slot.result = Some(Ok(None));
            }
        }
        // 已观察到的格式错误优先报告，不能因另一个关键帧胜出而隐藏。
        if let Some(index) = guard
            .state
            .slots
            .iter()
            .position(|s| matches!(s.result, Some(Err(_))))
        {
            return Err(guard.state.slots[index].result.take().unwrap().unwrap_err());
        }
        let selected = if recovering {
            guard
                .state
                .slots
                .iter()
                .position(|s| matches!(&s.result,Some(Ok(Some(o))) if o.independent))
        } else {
            guard
                .state
                .slots
                .first()
                .filter(|s| s.result.is_some())
                .map(|_| 0)
        };
        if let Some(index) = selected {
            let object = guard.state.slots[index].result.take().unwrap()?;
            // 仅丢弃胜出关键帧及之前的流，保留之后的完整对象与半对象。
            guard.state.slots.drain(..=index);
            if let Some(object) = &object {
                if guard.state.last.is_some_and(|last| object.sequence <= last) {
                    return Err(Error::Protocol("retained receive sequence"));
                }
                guard.state.last = Some(object.sequence);
            }
            guard.keep = true;
            return Ok(object);
        }
        if recovering
            && guard
                .state
                .slots
                .first()
                .is_some_and(|s| s.result.is_some())
        {
            // 只能删除最早的已完成依赖/缺口，不能越过一个仍可能成为关键帧的旧流。
            guard.state.slots.remove(0);
            if guard.state.slots.is_empty() {
                guard.keep = true;
                return Ok(None);
            }
            continue;
        }
        let have_reads = guard.state.slots.iter().any(|s| s.read.is_some());
        tokio::select! {
            biased;
            _ = std::future::poll_fn(|cx| {
                let mut changed=false;
                for slot in &mut guard.state.slots {
                    if let Some(read)=&mut slot.read
                        && let Poll::Ready(result)=read.as_mut().poll(cx) {
                        slot.read=None; slot.result=Some(result); changed=true;
                    }
                }
                if changed { Poll::Ready(()) } else { Poll::Pending }
            }), if have_reads => {},
            stream=connection.accept_uni(), if guard.state.slots.len()<4 => {
                let stream=stream.map_err(transport)?;
                let stream_id=stream.id().index();
                guard.state.slots.push(Slot {stream:stream_id,read:Some(read(stream,residence,false)),result:None,deadline:tokio::time::Instant::now()+residence});
            },
            _=tokio::time::sleep_until(deadline) => return Err(Error::Timeout),
        }
    }
}
