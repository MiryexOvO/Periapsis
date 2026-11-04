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
