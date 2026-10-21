//! Station lookup against radio-browser.info, independent of the playback owner.
mod mirrors;
mod provider;
#[cfg(test)]
mod tests;

use anyhow::{Result, ensure};
#[cfg_attr(not(test), expect(unused_imports))] // wired in by the directory protocol commands
pub(crate) use provider::Provider;
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
impl Cancellation {
    pub(crate) fn new(stop: Arc<AtomicBool>, abandoned: Arc<AtomicBool>) -> Self {
        Self { stop, abandoned }
    }
    #[cfg(test)]
    fn idle() -> Self {
        Self::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
    }
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Acquire) || self.abandoned.load(Ordering::Acquire)
    }
    fn check(&self, deadline: Instant) -> Result<()> {
        ensure!(!self.cancelled(), "directory request cancelled");
        ensure!(
            Instant::now() < deadline,
            "directory search deadline exceeded"
        );
        Ok(())
    }
}
