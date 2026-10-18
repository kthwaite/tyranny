//! The player's side of its `PlayOrder`: passes over queue entries, what
//! follows each pass, and which played entries Repeat All keeps.

use super::Player;
use crate::{
    output::{OutputSnapshot, TrackToken},
    play_order::{self, RepeatMode, ShuffleStrategy, Slot},
    store::played_overflow,
};
use std::{collections::HashSet, sync::Arc};

/// One decode of a queue entry. Each pass has its own token, so a replay
/// (Repeat One, a Repeat All wrap, a shuffle revisit) is heard from its start
/// while the previous pass's PCM still plays out.
pub(super) struct Pass {
    pub id: u64,
    pub token: Arc<TrackToken>,
    /// Frames submitted.
    pub frames: usize,
    /// Frames submitted before output started.
    pub prefilled: usize,
    /// Output has started this pass.
    pub heard: bool,
    /// Under Repeat All: this pass starts a fresh pass over its context,
    /// whose entries these are.
    pub fresh: Option<Vec<u64>>,
}

/// What the player decodes after a pass.
#[derive(Default)]
pub(super) struct Plan {
    /// `None` ends the queue.
    pub id: Option<u64>,
    pub fresh: Option<Vec<u64>>,
}

impl Player {
    /// Sets the repeat mode. Prepared passes after the current one are
    /// dropped, and what follows is decided again, as a queue edit does.
    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
        self.order.set_repeat(mode);
        self.replan_future();
    }

    /// Turns shuffle on with `strategy`, or off with `None`. Like `set_repeat`.
    pub fn set_shuffle(&mut self, strategy: Option<Box<dyn ShuffleStrategy>>) {
        self.shuffled = strategy.is_some();
        self.order.set_shuffle(strategy);
        self.replan_future();
    }

    /// Stopped or completed playback has no future to rebuild: it restarts
    /// through a selection, which decides again.
    fn replan_future(&mut self) {
        if !self.stopped && !self.completed() {
            let current = self.snapshot().current_id;
            self.rebuild_future(current);
        }
    }

    pub(crate) fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    /// Records that Previous moved back to `id`, so a second Previous goes
    /// further back.
    pub(crate) fn retreat_to(&mut self, id: u64) {
        self.order.retreated(id);
        self.last_heard = Some(id);
    }

    /// Each pass output started since the last call, oldest first: its
    /// entry, and the entries whose played state a fresh Repeat All pass clears.
    pub(crate) fn take_advances(&mut self) -> Vec<(u64, Vec<u64>)> {
        std::mem::take(&mut self.advances)
    }

    /// Clears the played state of `ids`, as the store does for a fresh pass.
    pub(crate) fn clear_played(&mut self, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        let ids: HashSet<u64> = ids.iter().copied().collect();
        for entry in &mut self.queue {
            if ids.contains(&entry.id) {
                entry.played = false;
            }
        }
    }

    /// The played entries to trim when `id` becomes current (see
    /// `played_overflow`). Under Repeat All the next pass replays `id`'s
    /// context, so its entries are kept; a whole-queue context trims nothing.
    pub(crate) fn trimmed(&self, newest_first: &[u64], id: u64) -> Vec<u64> {
        let mut kept = HashSet::new();
        if let (RepeatMode::All, Some(index)) = (self.repeat, self.index_of(id)) {
            let slots = self.slots();
            let context = play_order::context(&slots, index);
            if context.len() == slots.len() {
                return Vec::new();
            }
            kept.extend(slots[context].iter().map(|slot| slot.id));
        }
        played_overflow(newest_first, |entry| entry == id || kept.contains(&entry))
    }

    pub(super) fn index_of(&self, id: u64) -> Option<usize> {
        self.queue.iter().position(|entry| entry.id == id)
    }

    /// The queue as the play order sees it. An entry is played once heard in
    /// the current pass over its context, or once a pass of it is prepared.
    fn slots(&self) -> Vec<Slot> {
        // Passes from an unheard fresh pass on make up the new pass.
        let fresh = self
            .passes
            .iter()
            .rposition(|pass| pass.fresh.is_some() && !pass.heard);
        let prepared: HashSet<u64> = self
            .passes
            .range(fresh.unwrap_or(0)..)
            .map(|pass| pass.id)
            .collect();
        self.queue
            .iter()
            .map(|entry| Slot {
                id: entry.id,
                run: entry.run,
                played: (fresh.is_none() && entry.visited) || prepared.contains(&entry.id),
            })
            .collect()
    }

    /// Decides what follows a pass over the entry at `from`.
    pub(super) fn decide(&mut self, from: usize) -> Plan {
        let slots = self.slots();
        // The player keeps the plan; each decision here is a new one.
        self.order.invalidate();
        let Some(next) = self.order.next(&slots, Some(from)) else {
            return Plan::default();
        };
        let fresh = self
            .order
            .wrapped(&slots, from, next)
            .map(|context| slots[context].iter().map(|slot| slot.id).collect());
        Plan {
            id: Some(slots[next].id),
            fresh,
        }
    }

    /// Decides again what follows the pass being decoded.
    pub(super) fn replan(&mut self) {
        if self.decoder.is_some() {
            self.plan = self.decide(self.cursor);
        }
    }

    /// Starts a pass over the cursor's entry. It gets a fresh token; earlier
    /// passes keep theirs, as their PCM may still be playing.
    pub(super) fn begin_pass(&mut self) {
        let entry = &mut self.queue[self.cursor];
        entry.token = TrackToken::new(entry.id);
        self.passes.push_back(Pass {
            id: entry.id,
            token: Arc::clone(&entry.token),
            frames: 0,
            prefilled: 0,
            heard: false,
            fresh: self.opening.take(),
        });
        self.plan = self.decide(self.cursor);
    }

    /// The pass being decoded is complete. More passes without PCM in a row
    /// than the queue has entries end the queue instead of looping.
    pub(super) fn end_pass(&mut self) {
        let silent = self.passes.back().is_some_and(|pass| pass.frames == 0);
        self.silent = if silent { self.silent + 1 } else { 0 };
        self.follow_plan();
        if self.silent > self.queue.len() {
            self.cursor = self.queue.len();
            self.opening = None;
        }
    }

    /// Moves the cursor to the planned entry, or past the end.
    pub(super) fn follow_plan(&mut self) {
        let plan = std::mem::take(&mut self.plan);
        self.opening = plan.fresh;
        self.cursor = plan
            .id
            .and_then(|id| self.index_of(id))
            .unwrap_or(self.queue.len());
    }

    /// Notes the passes output has started, oldest first. Passes before the
    /// latest one are complete, and are dropped.
    pub(super) fn hear(&mut self) {
        let rendered = self.output.snapshot().rendered_frames;
        let Some(latest) = self
            .passes
            .iter()
            .rposition(|pass| pass.token.consumed_by(rendered))
        else {
            return;
        };
        for index in 0..=latest {
            let pass = &mut self.passes[index];
            if pass.heard || !pass.token.consumed_by(rendered) {
                continue;
            }
            pass.heard = true;
            let (id, fresh) = (pass.id, pass.fresh.take());
            self.heard(id, fresh);
        }
        self.passes.drain(..latest);
    }

    fn heard(&mut self, id: u64, fresh: Option<Vec<u64>>) {
        if let Some(context) = &fresh {
            let context: HashSet<u64> = context.iter().copied().collect();
            for entry in &mut self.queue {
                if context.contains(&entry.id) {
                    entry.visited = false;
                }
            }
        }
        if let Some(index) = self.index_of(id) {
            self.queue[index].visited = true;
        }
        // Repeat One and seeks hear the same entry again; history keeps one.
        if self.last_heard != Some(id) {
            self.order.advanced(id);
            self.last_heard = Some(id);
        }
        self.advances.push((id, fresh.unwrap_or_default()));
    }

    /// The pass output is playing: the latest valid pass it started for the
    /// entry it reports.
    pub(super) fn heard_pass(&self, output: &OutputSnapshot) -> Option<usize> {
        self.passes.iter().rposition(|pass| {
            pass.id == output.entry_id
                && pass.token.is_valid()
                && pass.token.consumed_by(output.rendered_frames)
        })
    }

    /// The entry after pass `pass`: the next prepared pass, else what the
    /// decoder plans or opens next.
    pub(super) fn following(&self, pass: usize) -> Option<u64> {
        if let Some(next) = self.passes.get(pass + 1) {
            Some(next.id)
        } else if self.decoder.is_some() {
            self.plan.id
        } else if self.finished {
            None
        } else {
            self.queue.get(self.cursor).map(|entry| entry.id)
        }
    }

    /// The entry Previous returns to from `index`. Without shuffle the order
    /// steps back one entry, which needs no slots.
    pub(super) fn previous_index(&self, index: usize) -> Option<usize> {
        if !self.shuffled {
            return index.checked_sub(1);
        }
        // Previous reads only the IDs.
        let slots: Vec<Slot> = self
            .queue
            .iter()
            .map(|entry| Slot {
                id: entry.id,
                run: entry.run,
                played: entry.visited,
            })
            .collect();
        self.order.previous(&slots, index)
    }

    /// Whether output may yet play entry `id` from what is prepared: a pass,
    /// the plan, or the entry the cursor opens next.
    pub(super) fn ahead(&self, id: u64) -> bool {
        self.passes.iter().any(|pass| pass.id == id)
            || self.plan.id == Some(id)
            || (self.decoder.is_none() && self.queue.get(self.cursor).is_some_and(|e| e.id == id))
    }

    /// Keeps `current`'s heard pass, or its first pass when none is heard yet,
    /// and drops every pass after it; with no `current`, every pass. A decoder
    /// on a dropped pass closes.
    pub(super) fn cut_after(&mut self, current: Option<u64>) {
        let rendered = self.output.snapshot().rendered_frames;
        let keep = current.and_then(|id| {
            let ours = |pass: &Pass| pass.id == id && pass.token.is_valid();
            self.passes
                .iter()
                .rposition(|pass| ours(pass) && pass.token.consumed_by(rendered))
                .or_else(|| self.passes.iter().position(ours))
        });
        let from = keep.map_or(0, |keep| keep + 1);
        if from < self.passes.len() {
            self.decoder = None;
            for pass in self.passes.drain(from..) {
                pass.token.invalidate();
                self.prefilled_frames -= pass.prefilled;
            }
        }
        // The kept pass may be older than the entry's latest token.
        if let Some(keep) = keep
            && let Some(index) = self.index_of(self.passes[keep].id)
        {
            self.queue[index].token = Arc::clone(&self.passes[keep].token);
        }
    }

    /// Drops the passes of removed entries. A decoder on one closes.
    pub(super) fn forget_passes(&mut self, removed: &[u64]) {
        if self
            .passes
            .back()
            .is_some_and(|pass| removed.contains(&pass.id))
        {
            self.decoder = None;
            self.pending = 0;
            self.decoded_position = 0;
        }
        let prefilled = &mut self.prefilled_frames;
        self.passes.retain(|pass| {
            let gone = removed.contains(&pass.id);
            if gone {
                pass.token.invalidate();
                *prefilled -= pass.prefilled;
            }
            !gone
        });
    }

    /// Drops every pass and what was planned after them.
    pub(super) fn drop_passes(&mut self) {
        for pass in self.passes.drain(..) {
            pass.token.invalidate();
            self.prefilled_frames -= pass.prefilled;
        }
        self.plan = Plan::default();
        self.opening = None;
        self.silent = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        decoder::GainMode,
        output::OutputConfig,
        play_order::{RepeatScope, RunInfo},
        store::LoadedEntry,
    };
    use std::{fs, path::PathBuf};

    #[test]
    fn repeat_all_trim_keeps_the_current_context() -> anyhow::Result<()> {
        let directory = std::env::temp_dir().join(format!("tyranny-trim-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        // 103 loose songs, then a three-entry album run; every entry is played.
        let loaded = |id: u64, run_id: u64, scope: RepeatScope| LoadedEntry {
            id,
            path: PathBuf::from(format!("/music/{id}.flac")),
            run: Some(RunInfo { run_id, scope }),
            played_seq: Some(id),
        };
        let entries = (1..=103)
            .map(|id| loaded(id, id, RepeatScope::Queue))
            .chain((104..=106).map(|id| loaded(id, 104, RepeatScope::Run)))
            .collect();
        let mut player = Player::from_loaded(
            entries,
            1,
            107,
            OutputConfig::Capture {
                path: directory.join("capture.f32"),
                clocked: false,
            },
            GainMode::Off,
        )?;
        let newest_first: Vec<u64> = (1..=106).rev().collect();
        // Off keeps the newest 100 besides the current entry.
        assert_eq!(player.trimmed(&newest_first, 106), [5, 4, 3, 2, 1]);
        player.set_repeat(RepeatMode::All);
        // The album run is kept whole; 100 of the other played entries stay.
        assert_eq!(player.trimmed(&newest_first, 106), [3, 2, 1]);
        // A song's context is the whole queue: nothing is trimmed.
        assert!(player.trimmed(&newest_first, 50).is_empty());
        drop(player);
        fs::remove_dir_all(&directory)?;
        Ok(())
    }

    #[test]
    fn a_selection_never_inherits_a_planned_fresh_pass() -> anyhow::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("tyranny-fresh-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let source = directory.join("source.wav");
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(36 + 400_u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        for field in [16_u32, 0x0002_0001, 48_000, 192_000, 0x0010_0004] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&400_u32.to_le_bytes());
        bytes.resize(bytes.len() + 400, 0);
        fs::write(&source, bytes)?;
        let mut player = Player::new(
            vec![source.clone(), source],
            OutputConfig::Capture {
                path: directory.join("capture.f32"),
                clocked: false,
            },
            GainMode::Off,
        )?;
        player.set_repeat(RepeatMode::All);
        // The cursor's next pass was planned to start a fresh Repeat All pass.
        player.opening = Some(vec![1, 2]);
        let prepared = player.prepare_selection(2, 0, GainMode::Off)?;
        player.commit_selection(prepared);
        assert!(player.passes.iter().all(|pass| pass.fresh.is_none()));
        drop(player);
        fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
