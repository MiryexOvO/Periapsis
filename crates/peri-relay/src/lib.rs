// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded single-resource fan-out without transcoding.
//! 单资源有界分发，不转码；积压订阅者断开，发布端不等待慢订阅者。

use peri_native::{
    Error, Session,
    media::{Catalog, Frame, FrameKind, Publisher, Subscriber},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

/// Resource limits for one relay instance.
/// 单个转发资源的配额，排队帧数与负载上限共同限制缓存。
#[derive(Clone, Copy)]
pub struct Limits {
    pub viewers: usize,
    pub queued_frames: usize,
    pub frame_bytes: usize,
    pub residence: Duration,
    /// 每个观看者排队、写入和待确认对象的媒体负载额度。
    /// Media payload allowance across queued, writing and unacknowledged objects per viewer.
    pub buffered_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            viewers: 8,
            queued_frames: 8,
            frame_bytes: 1024 * 1024 - 68,
            residence: Duration::from_millis(300),
            buffered_bytes: 12 * (1024 * 1024 - 68),
        }
    }
}

struct Item {
    frame: Arc<Frame>,
    received: Instant,
    _bytes: tokio::sync::OwnedSemaphorePermit,
}
struct Viewer {
    queue: mpsc::Sender<Item>,
    cancel: oneshot::Sender<()>,
    bytes: Arc<tokio::sync::Semaphore>,
}
struct State {
    viewers: HashMap<u64, Viewer>,
    next_id: u64,
    closed: bool,
    ingest_started: bool,
    sequence: u64,
    references: VecDeque<(u64, u16)>,
}

/// Fan out one immutable media resource to independently bounded viewers.
/// 对一个不可变媒体资源分发，各订阅者有独立容量和驻留时间边界。
pub struct Relay {
    catalog: Catalog,
    limits: Limits,
    state: Mutex<State>,
    requests: Mutex<Vec<peri_native::RecoveryRequest>>,
    report: Mutex<Option<peri_native::ReceiverReport>>,
    changed: tokio::sync::Notify,
}
struct Registration<'a> {
    relay: &'a Relay,
    id: u64,
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.relay
            .state
            .lock()
            .expect("relay lock")
            .viewers
            .remove(&self.id);
    }
}
struct IngestGuard<'a>(&'a Relay);
impl Drop for IngestGuard<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Relay {
    /// Construct bounded fan-out for a catalog already validated by the ingest API.
    /// 为接入 API 已校验的目录创建有界分发；每个实例只接受一次发布生命周期。
    pub fn new(catalog: Catalog, limits: Limits) -> Result<Self, Error> {
        let cache_bound = limits
            .queued_frames
            .checked_add(4)
            .and_then(|frames| frames.checked_mul(limits.frame_bytes))
            .map(|bytes| bytes.min(limits.buffered_bytes))
            .and_then(|bytes| bytes.checked_mul(limits.viewers));
        if cache_bound.is_none_or(|bytes| bytes > 128 * 1024 * 1024)
            || limits.buffered_bytes < limits.frame_bytes
            || limits.buffered_bytes > 128 * 1024 * 1024
            || limits.viewers == 0
            || limits.viewers > 64
            || limits.queued_frames == 0
            || limits.queued_frames > 128
            || limits.frame_bytes == 0
            || limits.frame_bytes > 1024 * 1024 - 68
            || limits.residence.is_zero()
            || limits.residence > Duration::from_secs(1)
        {
            return Err(Error::Protocol("relay limits"));
        }
        Ok(Self {
            catalog,
            limits,
            requests: Mutex::new(Vec::new()),
            report: Mutex::new(None),
            changed: tokio::sync::Notify::new(),
            state: Mutex::new(State {
                viewers: HashMap::new(),
                next_id: 0,
                closed: false,
                ingest_started: false,
                sequence: 0,
                references: VecDeque::new(),
            }),
        })
    }

    /// Ingest an ordered publisher; errors, idle timeout or cancellation close the resource.
    /// 接收有序发布端；错误、空闲超时或取消会关闭资源并通知所有订阅者。
    pub async fn ingest(&self, source: Subscriber, idle_budget: Duration) -> Result<(), Error> {
        if source.catalog() != &self.catalog {
            return Err(Error::Protocol("relay catalog mismatch"));
        }
        {
            let mut state = self.state.lock().expect("relay lock");
            if state.closed || state.ingest_started {
                return Err(Error::Protocol("relay ingest unavailable"));
            }
            state.ingest_started = true;
        }
        let _guard = IngestGuard(self);
        loop {
            // 读取future只创建一次；反馈到达不重启空闲预算，也不取消半个对象。
            let receive = source.recv_ingest_shared(idle_budget);
            tokio::pin!(receive);
            let frame = loop {
                let requests = std::mem::take(&mut *self.requests.lock().expect("requests"));
                for request in requests {
                    peri_native::trace("relay_recovery", request);
                    source.forward_recovery(request).await?;
                }
                let report = self.report.lock().expect("report").take();
                if let Some(report) = report {
                    let sole = {
                        let state = self.state.lock().expect("relay lock");
                        state.viewers.len() == 1 && state.viewers.contains_key(&report.viewer)
                    };
                    if sole {
                        source.send_report(report).await?;
                    }
                }
                tokio::select! {
                    result = &mut receive => break result?,
                    _ = self.changed.notified() => {},
                }
            };
            self.push_ingest(frame, Instant::now())?;
        }
    }

    fn push_ingest(&self, frame: Frame, now: Instant) -> Result<(), Error> {
        frame.validate(&self.catalog)?;
        if frame.payload.len() > self.limits.frame_bytes {
            return Err(Error::Protocol("relay frame limit"));
        }
        let mut state = self.state.lock().expect("relay lock");
        if state.closed || frame.sequence <= state.sequence {
            return Err(Error::Protocol("relay sequence or state"));
        }
        if frame
            .dependencies
            .iter()
            .any(|id| !state.references.contains(&(*id, frame.track)))
        {
            // Descendants may already be in flight when a publisher cancels a reference.
            // 发布端取消参考帧时，其后继可能已经在途；丢弃依赖缺口而不关闭资源。
            state.sequence = frame.sequence;
            drop(state);
            peri_native::trace(
                "relay_ingest_gap",
                serde_json::json!({"track":frame.track,"sequence":frame.sequence}),
            );
            self.request_recovery(frame.track, 0, frame.sequence);
            return Ok(());
        }
        drop(state);
        if frame.kind == FrameKind::Key {
            peri_native::trace(
                "relay_key_received",
                serde_json::json!({"sequence":frame.sequence,"pts_us":frame.timestamp_us,"bytes":frame.payload.len()}),
            );
        }
        self.push(frame, now)
    }

    fn push(&self, frame: Frame, now: Instant) -> Result<(), Error> {
        frame.validate(&self.catalog)?;
        if frame.payload.len() > self.limits.frame_bytes {
            return Err(Error::Protocol("relay frame limit"));
        }
        let mut state = self.state.lock().expect("relay lock");
        if state.closed {
            return Err(Error::Protocol("relay closed"));
        }
        if frame.sequence <= state.sequence
            || frame
                .dependencies
                .iter()
                .any(|id| !state.references.contains(&(*id, frame.track)))
        {
            return Err(Error::Protocol("relay sequence or dependency"));
        }
        state.sequence = frame.sequence;
        if frame.kind != FrameKind::Disposable {
            state.references.push_back((frame.sequence, frame.track));
            if state.references.len() > 128 {
                state.references.pop_front();
            }
        }
        let frame = Arc::new(frame);
        // 共享负载按观看者分别计费；一人的拥塞不能占用另一人的额度。
        // Charge shared payload separately per viewer so congestion stays isolated.
        let stale: Vec<_> = state
            .viewers
            .iter()
            .filter_map(|(id, viewer)| {
                let reservation = viewer
                    .bytes
                    .clone()
                    .try_acquire_many_owned(frame.payload.len() as u32);
                let failed = match reservation {
                    Ok(bytes) => viewer
                        .queue
                        .try_send(Item {
                            frame: frame.clone(),
                            received: now,
                            _bytes: bytes,
                        })
                        .is_err(),
                    Err(_) => true,
                };
                failed.then_some(*id)
            })
            .collect();
        for id in stale {
            if let Some(viewer) = state.viewers.remove(&id) {
                let _ = viewer.cancel.send(());
            }
        }
        Ok(())
    }

    /// Serve an authorized subscriber, waiting for independent units on late join.
    /// 服务已授权订阅者，晚加入时等待独立单元；过期或缺失依赖后按轨道恢复。
    pub async fn serve(&self, session: Session, setup_budget: Duration) -> Result<(), Error> {
        let agreement = session
            .agreement()
            .ok_or(Error::Protocol("closed relay subscriber"))?;
        let residence = self
            .limits
            .residence
            .min(Duration::from_millis(agreement.buffer.maximum_ms.into()));
        let (queue, mut receiver) = mpsc::channel(self.limits.queued_frames);
        let (cancel, mut cancelled) = oneshot::channel();
        let id = {
            let mut state = self.state.lock().expect("relay lock");
            if state.closed || state.viewers.len() >= self.limits.viewers {
                return Err(Error::Protocol("relay viewer limit or closed"));
            }
            let id = state.next_id;
            state.next_id = id
                .checked_add(1)
                .ok_or(Error::Protocol("relay viewer id exhausted"))?;
            state.viewers.insert(
                id,
                Viewer {
                    queue,
                    cancel,
                    bytes: Arc::new(tokio::sync::Semaphore::new(self.limits.buffered_bytes)),
                },
            );
            id
        };
        let _registration = Registration { relay: self, id };
        let operation = async {
            let publisher = Publisher::new(session, self.catalog.clone(), setup_budget).await?;
            let feedback_changed = publisher.feedback_notification();
            let mut forwarded = VecDeque::<(u64, u16)>::new();
            let mut sends = tokio::task::JoinSet::new();
            // 只保留一个待开流操作；反馈唤醒时继续poll同一个future，不重开流或续期。
            // Keep one pending write; feedback wakeups never recreate its future or deadline.
            type Starting<'a> = std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = (Item, Result<peri_native::media::MediaSend, Error>),
                        > + Send
                        + 'a,
                >,
            >;
            let mut starting: Option<Starting<'_>> = None;
            let mut recovery_points = HashMap::<u16, u64>::new();
            let mut decoded_points = HashMap::<u16, u64>::new();
            loop {
                // 每轮限制反馈工作量，让发送、确认和期限也有机会推进。
                // Bound feedback work per turn so writes, receipts and deadlines keep progressing.
                for _ in 0..8 {
                    let Some(mut request) = publisher.poll_recovery()? else {
                        break;
                    };
                    request.viewer = id;
                    let mut requests = self.requests.lock().expect("requests");
                    enqueue_recovery(&mut requests, request);
                    self.changed.notify_one();
                }
                if let Some(mut report) = publisher.poll_report()? {
                    decoded_points
                        .entry(report.track)
                        .and_modify(|seq| *seq = (*seq).max(report.sequence))
                        .or_insert(report.sequence);
                    report.viewer = id;
                    *self.report.lock().expect("report") = Some(report);
                    self.changed.notify_one();
                }
                let item = tokio::select! {
                    // 控制反馈不等待下一帧或ACK完成；通知不增加队列和后台任务。
                    _ = feedback_changed.notified() => { continue; },
                    completed = sends.join_next(), if !sends.is_empty() => {
                        let (track, sequence, kind, result) = completed.expect("pending send").map_err(|error| Error::Transport(error.to_string()))?;
                        match result {
                            Ok(()) => {},
                            Err(error @ (Error::Timeout | Error::ObjectAborted)) => {
                                peri_native::trace("relay_send_failed", serde_json::json!({"viewer":id,"track":track,"sequence":sequence,"reason":if matches!(error, Error::Timeout) {"ack_deadline"} else {"peer_cancelled"}}));
                                // A late failure from an older GOP must not invalidate a newer keyframe.
                                if failure_requires_recovery(kind, sequence, recovery_points.get(&track).copied(), decoded_points.get(&track).copied()) {
                                    forwarded.retain(|(_, id)| *id != track);
                                    self.request_recovery(track,id,sequence);
                                }
                            },
                            Err(error) => return Err(error),
                        }
                        continue;
                    }
                    completed = async { starting.as_mut().expect("pending start").await }, if starting.is_some() => {
                        starting = None;
                        let (item, result) = completed;
                        let frame = &item.frame;
                        match result {
                            Ok(send) => {
                                // 前驱可能在等待写入期间失败；再次检查依赖，取消已失效的发送。
                                // A predecessor can fail while this write waits; recheck before retaining the receipt.
                                if !frame.dependencies.iter().all(|seq| forwarded.contains(&(*seq, frame.track))) {
                                    drop(send);
                                    forwarded.retain(|(_, track)| *track != frame.track);
                                    self.request_recovery(frame.track, id, frame.sequence);
                                    continue;
                                }
                                let track = frame.track;
                                let sequence = frame.sequence;
                                let kind = frame.kind;
                                if matches!(kind, FrameKind::Key | FrameKind::Audio) {
                                    recovery_points.insert(track, sequence);
                                }
                                if kind != FrameKind::Disposable {
                                    forwarded.push_back((sequence, track));
                                    if forwarded.len() > 128 { forwarded.pop_front(); }
                                }
                                sends.spawn(async move {
                                    let result = send.finish().await;
                                    // 直到确认或取消才归还负载额度，出队不等于释放。
                                    // Return payload credit at receipt completion or cancellation, not on dequeue.
                                    drop(item);
                                    (track, sequence, kind, result)
                                });
                            },
                            Err(Error::Timeout | Error::ObjectAborted) => {
                                peri_native::trace("relay_start_failed", serde_json::json!({"viewer":id,"sequence":frame.sequence}));
                                forwarded.retain(|(_, track)| *track != frame.track);
                                self.request_recovery(frame.track, id, frame.sequence);
                            },
                            Err(error) => return Err(error),
                        }
                        continue;
                    }
                    // 四个发送槽都在等待确认时，先让有界队列保留新帧。
                    // 不因瞬时槽满立即丢掉参考帧；出队后仍按原始接收时间检查截止。
                    item = receiver.recv(), if starting.is_none() && sends.len() < 4 => item,
                };
                let item = item.ok_or(Error::Protocol("relay closed"))?;
                let frame = &item.frame;
                let deadline = item.received + residence;
                let ready = frame
                    .dependencies
                    .iter()
                    .all(|id| forwarded.contains(&(*id, frame.track)));
                let now = Instant::now();
                if deadline <= now || !ready || sends.len() >= 4 {
                    peri_native::trace(
                        "relay_drop",
                        serde_json::json!({"viewer":id,"sequence":frame.sequence,"key":frame.kind == FrameKind::Key,"bytes":frame.payload.len(),"queue_ms":now.saturating_duration_since(item.received).as_millis(),"reason":if deadline <= now {"expired"} else if !ready {"dependency"} else {"inflight_limit"}}),
                    );
                    if frame.kind != FrameKind::Disposable {
                        forwarded.retain(|(_, track)| *track != frame.track);
                        self.request_recovery(frame.track, id, frame.sequence);
                    }
                    continue;
                }
                if frame.kind == FrameKind::Key {
                    peri_native::trace(
                        "relay_key_send",
                        serde_json::json!({"viewer":id,"sequence":frame.sequence,"bytes":frame.payload.len(),"budget_ms":(deadline-now).as_millis()}),
                    );
                }
                let publisher = &publisher;
                starting = Some(Box::pin(async move {
                    let result = publisher.start_send_at(&item.frame, deadline).await;
                    (item, result)
                }));
            }
        };
        tokio::select! {
            result = operation => result,
            _ = &mut cancelled => Err(Error::Protocol("relay subscriber cancelled")),
        }
    }

    fn request_recovery(&self, track: u16, viewer: u64, after_sequence: u64) {
        if self
            .catalog
            .tracks
            .iter()
            .any(|entry| entry.id == track && entry.kind == peri_native::media::TrackKind::Video)
        {
            let mut requests = self.requests.lock().expect("requests");
            enqueue_recovery(
                &mut requests,
                peri_native::RecoveryRequest::after(track, viewer, after_sequence),
            );
            self.changed.notify_one();
        }
    }

    /// Count registered viewers, including catalog setup in progress.
    /// 返回已注册订阅数，包含正在发送目录的订阅者。
    pub fn viewers(&self) -> usize {
        self.state.lock().expect("relay lock").viewers.len()
    }

    /// Close the resource and cancel all downstream sessions.
    /// 关闭资源并取消所有下游会话；重复调用无副作用。
    pub fn close(&self) {
        let mut state = self.state.lock().expect("relay lock");
        state.closed = true;
        for (_, viewer) in state.viewers.drain() {
            let _ = viewer.cancel.send(());
        }
    }
}

// Transport completion can arrive after successful decoding or a newer GOP.
// Disposable objects never invalidate references, even when their ACK times out.
// 迟到的传输失败不能推翻已报告的解码进度或新GOP；可丢弃帧失败不破坏参考链。
fn failure_requires_recovery(
    kind: FrameKind,
    sequence: u64,
    key: Option<u64>,
    decoded: Option<u64>,
) -> bool {
    kind != FrameKind::Disposable
        && key.is_none_or(|key| key <= sequence)
        && decoded.is_none_or(|decoded| decoded < sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_native::media::{Track, TrackKind};
    #[test]
    fn late_ack_failure_cannot_invalidate_disposable_decoded_or_newer_gop() {
        assert!(!failure_requires_recovery(
            FrameKind::Disposable,
            20,
            Some(10),
            None
        ));
        assert!(!failure_requires_recovery(
            FrameKind::Reference,
            20,
            Some(10),
            Some(20)
        ));
        assert!(!failure_requires_recovery(
            FrameKind::Reference,
            20,
            Some(10),
            Some(25)
        ));
        assert!(!failure_requires_recovery(
            FrameKind::Reference,
            20,
            Some(21),
            None
        ));
        assert!(failure_requires_recovery(
            FrameKind::Reference,
            20,
            Some(10),
            Some(19)
        ));
        assert!(failure_requires_recovery(
            FrameKind::Key,
            20,
            Some(20),
            None
        ));
        assert!(failure_requires_recovery(
            FrameKind::Reference,
            20,
            None,
            None
        ));
    }
    fn catalog() -> Catalog {
        Catalog {
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                codec: "video/av1".into(),
                init: vec![],
                width: Some(320),
                height: Some(180),
                sample_rate: None,
                channels: None,
            }],
        }
    }
    fn key(sequence: u64) -> Frame {
        Frame {
            track: 1,
            sequence,
            timestamp_us: 0,
            duration_us: 33333,
            kind: FrameKind::Key,
            dependencies: vec![],
            payload: vec![42],
        }
    }
    #[test]
    fn full_viewer_is_cancelled_without_blocking_another_viewer() {
        let relay = Relay::new(catalog(), Limits::default()).unwrap();
        let (slow, mut slow_rx) = mpsc::channel(1);
        let (slow_cancel, mut cancelled) = oneshot::channel();
        let (fast, mut fast_rx) = mpsc::channel(2);
        let (fast_cancel, _) = oneshot::channel();
        {
            let mut state = relay.state.lock().unwrap();
            state.viewers.insert(
                0,
                Viewer {
                    queue: slow,
                    bytes: Arc::new(tokio::sync::Semaphore::new(1024)),
                    cancel: slow_cancel,
                },
            );
            state.viewers.insert(
                1,
                Viewer {
                    queue: fast,
                    bytes: Arc::new(tokio::sync::Semaphore::new(1024)),
                    cancel: fast_cancel,
                },
            );
        }
        relay.push(key(1), Instant::now()).unwrap();
        relay.push(key(2), Instant::now()).unwrap();
        assert!(cancelled.try_recv().is_ok());
        assert_eq!(relay.viewers(), 1);
        assert_eq!(slow_rx.try_recv().unwrap().frame.sequence, 1);
        assert_eq!(fast_rx.try_recv().unwrap().frame.sequence, 1);
        assert_eq!(fast_rx.try_recv().unwrap().frame.sequence, 2);
    }
    #[test]
    fn relay_rejects_reordered_missing_dependencies_and_oversize_input() {
        let relay = Relay::new(
            catalog(),
            Limits {
                frame_bytes: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        relay.push(key(1), Instant::now()).unwrap();
        assert!(relay.push(key(1), Instant::now()).is_err());
        let mut missing = key(3);
        missing.kind = FrameKind::Reference;
        missing.dependencies = vec![2];
        assert!(relay.push(missing, Instant::now()).is_err());
        let mut large = key(2);
        large.payload.push(0);
        assert!(relay.push(large, Instant::now()).is_err());
        relay.push(key(2), Instant::now()).unwrap();
        relay.close();
        assert!(relay.push(key(3), Instant::now()).is_err());
    }

    #[test]
    fn ingest_discards_cancelled_reference_descendants_until_a_new_key() {
        let relay = Relay::new(catalog(), Limits::default()).unwrap();
        relay.push_ingest(key(1), Instant::now()).unwrap();
        for sequence in [3, 4] {
            let mut frame = key(sequence);
            frame.kind = FrameKind::Reference;
            frame.dependencies = vec![sequence - 1];
            relay.push_ingest(frame, Instant::now()).unwrap();
        }
        assert!(!relay.state.lock().unwrap().closed);
        assert_eq!(relay.state.lock().unwrap().references.len(), 1);
        assert_eq!(relay.requests.lock().unwrap().len(), 1);
        assert!(relay.push_ingest(key(4), Instant::now()).is_err());
        relay.push_ingest(key(5), Instant::now()).unwrap();
        let mut next = key(6);
        next.kind = FrameKind::Reference;
        next.dependencies = vec![5];
        relay.push_ingest(next, Instant::now()).unwrap();
        assert_eq!(relay.state.lock().unwrap().sequence, 6);
        assert_eq!(relay.state.lock().unwrap().references.len(), 3);
    }
    // 字节额度必须跨越出队阶段；释放对象才归还，慢观看者独立断开。
    // Credit survives dequeue; dropping the item returns it, and a slow viewer is isolated.
    #[test]
    fn payload_credit_survives_dequeue_and_isolated_overflow() {
        let relay = Relay::new(catalog(), Limits::default()).unwrap();
        let bytes = Arc::new(tokio::sync::Semaphore::new(1));
        let (queue, mut receiver) = mpsc::channel(8);
        let (cancel, mut cancelled) = oneshot::channel();
        relay.state.lock().unwrap().viewers.insert(
            0,
            Viewer {
                queue,
                cancel,
                bytes: bytes.clone(),
            },
        );
        relay.push(key(1), Instant::now()).unwrap();
        let inflight = receiver.try_recv().unwrap();
        assert_eq!(bytes.available_permits(), 0);
        relay.push(key(2), Instant::now()).unwrap();
        assert!(cancelled.try_recv().is_ok());
        assert_eq!(relay.viewers(), 0);
        assert_eq!(bytes.available_permits(), 0);
        drop(inflight);
        assert_eq!(bytes.available_permits(), 1);
    }

    // 通道满导致入队失败时，临时预留的字节必须归还。
    // Failed queue admission must return its temporary byte reservation.
    #[test]
    fn failed_enqueue_returns_payload_credit() {
        let relay = Relay::new(catalog(), Limits::default()).unwrap();
        let bytes = Arc::new(tokio::sync::Semaphore::new(8));
        let (queue, mut receiver) = mpsc::channel(1);
        let (cancel, _) = oneshot::channel();
        relay.state.lock().unwrap().viewers.insert(
            0,
            Viewer {
                queue,
                cancel,
                bytes: bytes.clone(),
            },
        );
        relay.push(key(1), Instant::now()).unwrap();
        relay.push(key(2), Instant::now()).unwrap();
        assert_eq!(bytes.available_permits(), 7);
        drop(receiver.try_recv().unwrap());
        assert_eq!(bytes.available_permits(), 8);
        assert!(
            Relay::new(
                catalog(),
                Limits {
                    buffered_bytes: 0,
                    ..Limits::default()
                }
            )
            .is_err()
        );
    }
    #[test]
    fn aggregate_cache_limit_rejects_oversized_configuration() {
        // 显式字节额度允许较长的小帧队列，总预留仍受128MiB限制。
        // Explicit byte limits permit longer small-frame queues within the aggregate cap.
        assert!(
            Relay::new(
                catalog(),
                Limits {
                    viewers: 64,
                    queued_frames: 128,
                    buffered_bytes: 2 * 1024 * 1024,
                    ..Limits::default()
                }
            )
            .is_ok()
        );
        assert!(
            Relay::new(
                catalog(),
                Limits {
                    viewers: 64,
                    queued_frames: 128,
                    ..Limits::default()
                }
            )
            .is_err()
        );
        assert!(
            Relay::new(
                catalog(),
                Limits {
                    queued_frames: usize::MAX,
                    ..Limits::default()
                }
            )
            .is_err()
        );
    }
}

// 同一订阅者的需求可合并，不能用一个订阅者的已恢复进度覆盖另一个人的故障。
fn enqueue_recovery(
    requests: &mut Vec<peri_native::RecoveryRequest>,
    request: peri_native::RecoveryRequest,
) {
    if let Some(old) = requests
        .iter_mut()
        .find(|r| r.track == request.track && r.viewer == request.viewer)
    {
        let position = if old.after_sequence == 0 || request.after_sequence == 0 {
            0
        } else {
            old.after_sequence.max(request.after_sequence)
        };
        *old = request;
        old.after_sequence = position;
    } else if requests.len() < 8 {
        requests.push(request);
    }
}

#[cfg(test)]
mod recovery_queue_tests {
    use super::*;
    #[test]
    fn scopes_cannot_coalesce_and_unknown_position_stays_conservative() {
        let mut queue = Vec::new();
        enqueue_recovery(&mut queue, peri_native::RecoveryRequest::after(1, 1, 10));
        enqueue_recovery(&mut queue, peri_native::RecoveryRequest::after(1, 2, 20));
        assert_eq!(queue.len(), 2);
        enqueue_recovery(&mut queue, peri_native::RecoveryRequest::after(1, 1, 15));
        assert_eq!(queue[0].after_sequence, 15);
        enqueue_recovery(&mut queue, peri_native::RecoveryRequest::after(1, 1, 0));
        assert_eq!(queue[0].after_sequence, 0);
        enqueue_recovery(&mut queue, peri_native::RecoveryRequest::after(1, 1, 30));
        assert_eq!(queue[0].after_sequence, 0);
        for viewer in 3..=10 {
            enqueue_recovery(
                &mut queue,
                peri_native::RecoveryRequest::after(1, viewer, 30),
            );
        }
        assert_eq!(queue.len(), 8);
        assert!(!queue.iter().any(|r| r.viewer == 10));
    }
}
