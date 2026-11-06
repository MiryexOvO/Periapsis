//! 单视频轨的短时乱序缓冲。调用者给出已处理序号及单调时间。
//! Short reorder buffer for one video track, driven by the caller's sequence and clock.
use crate::media::{Frame, FrameKind};
use std::time::{Duration, Instant};

/// 交付保留首次到达时间，供播放端计算剩余预算。
/// Delivery preserves arrival time for the player's remaining budget.
#[derive(Debug)]
pub enum Delivery {
    Frame(Frame, Instant),
    Discarded,
}

/// 每个观看会话、每条选定视频轨使用独立窗口；输入应为Subscriber校验后的帧。
/// Use one window per session and selected video track, with validated subscriber frames.
pub struct ReorderWindow {
    track: u16,
    queued: Vec<(Frame, Instant)>,
}
impl ReorderWindow {
    pub fn new(track: u16) -> Self {
        Self {
            track,
            queued: Vec::new(),
        }
    }

    /// 进入关键帧恢复时释放积压，并返回丢弃数量。
    /// Clear pending references on recovery and return the number dropped.
    pub fn clear(&mut self) -> usize {
        let count = self.queued.len();
        self.queued.clear();
        count
    }

    /// 下一次需要调用poll的时刻，以最早入窗时间起算。
    /// Next poll deadline, measured from the oldest arrival.
    pub fn deadline(&self) -> Option<Instant> {
        self.queued
            .iter()
            .map(|(_, time)| *time)
            .min()
            .map(|time| time.checked_add(Duration::from_millis(40)).unwrap_or(time))
    }

    /// previous是本轨已处理的最高序号。其他轨直接返回，由调用者分派。
    /// previous is the selected track's processed high-water mark. Other tracks pass through.
    /// None表示已缓存。返回帧仍须经过依赖、期限和解码校验。
    /// None means buffered. Delivered frames still require dependency, deadline and decode checks.
    pub fn offer(&mut self, frame: Frame, previous: u64, arrived: Instant) -> Option<Delivery> {
        if frame.track != self.track {
            return Some(Delivery::Frame(frame, arrived));
        }
        if frame.sequence <= previous
            || self
                .queued
                .iter()
                .any(|(held, _)| held.sequence == frame.sequence)
        {
            return Some(Delivery::Discarded);
        }
        if frame.kind != FrameKind::Reference
            || frame.dependencies.iter().all(|seq| *seq <= previous)
        {
            return Some(Delivery::Frame(frame, arrived));
        }
        if self.queued.len() < 4 {
            self.queued.push((frame, arrived));
            return None;
        }
        // 即使调用者连续offer，也只保留四帧；最小序号先交回恢复逻辑。
        // Consecutive offers still retain at most four frames. Release the lowest sequence first.
        let index = self.first().expect("full window");
        if frame.sequence < self.queued[index].0.sequence {
            return Some(Delivery::Frame(frame, arrived));
        }
        let (frame, arrived) = std::mem::replace(&mut self.queued[index], (frame, arrived));
        Some(Delivery::Frame(frame, arrived))
    }

    fn first(&self) -> Option<usize> {
        self.queued
            .iter()
            .enumerate()
            .min_by_key(|(_, (frame, _))| frame.sequence)
            .map(|(i, _)| i)
    }

    /// 在下一次网络读取前轮询。缺口超时或窗口已满时交付最小序号供调用者判断恢复。
    /// Poll before reading again. On expiry or capacity, release the lowest sequence for recovery.
    pub fn poll(&mut self, previous: u64, now: Instant) -> Option<Delivery> {
        let index = self.first()?;
        let frame = &self.queued[index].0;
        if frame.sequence <= previous {
            self.queued.swap_remove(index);
            return Some(Delivery::Discarded);
        }
        if frame.dependencies.iter().any(|seq| *seq > previous)
            && self.queued.len() < 4
            && now < self.deadline()?
        {
            return None;
        }
        let (frame, arrived) = self.queued.swap_remove(index);
        Some(Delivery::Frame(frame, arrived))
    }
}
