//! Cancellation shared by the lyrics and tagging workers.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[derive(Clone)]
pub(crate) struct Cancellation {
    stop: Arc<AtomicBool>,
    abandoned: Arc<AtomicBool>,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    Expired,
}
impl Cancellation {
    pub(crate) fn with_flags(stop: Arc<AtomicBool>, abandoned: Arc<AtomicBool>) -> Self {
        Self { stop, abandoned }
    }
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_flags(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
    }
    pub(crate) fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Acquire) || self.abandoned.load(Ordering::Acquire)
    }
    /// Cancellation wins over an expired deadline.
    pub(crate) fn stopped(&self, deadline: Instant) -> Option<Stop> {
        if self.cancelled() {
            Some(Stop::Cancelled)
        } else if Instant::now() >= deadline {
            Some(Stop::Expired)
        } else {
            None
        }
    }
}
/// Sets its flag when dropped, so abandoned requests stop their workers.
pub(crate) struct CancelOnDrop(pub(crate) Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stopped_reports_cancel_before_deadline() {
        let future = Instant::now() + Duration::from_secs(60);
        let past = Instant::now() - Duration::from_secs(1);
        let cancel = Cancellation::new();
        assert_eq!(cancel.stopped(future), None);
        assert_eq!(cancel.stopped(past), Some(Stop::Expired));
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Cancellation::with_flags(Arc::clone(&stop), Arc::new(AtomicBool::new(false)));
        stop.store(true, Ordering::Release);
        assert_eq!(cancel.stopped(future), Some(Stop::Cancelled));
        assert_eq!(cancel.stopped(past), Some(Stop::Cancelled));
        let flag = Arc::new(AtomicBool::new(false));
        drop(CancelOnDrop(Arc::clone(&flag)));
        assert!(flag.load(Ordering::Acquire));
    }
}
