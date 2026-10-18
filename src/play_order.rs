//! Queue run identity and play order (repeat and shuffle).
//! A run is the batch of entries one enqueue added.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Repeat mode. `All` cycles over the current context (see `PlayOrder`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

/// Picks the next entry under shuffle. Other strategies can implement this later.
pub trait ShuffleStrategy: Send {
    /// Returns an index into `candidates`, which is non-empty.
    fn choose(&mut self, candidates: &[u64]) -> usize;
}

/// Uniform random pick (xorshift64*). Played entries are excluded by `PlayOrder`.
pub struct UniformShuffle {
    state: u64,
}

impl UniformShuffle {
    pub fn seeded(seed: u64) -> Self {
        // xorshift cannot leave the all-zero state.
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    pub fn from_entropy() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Self::seeded(nanos ^ u64::from(std::process::id()))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

impl ShuffleStrategy for UniformShuffle {
    fn choose(&mut self, candidates: &[u64]) -> usize {
        ((u128::from(self.next_u64()) * candidates.len() as u128) >> 64) as usize
    }
}

/// One queue entry as the play order sees it.
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub id: u64,
    pub run: Option<RunInfo>,
    /// Played in the current pass over its context.
    pub played: bool,
}

const HISTORY_LIMIT: usize = 100;

/// Decides next and previous entries under repeat and shuffle.
///
/// The context of an entry is its run when its scope is `Run`, else the whole queue.
/// Repeat All cycles over the context. With repeat off, playback moves on past
/// the context's end, and stops at the end of the queue.
pub struct PlayOrder {
    repeat: RepeatMode,
    shuffle: Option<Box<dyn ShuffleStrategy>>,
    /// Entry id chosen by the last `next`; cached until `invalidate` or `advanced`.
    planned: Option<u64>,
    /// Ids of entries actually played, oldest first.
    history: VecDeque<u64>,
}

impl PlayOrder {
    pub fn new(repeat: RepeatMode, shuffle: Option<Box<dyn ShuffleStrategy>>) -> Self {
        Self {
            repeat,
            shuffle,
            planned: None,
            history: VecDeque::new(),
        }
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
        self.invalidate();
    }

    pub fn set_shuffle(&mut self, s: Option<Box<dyn ShuffleStrategy>>) {
        self.shuffle = s;
        self.invalidate();
    }

    /// Drops the cached decision. Call after any queue edit.
    pub fn invalidate(&mut self) {
        self.planned = None;
    }

    /// Records that `to` started playing on a forward move (next, or a pick).
    /// For Previous use `retreated` instead.
    pub fn advanced(&mut self, to: u64) {
        self.planned = None;
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back(to);
    }

    /// Records that Previous moved back to `to`. Drops history after its latest
    /// occurrence, so a second Previous goes further back.
    pub fn retreated(&mut self, to: u64) {
        self.planned = None;
        if let Some(p) = self.history.iter().rposition(|&id| id == to) {
            self.history.truncate(p + 1);
        }
    }

    /// The entry to play after `current`, decided once until `invalidate` or `advanced`.
    pub fn next(&mut self, slots: &[Slot], current: Option<usize>) -> Option<usize> {
        let cached = self
            .planned
            .and_then(|id| slots.iter().position(|s| s.id == id));
        if cached.is_some() {
            return cached;
        }
        let chosen = self.decide(slots, current)?;
        self.planned = Some(slots[chosen].id);
        Some(chosen)
    }

    fn decide(&mut self, slots: &[Slot], current: Option<usize>) -> Option<usize> {
        if current.is_some_and(|i| i >= slots.len()) {
            return None; // stale index
        }
        if self.repeat == RepeatMode::One && current.is_some() {
            return current;
        }
        let ctx = current.map_or(0..slots.len(), |i| context(slots, i));
        if self.shuffle.is_some() {
            let mut candidates = unplayed(slots, &ctx, current);
            if candidates.is_empty() {
                if self.repeat == RepeatMode::Off {
                    // Off stops only at the end of the queue.
                    return (ctx.end < slots.len()).then_some(ctx.end);
                }
                candidates = ctx.clone().filter(|&k| Some(k) != current).collect();
                if candidates.is_empty() {
                    return current; // single-entry context under Repeat All
                }
            }
            let ids: Vec<u64> = candidates.iter().map(|&k| slots[k].id).collect();
            let pick = self.shuffle.as_mut()?.choose(&ids);
            return candidates.get(pick).copied();
        }
        let Some(i) = current else {
            return (!slots.is_empty()).then_some(0);
        };
        if i + 1 < ctx.end {
            Some(i + 1)
        } else if self.repeat == RepeatMode::All {
            Some(ctx.start)
        } else {
            (ctx.end < slots.len()).then_some(ctx.end)
        }
    }

    /// The entry to return to: the last one actually played under shuffle, else `current - 1`.
    pub fn previous(&self, slots: &[Slot], current: usize) -> Option<usize> {
        if current >= slots.len() {
            return None;
        }
        if self.shuffle.is_some() {
            let here = slots.get(current).map(|s| s.id);
            let back = self
                .history
                .iter()
                .rev()
                .filter(|&&id| Some(id) != here)
                .find_map(|&id| slots.iter().position(|s| s.id == id));
            if back.is_some() {
                return back;
            }
        }
        current.checked_sub(1)
    }

    /// The context whose played flags the caller must reset, when `next` wrapped it.
    pub fn wrapped(&self, slots: &[Slot], current: usize, next: usize) -> Option<Range<usize>> {
        if self.repeat != RepeatMode::All || current >= slots.len() {
            return None;
        }
        let ctx = context(slots, current);
        let wrapped = if self.shuffle.is_some() {
            unplayed(slots, &ctx, Some(current)).is_empty()
        } else {
            next <= current
        };
        wrapped.then_some(ctx)
    }
}

/// The range an entry repeats over.
pub(crate) fn context(slots: &[Slot], i: usize) -> Range<usize> {
    let Some(RunInfo {
        run_id,
        scope: RepeatScope::Run,
    }) = slots[i].run
    else {
        return 0..slots.len();
    };
    let same = |s: &Slot| matches!(s.run, Some(r) if r.run_id == run_id);
    let start = slots[..i]
        .iter()
        .rposition(|s| !same(s))
        .map_or(0, |p| p + 1);
    let end = slots[i..]
        .iter()
        .position(|s| !same(s))
        .map_or(slots.len(), |p| i + p);
    start..end
}

/// Unplayed slots in `ctx` other than `current`, in index order.
fn unplayed(slots: &[Slot], ctx: &Range<usize>, current: Option<usize>) -> Vec<usize> {
    ctx.clone()
        .filter(|&k| Some(k) != current && !slots[k].played)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_slot(id: u64) -> Slot {
        Slot {
            id,
            run: None,
            played: false,
        }
    }

    fn run_slot(id: u64, run_id: u64) -> Slot {
        Slot {
            id,
            run: Some(RunInfo {
                run_id,
                scope: RepeatScope::Run,
            }),
            played: false,
        }
    }

    fn songs(n: u64) -> Vec<Slot> {
        (0..n).map(queue_slot).collect()
    }

    fn seeded(seed: u64) -> Option<Box<dyn ShuffleStrategy>> {
        Some(Box::new(UniformShuffle::seeded(seed)))
    }

    #[test]
    fn sequential_off_ends_at_queue_end() {
        let slots = songs(3);
        let mut order = PlayOrder::new(RepeatMode::Off, None);
        assert_eq!(order.next(&slots, Some(0)), Some(1));
        order.invalidate();
        assert_eq!(order.next(&slots, Some(2)), None);
    }

    #[test]
    fn repeat_one_returns_current() {
        let slots = songs(3);
        let mut order = PlayOrder::new(RepeatMode::One, seeded(1));
        assert_eq!(order.next(&slots, Some(1)), Some(1));
    }

    #[test]
    fn repeat_all_wraps_within_album_run_not_whole_queue() {
        // Entries 0,1 are loose songs; 2,3,4 are one album run; 5 is another run.
        let slots = vec![
            queue_slot(10),
            queue_slot(11),
            run_slot(12, 12),
            run_slot(13, 12),
            run_slot(14, 12),
            run_slot(15, 15),
        ];
        let mut order = PlayOrder::new(RepeatMode::All, None);
        assert_eq!(order.next(&slots, Some(3)), Some(4));
        order.invalidate();
        assert_eq!(order.next(&slots, Some(4)), Some(2));
        assert_eq!(order.wrapped(&slots, 4, 2), Some(2..5));
        assert_eq!(order.wrapped(&slots, 3, 4), None);
    }

    #[test]
    fn off_continues_past_the_run_to_the_queue_end() {
        // Entries 0,1 are one album run; 2 is a loose song after it.
        let mut slots = vec![run_slot(10, 10), run_slot(11, 10), queue_slot(12)];
        let mut order = PlayOrder::new(RepeatMode::Off, None);
        assert_eq!(order.next(&slots, Some(1)), Some(2));
        order.invalidate();
        assert_eq!(order.next(&slots, Some(2)), None);
        // Shuffle picks within the run, then moves on past it.
        let mut order = PlayOrder::new(RepeatMode::Off, seeded(3));
        slots[0].played = true;
        assert_eq!(order.next(&slots, Some(0)), Some(1));
        slots[1].played = true;
        order.invalidate();
        assert_eq!(order.next(&slots, Some(1)), Some(2));
    }

    #[test]
    fn repeat_all_wraps_whole_queue_for_songs_source() {
        let slots = songs(3);
        let mut order = PlayOrder::new(RepeatMode::All, None);
        assert_eq!(order.next(&slots, Some(2)), Some(0));
        assert_eq!(order.wrapped(&slots, 2, 0), Some(0..3));
    }

    #[test]
    fn shuffle_never_repeats_until_context_exhausted() {
        let mut slots = songs(20);
        let mut order = PlayOrder::new(RepeatMode::Off, seeded(42));
        let mut current = 0;
        slots[0].played = true;
        let mut picks = vec![0];
        for _ in 0..19 {
            let next = order.next(&slots, Some(current)).expect("candidate left");
            order.advanced(slots[next].id);
            slots[next].played = true;
            picks.push(next);
            current = next;
        }
        let mut sorted = picks.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 20);
        assert_eq!(order.next(&slots, Some(current)), None);
    }

    #[test]
    fn shuffle_with_all_wraps_and_reports_reset_range() {
        let mut slots = songs(3);
        for s in &mut slots {
            s.played = true;
        }
        let mut order = PlayOrder::new(RepeatMode::All, seeded(7));
        let next = order.next(&slots, Some(1)).expect("wraps");
        assert_ne!(next, 1);
        assert_eq!(order.wrapped(&slots, 1, next), Some(0..3));
        slots[0].played = false;
        assert_eq!(order.wrapped(&slots, 1, 0), None);
    }

    #[test]
    fn next_is_stable_until_invalidated() {
        let slots = songs(50);
        let mut order = PlayOrder::new(RepeatMode::Off, seeded(3));
        let first = order.next(&slots, Some(0));
        for _ in 0..10 {
            assert_eq!(order.next(&slots, Some(0)), first);
        }
        let mut changed = false;
        for _ in 0..20 {
            order.invalidate();
            changed |= order.next(&slots, Some(0)) != first;
        }
        assert!(changed);
    }

    #[test]
    fn previous_follows_actual_history_under_shuffle() {
        let slots = songs(10);
        let mut order = PlayOrder::new(RepeatMode::Off, seeded(5));
        order.advanced(7);
        order.advanced(2);
        order.advanced(9);
        assert_eq!(order.previous(&slots, 9), Some(2));
        let shrunk: Vec<Slot> = slots.into_iter().filter(|s| s.id != 2).collect();
        assert_eq!(order.previous(&shrunk, 8), Some(6)); // id 7 sits at index 6
        let sequential = PlayOrder::new(RepeatMode::Off, None);
        assert_eq!(sequential.previous(&shrunk, 4), Some(3));
        assert_eq!(sequential.previous(&shrunk, 0), None);
    }

    #[test]
    fn previous_walks_back_with_retreated() {
        let slots = songs(10);
        let mut order = PlayOrder::new(RepeatMode::Off, seeded(5));
        order.advanced(7);
        order.advanced(2);
        order.advanced(9);
        assert_eq!(order.previous(&slots, 9), Some(2));
        order.retreated(2);
        assert_eq!(order.previous(&slots, 2), Some(7));
    }

    #[test]
    fn next_without_current_starts_playback() {
        let slots = songs(3);
        for mode in [RepeatMode::Off, RepeatMode::One, RepeatMode::All] {
            let mut order = PlayOrder::new(mode, None);
            assert_eq!(order.next(&slots, None), Some(0), "{mode:?}");
        }
        for mode in [RepeatMode::Off, RepeatMode::One, RepeatMode::All] {
            let mut order = PlayOrder::new(mode, seeded(11));
            assert!(order.next(&slots, None).is_some_and(|i| i < 3), "{mode:?}");
        }
        let mut order = PlayOrder::new(RepeatMode::Off, None);
        assert_eq!(order.next(&[], None), None);
    }

    #[test]
    fn out_of_range_current_returns_none() {
        let slots = songs(3);
        for shuffle in [None, seeded(1)] {
            let mut order = PlayOrder::new(RepeatMode::All, shuffle);
            assert_eq!(order.next(&slots, Some(5)), None);
            assert_eq!(order.previous(&slots, 5), None);
            assert_eq!(order.wrapped(&slots, 5, 0), None);
        }
    }

    #[test]
    fn uniform_shuffle_seeded_is_deterministic() {
        let candidates: Vec<u64> = (0..100).collect();
        let mut a = UniformShuffle::seeded(99);
        let mut b = UniformShuffle::seeded(99);
        let run_a: Vec<usize> = (0..20).map(|_| a.choose(&candidates)).collect();
        let run_b: Vec<usize> = (0..20).map(|_| b.choose(&candidates)).collect();
        assert_eq!(run_a, run_b);
        assert!(run_a.iter().all(|&i| i < 100));
        let mut zero = UniformShuffle::seeded(0);
        assert!((0..20).map(|_| zero.choose(&candidates)).any(|i| i != 0));
    }

    #[test]
    fn repeat_mode_serialises_snake_case() {
        assert_eq!(serde_json::to_string(&RepeatMode::All).unwrap(), "\"all\"");
        assert_eq!(RepeatMode::default(), RepeatMode::Off);
    }
}
