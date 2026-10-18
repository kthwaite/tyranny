//! Queue run identity. A run is the batch of entries one enqueue added.

/// What Repeat All cycles over for an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepeatScope {
    /// The entry's own run (album, playlist, smart playlist).
    Run,
    /// The whole queue.
    Queue,
}

/// The run an entry belongs to. `run_id` is the id of the run's first entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunInfo {
    pub run_id: u64,
    pub scope: RepeatScope,
}
