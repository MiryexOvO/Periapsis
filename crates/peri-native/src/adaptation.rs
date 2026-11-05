//! Experimental single-viewer media budget; QUIC still owns congestion control.
use crate::ReceiverReport;
use std::time::{Duration, Instant};
pub struct RateController {
    rate: u32,
    viewer: Option<u64>,
    sequence: u64,
    sample: u64,
    dropped: u64,
    recoveries: u32,
    healthy: u8,
    changed: Instant,
}
impl RateController {
    pub fn new(now: Instant) -> Self {
        Self {
            rate: 300000,
            viewer: None,
            sequence: 0,
            sample: 0,
            dropped: 0,
            recoveries: 0,
            healthy: 0,
            changed: now - Duration::from_secs(1),
        }
    }
    pub fn update(&mut self, report: ReceiverReport, now: Instant) -> Option<u32> {
        if !report.valid() {
            return None;
        }
        if self.viewer != Some(report.viewer) {
            self.viewer = Some(report.viewer);
            self.sequence = 0;
            self.sample = 0;
            self.dropped = 0;
            self.recoveries = 0;
            self.healthy = 0;
        }
        if report.sample <= self.sample
            || report.sequence < self.sequence
            || report.dropped < self.dropped
            || report.recoveries < self.recoveries
        {
            return None;
        }
        let bad = report.lateness_ms > 80
            || report.buffered_ms > 150
            || report.dropped > self.dropped
            || report.recoveries > self.recoveries;
        self.sample = report.sample;
        self.sequence = report.sequence;
        self.dropped = report.dropped;
        self.recoveries = report.recoveries;
        self.healthy = if bad {
            0
        } else {
            self.healthy.saturating_add(1)
        };
        if now.duration_since(self.changed) < Duration::from_secs(1) {
            return None;
        }
        let rate = if bad {
            (self.rate * 3 / 4).max(100000)
        } else if self.healthy >= 5 {
            (self.rate * 11 / 10).min(300000)
        } else {
            self.rate
        };
        if rate == self.rate {
            return None;
        }
        self.rate = rate;
        self.changed = now;
        self.healthy = 0;
        Some(rate)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn report(sample: u64, bad: bool) -> ReceiverReport {
        ReceiverReport {
            track: 1,
            viewer: 1,
            sample,
            sequence: sample,
            media_us: 0,
            buffered_ms: 0,
            queued_bytes: 0,
            lateness_ms: if bad { 100 } else { 0 },
            dropped: 0,
            recoveries: 0,
        }
    }
    #[test]
    fn bounds_cooldown_replay_and_recovery() {
        let now = Instant::now();
        let mut controller = RateController::new(now);
        assert_eq!(controller.update(report(1, true), now), Some(225000));
        assert_eq!(controller.update(report(2, true), now), None);
        assert_eq!(
            controller.update(report(1, true), now + Duration::from_secs(1)),
            None
        );
        for n in 3..15 {
            controller.update(report(n, true), now + Duration::from_secs(n));
        }
        assert_eq!(controller.rate, 100000);
        for n in 15..20 {
            controller.update(report(n, false), now + Duration::from_secs(n));
        }
        assert_eq!(controller.rate, 110000);
        for n in 20..100 {
            controller.update(report(n, false), now + Duration::from_secs(n));
        }
        assert_eq!(controller.rate, 300000);
    }
}

/// Coalesce target changes into recovery or predicted natural-keyframe slots.
#[derive(Default)]
pub struct SwitchSchedule {
    last_key: Option<u64>,
    last_switch: Option<u64>,
}
impl SwitchSchedule {
    pub fn rate_for(&self, frame: u64, applied: u32, desired: u32, recovery: bool) -> u32 {
        let desired = match desired {
            0..=149999 => 100000,
            150000..=224999 => 150000,
            225000..=299999 => 225000,
            _ => 300000,
        };
        let due = self
            .last_key
            .is_none_or(|key| frame.saturating_sub(key) >= 60);
        let cooled = self
            .last_switch
            .is_none_or(|at| frame.saturating_sub(at) >= 45);
        if desired != applied && cooled && (recovery || due) {
            desired
        } else {
            applied
        }
    }
    pub fn encoded(&mut self, frame: u64, key: bool, switched: bool) {
        if key {
            self.last_key = Some(frame);
        }
        if switched {
            self.last_switch = Some(frame);
        }
    }
}
#[cfg(test)]
mod switch_tests {
    use super::*;
    #[test]
    fn rate_only_changes_wait_and_merge_with_recovery() {
        let mut s = SwitchSchedule::default();
        s.encoded(0, true, false);
        for frame in 1..60 {
            assert_eq!(s.rate_for(frame, 300000, 225000, false), 300000);
        }
        assert_eq!(s.rate_for(60, 300000, 225000, false), 225000);
        assert_eq!(s.rate_for(15, 300000, 168750, true), 150000);
        s.encoded(15, true, true);
        assert_eq!(s.rate_for(30, 150000, 126562, true), 150000);
        assert_eq!(s.rate_for(60, 150000, 126562, true), 100000);
        assert_eq!(s.rate_for(60, 150000, 126562, false), 150000);
        assert_eq!(s.rate_for(75, 150000, 126562, false), 100000);
    }
}

/// Coalesces recovery demand without postponing it on repeated requests.
/// The caller confirms actual independent encoding with `encoded`.
/// 合并恢复需求，重复请求不延后冷却；只有实际编码出独立帧后才能确认。
pub struct RecoveryPacer {
    interval: Duration,
    last: Option<Instant>,
    pending: bool,
}
impl RecoveryPacer {
    /// 冷却限定为300至1000毫秒；超出范围拒绝，避免无界关键帧突发。
    pub fn new(interval: Duration) -> Option<Self> {
        if !(Duration::from_millis(300)..=Duration::from_secs(1)).contains(&interval) {
            return None;
        }
        Some(Self {
            interval,
            last: None,
            pending: false,
        })
    }
    /// 合并请求但不重置上次编码时间，持续请求不会饿死恢复。
    pub fn request(&mut self) {
        self.pending = true;
    }
    /// 只检查单调时钟和待处理需求，不提前消耗恢复机会。
    pub fn ready(&self, now: Instant) -> bool {
        self.pending
            && self
                .last
                .is_none_or(|last| now.saturating_duration_since(last) >= self.interval)
    }
    /// 撤销已被实际播放进度覆盖的需求，不重置编码冷却时间。
    pub fn clear(&mut self) {
        self.pending = false;
    }
    /// 实际独立帧编码成功后清除需求并开始冷却，编码失败不得调用。
    pub fn encoded(&mut self, now: Instant) {
        self.pending = false;
        self.last = Some(now);
    }
}
#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn repeated_demand_cannot_starve_recovery_or_exceed_time_budget() {
        let t = Instant::now();
        let mut p = RecoveryPacer::new(Duration::from_millis(333)).unwrap();
        assert!(!p.ready(t));
        p.request();
        assert!(p.ready(t));
        p.encoded(t);
        for ms in 1..333 {
            p.request();
            assert!(!p.ready(t + Duration::from_millis(ms)));
        }
        assert!(p.ready(t + Duration::from_millis(333)));
        p.clear();
        assert!(!p.ready(t + Duration::from_millis(333)));
        p.request();
        assert!(p.ready(t + Duration::from_millis(333)));
        p.encoded(t + Duration::from_millis(333));
        assert!(!p.ready(t + Duration::from_secs(10)));
        p.request();
        assert!(p.ready(t + Duration::from_secs(10)));
        assert!(RecoveryPacer::new(Duration::ZERO).is_none());
        assert!(RecoveryPacer::new(Duration::from_secs(2)).is_none());
    }
}

/// 有界的接收进度窗口；只抑制同作用域中已被更新实际播放序号覆盖的请求。
#[derive(Default)]
pub struct RecoveryProgress {
    entries: std::collections::VecDeque<(ReceiverReport, Instant)>,
}
impl RecoveryProgress {
    /// 验证报告单调性，拒绝重放、序号回退和累计计数回退；最多保留8个轨道/观看者组合。
    pub fn observe(&mut self, report: ReceiverReport, now: Instant) -> bool {
        if !report.valid() {
            return false;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|(old, _)| old.track == report.track && old.viewer == report.viewer)
        {
            let old = self.entries[index].0;
            if report.sample <= old.sample
                || report.sequence < old.sequence
                || report.dropped < old.dropped
                || report.recoveries < old.recoveries
            {
                return false;
            }
            self.entries.remove(index);
        }
        if self.entries.len() == 8 {
            self.entries.pop_front();
        }
        self.entries.push_back((report, now));
        true
    }
    /// 未知位置、不同观看者、相等序号或已老化报告均不能证明恢复。
    pub fn obsolete(&self, request: crate::RecoveryRequest, now: Instant) -> bool {
        request.after_sequence > 0
            && self.entries.iter().any(|(report, at)| {
                report.track == request.track
                    && report.viewer == request.viewer
                    && report.sequence > request.after_sequence
                    && now
                        .checked_duration_since(*at)
                        .is_some_and(|age| age <= Duration::from_secs(2))
            })
    }
}
#[cfg(test)]
mod progress_tests {
    use super::*;
    fn report(sample: u64, viewer: u64, sequence: u64) -> ReceiverReport {
        ReceiverReport {
            track: 1,
            viewer,
            sample,
            sequence,
            media_us: 0,
            buffered_ms: 0,
            queued_bytes: 0,
            lateness_ms: 0,
            dropped: 0,
            recoveries: 0,
        }
    }
    #[test]
    fn confirmation_must_be_newer_same_viewer_and_fresh() {
        let t = Instant::now();
        let mut w = RecoveryProgress::default();
        w.observe(report(2, 7, 90), t);
        assert!(w.obsolete(crate::RecoveryRequest::after(1, 7, 80), t));
        assert!(!w.obsolete(
            crate::RecoveryRequest::after(1, 7, 80),
            t - Duration::from_millis(1)
        ));
        for r in [
            crate::RecoveryRequest::after(1, 8, 80),
            crate::RecoveryRequest::after(2, 7, 80),
            crate::RecoveryRequest::after(1, 7, 90),
            crate::RecoveryRequest::after(1, 7, 91),
            crate::RecoveryRequest::after(1, 7, 0),
        ] {
            assert!(!w.obsolete(r, t));
        }
        assert!(!w.obsolete(
            crate::RecoveryRequest::after(1, 7, 80),
            t + Duration::from_millis(2001)
        ));
        assert!(!w.observe(report(2, 7, 100), t + Duration::from_secs(1)));
        assert!(!w.observe(report(3, 7, 89), t + Duration::from_secs(1)));
        let mut advanced = report(3, 7, 91);
        advanced.dropped = 2;
        advanced.recoveries = 1;
        assert!(w.observe(advanced, t));
        let mut rollback = report(4, 7, 92);
        rollback.recoveries = 1;
        assert!(!w.observe(rollback, t + Duration::from_secs(1)));
        rollback.dropped = 2;
        rollback.recoveries = 0;
        assert!(!w.observe(rollback, t + Duration::from_secs(1)));
        assert!(!w.obsolete(
            crate::RecoveryRequest::after(1, 7, 80),
            t + Duration::from_millis(2001)
        ));
    }
    #[test]
    fn bounded_window_eviction_cannot_prove_unknown_viewer_recovered() {
        let t = Instant::now();
        let mut w = RecoveryProgress::default();
        for viewer in 0..9 {
            assert!(w.observe(report(1, viewer, 10), t));
        }
        assert_eq!(w.entries.len(), 8);
        assert!(!w.obsolete(crate::RecoveryRequest::after(1, 0, 5), t));
    }
}
