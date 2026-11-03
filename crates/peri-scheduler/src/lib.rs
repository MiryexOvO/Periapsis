//! Bounded, dependency-aware media policy. Deadlines use local `Instant` values.
//! This queue advises the application; congestion control remains with QUIC.
//! 有界且感知解码依赖的媒体策略，使用本地单调时钟；拥塞控制仍由 QUIC 负责。

use std::{collections::HashSet, fmt, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Audio,
    Key,
    Reference,
    Disposable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub id: u64,
    pub track: String,
    pub bytes: usize,
    pub deadline: Instant,
    pub kind: Kind,
    pub dependencies: Vec<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Send,
    Drop,
    Recover,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub action: Action,
    pub unit: Unit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    InvalidBounds,
    InvalidUnit,
    Duplicate,
    Full,
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "media queue: {self:?}")
    }
}
impl std::error::Error for QueueError {}

pub struct DeadlineQueue {
    pending: Vec<Unit>,
    used_bytes: usize,
    max_bytes: usize,
    max_units: usize,
}

impl DeadlineQueue {
    pub fn new(max_bytes: usize, max_units: usize) -> Result<Self, QueueError> {
        if max_bytes == 0 || max_units == 0 {
            return Err(QueueError::InvalidBounds);
        }
        Ok(Self {
            pending: Vec::new(),
            used_bytes: 0,
            max_bytes,
            max_units,
        })
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
    pub fn bytes(&self) -> usize {
        self.used_bytes
    }

    pub fn push(&mut self, unit: Unit) -> Result<(), QueueError> {
        if unit.bytes == 0
            || unit.track.is_empty()
            || unit.track.len() > 256
            || unit.dependencies.len() > 32
            || unit.dependencies.contains(&unit.id)
        {
            return Err(QueueError::InvalidUnit);
        }
        if self.pending.iter().any(|u| u.id == unit.id) {
            return Err(QueueError::Duplicate);
        }
        let total = self
            .used_bytes
            .checked_add(unit.bytes)
            .ok_or(QueueError::Full)?;
        if total > self.max_bytes || self.len() >= self.max_units {
            return Err(QueueError::Full);
        }
        self.pending.push(unit);
        self.used_bytes = total;
        Ok(())
    }

    pub fn next(&mut self, now: Instant, available: &HashSet<u64>) -> Option<Decision> {
        if let Some(index) = self.pending.iter().position(|u| u.deadline <= now) {
            let unit = self.take(index);
            let action = if unit.kind == Kind::Disposable {
                Action::Drop
            } else {
                Action::Recover
            };
            return Some(Decision { action, unit });
        }
        let index = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, u)| u.dependencies.iter().all(|d| available.contains(d)))
            .min_by_key(|(_, u)| (u.deadline, u.kind))
            .map(|(i, _)| i)?;
        Some(Decision {
            action: Action::Send,
            unit: self.take(index),
        })
    }

    fn take(&mut self, index: usize) -> Unit {
        let unit = self.pending.remove(index);
        self.used_bytes -= unit.bytes;
        unit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn unit(id: u64, kind: Kind, deadline: Instant) -> Unit {
        Unit {
            id,
            kind,
            deadline,
            bytes: 100,
            track: "video".into(),
            dependencies: vec![],
        }
    }

    #[test]
    fn overflow_does_not_change_accounting() {
        let now = Instant::now();
        let mut queue = DeadlineQueue::new(100, 1).unwrap();
        queue
            .push(unit(1, Kind::Key, now + Duration::from_secs(1)))
            .unwrap();
        assert_eq!(
            queue.push(unit(2, Kind::Reference, now)),
            Err(QueueError::Full)
        );
        assert_eq!(queue.bytes(), 100);
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue.next(now, &HashSet::new()).unwrap().action,
            Action::Send
        );
        assert_eq!(queue.bytes(), 0);
    }

    #[test]
    fn expired_reference_requests_recovery_but_disposable_can_drop() {
        let now = Instant::now();
        let mut queue = DeadlineQueue::new(1000, 4).unwrap();
        queue.push(unit(1, Kind::Reference, now)).unwrap();
        queue.push(unit(2, Kind::Disposable, now)).unwrap();
        assert_eq!(
            queue.next(now, &HashSet::new()).unwrap().action,
            Action::Recover
        );
        assert_eq!(
            queue.next(now, &HashSet::new()).unwrap().action,
            Action::Drop
        );
    }

    #[test]
    fn dependency_order_overrides_earlier_deadline() {
        let now = Instant::now();
        let mut queue = DeadlineQueue::new(1000, 4).unwrap();
        let mut dependent = unit(2, Kind::Reference, now + Duration::from_millis(50));
        dependent.dependencies = vec![1];
        queue.push(dependent).unwrap();
        queue
            .push(unit(1, Kind::Key, now + Duration::from_millis(60)))
            .unwrap();
        assert_eq!(queue.next(now, &HashSet::new()).unwrap().unit.id, 1);
        assert!(queue.next(now, &HashSet::new()).is_none());
        assert_eq!(queue.next(now, &HashSet::from([1])).unwrap().unit.id, 2);
    }

    #[test]
    fn byte_accounting_cannot_overflow() {
        let now = Instant::now();
        let mut queue = DeadlineQueue::new(usize::MAX, 4).unwrap();
        let mut huge = unit(1, Kind::Key, now);
        huge.bytes = usize::MAX;
        queue.push(huge).unwrap();
        assert_eq!(
            queue.push(unit(2, Kind::Reference, now)),
            Err(QueueError::Full)
        );
        assert_eq!(queue.bytes(), usize::MAX);
    }
}
