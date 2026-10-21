//! Proposals waiting to be applied, and releases fetched recently. Both live
//! in the worker's memory only.
use super::{musicbrainz::Release, safe_write::FileStamp};
use crate::protocol::TagField;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::{Duration, Instant},
};

pub(super) const PROPOSAL_TTL: Duration = Duration::from_secs(15 * 60);
pub(super) const MAX_PROPOSALS: usize = 8;
const RELEASE_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_RELEASES: usize = 32;

/// One file a proposal writes: only its writable, changed fields.
pub(super) struct ProposedFile {
    pub track_id: u64,
    pub path: PathBuf,
    pub stamp: FileStamp,
    pub values: BTreeMap<TagField, String>,
}

pub(super) struct Proposal {
    pub release_id: String,
    pub release_title: String,
    pub files: Vec<ProposedFile>,
}

/// Entries keyed by ID, each with the time it was stored. Entries older than
/// the TTL are gone. A full map drops its oldest entry.
struct Expiring<T> {
    entries: HashMap<String, (Instant, T)>,
    ttl: Duration,
    capacity: usize,
}
impl<T> Expiring<T> {
    fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
            capacity,
        }
    }
    fn fresh(&self, now: Instant, stored: Instant) -> bool {
        now.saturating_duration_since(stored) < self.ttl
    }
    fn insert(&mut self, now: Instant, id: String, value: T) {
        let ttl = self.ttl;
        self.entries
            .retain(|_, (stored, _)| now.saturating_duration_since(*stored) < ttl);
        self.entries.remove(&id);
        while self.entries.len() >= self.capacity {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (stored, _))| *stored)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.entries.insert(id, (now, value));
    }
    fn get(&mut self, now: Instant, id: &str) -> Option<&T> {
        let (stored, _) = self.entries.get(id)?;
        if !self.fresh(now, *stored) {
            self.entries.remove(id);
            return None;
        }
        self.entries.get(id).map(|(_, value)| value)
    }
    fn take(&mut self, now: Instant, id: &str) -> Option<T> {
        let (stored, value) = self.entries.remove(id)?;
        self.fresh(now, stored).then_some(value)
    }
}

/// Proposals by `proposal_id`.
pub(super) struct Proposals(Expiring<Proposal>);
impl Default for Proposals {
    fn default() -> Self {
        Self(Expiring::new(PROPOSAL_TTL, MAX_PROPOSALS))
    }
}
impl Proposals {
    /// Stores the proposal under a new random ID, and returns the ID.
    pub fn insert(&mut self, now: Instant, proposal: Proposal) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.0.insert(now, id.clone(), proposal);
        id
    }
    /// Removes the proposal. `None` if it is unknown or expired.
    pub fn take(&mut self, now: Instant, id: &str) -> Option<Proposal> {
        self.0.take(now, id)
    }
}

/// Releases by MusicBrainz ID.
pub(super) struct ReleaseCache(Expiring<Release>);
impl Default for ReleaseCache {
    fn default() -> Self {
        Self(Expiring::new(RELEASE_TTL, MAX_RELEASES))
    }
}
impl ReleaseCache {
    pub fn get(&mut self, now: Instant, id: &str) -> Option<Release> {
        self.0.get(now, id).cloned()
    }
    pub fn put(&mut self, now: Instant, release: Release) {
        self.0.insert(now, release.id.clone(), release);
    }
}
