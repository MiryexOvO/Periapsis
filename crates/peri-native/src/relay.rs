//! Bounded single-resource fan-out without transcoding.
//! 单资源有界分发，不转码；积压订阅者断开，发布端不等待慢订阅者。

use crate::{
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
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            viewers: 8,
            queued_frames: 8,
            frame_bytes: 1024 * 1024 - 68,
            residence: Duration::from_millis(300),
        }
    }
}

#[derive(Clone)]
struct Item {
    frame: Arc<Frame>,
    received: Instant,
}
struct Viewer {
    queue: mpsc::Sender<Item>,
    cancel: oneshot::Sender<()>,
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
    requests: Mutex<Vec<u16>>,
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
            .and_then(|frames| frames.checked_mul(limits.viewers))
            .and_then(|frames| frames.checked_mul(limits.frame_bytes));
        if cache_bound.is_none_or(|bytes| bytes > 128 * 1024 * 1024)
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
    pub async fn ingest(&self, mut source: Subscriber, idle_budget: Duration) -> Result<(), Error> {
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
            let requests = std::mem::take(&mut *self.requests.lock().expect("requests"));
            for track in requests {
                source.request_keyframe(track).await?;
            }
            self.push(source.recv(idle_budget).await?, Instant::now())?;
        }
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
        let item = Item {
            frame: Arc::new(frame),
            received: now,
        };
        let stale: Vec<_> = state
            .viewers
            .iter()
            .filter_map(|(id, viewer)| viewer.queue.try_send(item.clone()).err().map(|_| *id))
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
            state.viewers.insert(id, Viewer { queue, cancel });
            id
        };
        let _registration = Registration { relay: self, id };
        let operation = async {
            let publisher = Publisher::new(session, self.catalog.clone(), setup_budget).await?;
            let mut forwarded = VecDeque::<(u64, u16)>::new();
            let mut sends = tokio::task::JoinSet::new();
            let mut recovery_points = HashMap::<u16, u64>::new();
            loop {
                if let Some(track) = publisher.poll_keyframe_request()? {
                    let mut requests = self.requests.lock().expect("requests");
                    if !requests.contains(&track) && requests.len() < 8 {
                        requests.push(track);
                    }
                }
                let item = tokio::select! {
                    biased;
                    completed = sends.join_next(), if !sends.is_empty() => {
                        let (track, sequence, result) = completed.expect("pending send").map_err(crate::transport)?;
                        match result {
                            Ok(()) => {},
                            Err(Error::Timeout | Error::ObjectAborted) => {
                                // A late failure from an older GOP must not invalidate a newer keyframe.
                                if recovery_points.get(&track).is_none_or(|key| *key <= sequence) {
                                    forwarded.retain(|(_, id)| *id != track);
                                    self.request_recovery(track);
                                }
                            },
                            Err(error) => return Err(error),
                        }
                        continue;
                    }
                    item = receiver.recv() => item,
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
                    if frame.kind != FrameKind::Disposable {
                        forwarded.retain(|(_, track)| *track != frame.track);
                        self.request_recovery(frame.track);
                    }
                    continue;
                }
                match publisher.start_send(frame, deadline - now).await {
                    Ok(send) => {
                        let track = frame.track;
                        let sequence = frame.sequence;
                        sends.spawn(async move { (track, sequence, send.finish().await) });
                        if matches!(frame.kind, FrameKind::Key | FrameKind::Audio) {
                            recovery_points.insert(track, sequence);
                        }
                    }
                    Err(Error::Timeout | Error::ObjectAborted) => {
                        forwarded.retain(|(_, id)| *id != frame.track);
                        self.request_recovery(frame.track);
                        continue;
                    }
                    Err(error) => return Err(error),
                }
                if frame.kind != FrameKind::Disposable {
                    forwarded.push_back((frame.sequence, frame.track));
                    if forwarded.len() > 128 {
                        forwarded.pop_front();
                    }
                }
            }
        };
        tokio::select! {
            result = operation => result,
            _ = &mut cancelled => Err(Error::Protocol("relay subscriber cancelled")),
        }
    }

    fn request_recovery(&self, track: u16) {
        if self
            .catalog
            .tracks
            .iter()
            .any(|entry| entry.id == track && entry.kind == crate::media::TrackKind::Video)
        {
            let mut requests = self.requests.lock().expect("requests");
            if !requests.contains(&track) && requests.len() < 8 {
                requests.push(track);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{Track, TrackKind};
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
                    cancel: slow_cancel,
                },
            );
            state.viewers.insert(
                1,
                Viewer {
                    queue: fast,
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
    fn aggregate_cache_limit_rejects_oversized_configuration() {
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
