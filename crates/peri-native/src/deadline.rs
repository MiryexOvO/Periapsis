//! 同一操作跨排队、读写和确认阶段共用一个本地截止时间。
//! One local deadline covers queueing, I/O and acknowledgement.
use crate::Error;
use std::{
    future::Future,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(crate) struct Deadline(Instant);
impl Deadline {
    pub(crate) fn after(budget: Duration) -> Result<Self, Error> {
        Instant::now()
            .checked_add(budget)
            .map(Self)
            .ok_or(Error::Protocol("deadline overflow"))
    }
    pub(crate) fn at(at: Instant) -> Self {
        Self(at)
    }
    pub(crate) fn instant(self) -> Instant {
        self.0
    }
    fn check_at(self, now: Instant) -> Result<(), Error> {
        if now >= self.0 {
            Err(Error::Timeout)
        } else {
            Ok(())
        }
    }
    pub(crate) async fn run<F: Future>(self, future: F) -> Result<F::Output, Error> {
        // 就绪future也必须服从期限；仅包一层timeout不能保证这一点。
        // Ready futures must respect expiry too; timeout alone does not enforce this.
        self.check_at(Instant::now())?;
        let result = tokio::time::timeout_at(self.0.into(), future)
            .await
            .map_err(|_| Error::Timeout)?;
        self.check_at(Instant::now())?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 边界恰好到期即失效，复制期限不会延长寿命。
    // Expiry is inclusive; copying a deadline cannot extend it.
    #[test]
    fn exact_boundary_expires() {
        let now = Instant::now();
        let deadline = Deadline::at(now + Duration::from_millis(10));
        assert!(deadline.check_at(now).is_ok());
        assert!(matches!(
            deadline.check_at(now + Duration::from_millis(10)),
            Err(Error::Timeout)
        ));
        assert!(matches!(
            deadline.check_at(now + Duration::from_millis(11)),
            Err(Error::Timeout)
        ));
    }
    // 过期操作不能执行副作用，即使future第一次poll就能完成。
    // An expired operation must not run side effects, even if its first poll would complete.
    #[tokio::test]
    async fn expired_ready_future_is_not_polled() {
        let called = std::sync::atomic::AtomicBool::new(false);
        let deadline = Deadline::at(Instant::now());
        let result = deadline
            .run(async {
                called.store(true, std::sync::atomic::Ordering::Relaxed);
            })
            .await;
        assert!(matches!(result, Err(Error::Timeout)));
        assert!(!called.load(std::sync::atomic::Ordering::Relaxed));
    }
}
