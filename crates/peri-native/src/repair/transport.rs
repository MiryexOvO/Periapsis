//! 协商后媒体收发适配器；原始Session对象API保持可用。
//! Negotiated media adapters; raw Session object APIs remain available.
use super::{Admission, Assembler, Codec, Fragment, Limits, RepairBudget};
use crate::{Error, Object, Session, deadline::Deadline, receive, transport};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
    time::{Duration, Instant},
};

// 每个发布者只开放一个修复组；组内最多四个分片future，无后台任务。
// Admit one repair group per publisher, with four fragment futures and no spawned tasks.
// 组间确认串行化保留有序媒体约定；代价是ACK等待，尚需统一调度器替代。
// Serial group ACKs preserve ordered media; the ACK cost needs a unified scheduler later.
pub(crate) struct GroupSend {
    future: Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>,
}
impl GroupSend {
    pub(crate) async fn finish(self) -> Result<(), Error> {
        self.future.await
    }
}
pub(crate) struct Sender {
    codec: Codec,
    budget: Mutex<RepairBudget>,
    slots: Arc<tokio::sync::Semaphore>,
    sequence: Mutex<u64>,
}
impl Sender {
    pub(crate) fn new(session: &Session) -> Result<Self, Error> {
        Ok(Self {
            codec: Codec::new(
                session
                    .agreement()
                    .ok_or(Error::Protocol("closed session"))?,
            )?,
            budget: Mutex::new(RepairBudget::new(32768, 16384, Instant::now())?),
            slots: Arc::new(tokio::sync::Semaphore::new(1)),
            sequence: Mutex::new(1),
        })
    }
    pub(crate) async fn start(
        &self,
        session: Arc<Session>,
        frame: &crate::media::Frame,
        catalog: &crate::media::Catalog,
        deadline: Deadline,
    ) -> Result<GroupSend, Error> {
        let permit = deadline
            .run(self.slots.clone().acquire_owned())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(transport)?;
        // 先取得槽再复制媒体负载，等待者不额外持有适配器对象缓存。
        // Acquire capacity before copying payload; waiters retain no adapter-owned packet.
        let object = frame.to_object(catalog)?;
        // 不适用尺寸或预算不足时显式回退，不隐藏编码或协议错误。
        // Fall back for unsupported sizes or exhausted redundancy credit only.
        let packet_len = object.payload.len() + 25;
        let protected = if object.sequence > 0
            && object.duration_us > 0
            && (1025..=32768).contains(&packet_len)
        {
            self.codec.protect(
                &object,
                1024,
                &mut self.budget.lock().expect("repair budget"),
                Instant::now(),
            )?
        } else {
            None
        };
        let objects = match protected {
            Some(parts) => {
                crate::trace(
                    "repair_send",
                    serde_json::json!({"sequence":object.sequence,"fragments":parts.len(),"overhead":parts.last().unwrap().protection_overhead()}),
                );
                // 外层分片序号单独递增，不使用原帧序号作为分片身份。
                // Outer fragment numbering advances independently of media sequence numbers.
                let base = {
                    let mut next = self.sequence.lock().expect("repair sequence");
                    let base = *next;
                    *next = next
                        .checked_add(parts.len() as u64)
                        .ok_or(Error::Protocol("repair sequence exhausted"))?;
                    base
                };
                parts
                    .into_iter()
                    .enumerate()
                    .map(|(index, part)| part.to_object(base + index as u64))
                    .collect::<Result<Vec<_>, _>>()?
            }
            None => {
                crate::trace(
                    "repair_fallback",
                    serde_json::json!({"sequence":object.sequence,"bytes":object.payload.len()}),
                );
                vec![object]
            }
        };
        Ok(GroupSend {
            future: Box::pin(async move {
                let _permit = permit;
                let mut objects = objects.into_iter();
                type Sending = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;
                let mut active: Vec<Sending> = Vec::new();
                loop {
                    while active.len() < 4 {
                        let Some(object) = objects.next() else { break };
                        let session = session.clone();
                        active.push(Box::pin(async move {
                            session
                                .start_send_until(&object, deadline)
                                .await?
                                .finish()
                                .await
                        }));
                    }
                    if active.is_empty() {
                        return Ok(());
                    }
                    deadline
                        .run(std::future::poll_fn(|cx| {
                            for i in 0..active.len() {
                                if let Poll::Ready(result) = active[i].as_mut().poll(cx) {
                                    drop(active.swap_remove(i));
                                    return Poll::Ready(result);
                                }
                            }
                            Poll::Pending
                        }))
                        .await
                        .map_err(|_| Error::Timeout)??;
                }
            }),
        })
    }
}

struct Reading {
    future: receive::Read,
    deadline: Instant,
}
struct State {
    assembler: Assembler,
    reads: Vec<Reading>,
}
// 成功交付保留其余四槽；错误、超时和future取消同步STOP(1)并终结重组组。
// Retain other slots on delivery; errors, timeout and cancellation STOP(1) and retire groups.
struct Guard<'a> {
    state: &'a mut State,
    keep: bool,
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if !self.keep {
            self.state.reads.clear();
            self.state.assembler.cancel();
        }
    }
}
pub(crate) struct Receiver {
    state: tokio::sync::Mutex<State>,
    age: Duration,
}
impl Receiver {
    pub(crate) fn new(session: &Session) -> Result<Self, Error> {
        let agreement = session
            .agreement()
            .ok_or(Error::Protocol("closed session"))?;
        let age = Duration::from_millis(150)
            .min(Duration::from_millis(agreement.buffer.maximum_ms.into()));
        Ok(Self {
            state: tokio::sync::Mutex::new(State {
                assembler: Codec::new(agreement)?.assembler(Limits {
                    max_age: age,
                    ..Limits::default()
                })?,
                reads: Vec::new(),
            }),
            age,
        })
    }
    pub(crate) fn clear(&mut self) {
        let state = self.state.get_mut();
        state.reads.clear();
        state.assembler.cancel();
    }
    pub(crate) async fn recv(
        &self,
        session: &Session,
        total: Duration,
        residence: Duration,
        independent_only: bool,
    ) -> Result<Option<Object>, Error> {
        if residence.is_zero() || residence > Duration::from_secs(1) {
            return Err(Error::Protocol("object residence"));
        }
        let end = tokio::time::Instant::now() + total;
        let mut state = tokio::time::timeout_at(end, self.state.lock())
            .await
            .map_err(|_| Error::Timeout)?;
        let mut guard = Guard {
            state: &mut state,
            keep: false,
        };
        loop {
            if tokio::time::Instant::now() >= end {
                return Err(Error::Timeout);
            }
            if guard.state.assembler.expire(Instant::now()) > 0 {
                crate::trace("repair_expired", serde_json::json!({}));
                guard.keep = true;
                return Ok(None);
            }
            let expiry = guard
                .state
                .assembler
                .deadline()
                .map(tokio::time::Instant::from_std)
                .unwrap_or(end)
                .min(end);
            tokio::select! {
                biased;
                result = std::future::poll_fn(|cx| {
                    for index in 0..guard.state.reads.len() {
                        if let Poll::Ready(result) = guard.state.reads[index].future.as_mut().poll(cx) {
                            let read = guard.state.reads.swap_remove(index);
                            return Poll::Ready((result, read.deadline));
                        }
                    }
                    Poll::Pending
                }), if !guard.state.reads.is_empty() => {
                    let (result, deadline) = result;
                    let Some(object) = result? else {
                        guard.keep = true;
                        return Ok(None);
                    };
                    let object = if object.sequence != 0 && object.duration_us == 0 {
                        let fragment = Fragment::from_object(object)?;
                        match guard.state.assembler.push(fragment, Instant::now(), deadline)? {
                            Admission::Complete { object, repaired } => {
                                crate::trace("repair_complete", serde_json::json!({"sequence":object.sequence,"repaired":repaired}));
                                object
                            },
                            Admission::AtCapacity | Admission::Expired => {
                                guard.keep = true;
                                return Ok(None);
                            },
                            _ => continue,
                        }
                    } else { object };
                    guard.keep = true;
                    return Ok((!independent_only || object.independent).then_some(object));
                },
                stream = session.guard.0.accept_uni(), if guard.state.reads.len() < 4 => {
                    let stream = stream.map_err(transport)?;
                    // 从接受流时起算驻留；后续分片不刷新组的原始期限。
                    // Start residence at stream acceptance; later fragments never refresh group expiry.
                    let accepted = Instant::now();
                    guard.state.reads.push(Reading {
                        future: receive::read_until(stream, Deadline::at(accepted + residence), false),
                        deadline: accepted + residence.min(self.age),
                    });
                },
                _ = tokio::time::sleep_until(expiry) => {},
            }
        }
    }
}
