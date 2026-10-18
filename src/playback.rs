//! Single-owner playback state. Drive `pump` on a worker, never the UI/audio thread.
//! Decoding may perform disk I/O; output consumption is independent and bounded.
use crate::{
    BLOCK_FRAMES, Frame, SAMPLE_RATE,
    decoder::{Decoder, GainMode},
    output::{Output, OutputConfig, OutputSnapshot, TrackToken},
    play_order::{RepeatScope, RunInfo},
    protocol::NowPlaying,
    store::LoadedEntry,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

const PREBUFFER_FRAMES: usize = SAMPLE_RATE as usize / 2;

struct Entry {
    id: u64,
    path: PathBuf,
    token: Arc<TrackToken>,
    prefilled_frames: usize,
    metadata: Option<Arc<NowPlaying>>,
    /// Read by Repeat All to find the run an entry repeats within.
    #[allow(dead_code)]
    run: Option<RunInfo>,
}

/// A validated decoder replacement. Commit on the same player without an
/// intervening queue mutation; output consumption may continue in the meantime.
pub struct PreparedSelection {
    revision: u64,
    index: usize,
    id: u64,
    frame: u64,
    mode: GainMode,
    decoder: Decoder,
    metadata: Arc<NowPlaying>,
}

/// Resources for an atomic playlist append. Preparation may reserve capacity,
/// but does not change queue identity, play intent, selection, or buffered PCM.
pub(crate) struct PreparedAppend {
    revision: u64,
    next_id: u64,
    entries: Vec<Entry>,
    selection: Option<PreparedSelection>,
    replacement_tokens: Vec<Arc<TrackToken>>,
}
impl PreparedAppend {
    pub fn entries(&self) -> impl ExactSizeIterator<Item = (u64, &Path)> {
        self.entries
            .iter()
            .map(|entry| (entry.id, entry.path.as_path()))
    }
    pub fn first_id(&self) -> u64 {
        self.entries[0].id
    }
}

/// A permutation and replacement tokens prepared without touching audible state.
pub(crate) struct PreparedMove {
    revision: u64,
    destinations: Vec<usize>,
    tokens: Vec<Arc<TrackToken>>,
}

pub(crate) struct PreparedInsert {
    append: PreparedAppend,
    destination: usize,
    tokens: Vec<Arc<TrackToken>>,
}
impl PreparedInsert {
    pub fn entries(&self) -> impl ExactSizeIterator<Item = (u64, &Path)> {
        self.append.entries()
    }
    pub fn first_id(&self) -> u64 {
        self.append.first_id()
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub output: OutputSnapshot,
    pub queue_revision: u64,
    pub current_id: Option<u64>,
    pub position_frames: u64,
    pub next_id: Option<u64>,
    pub previous_id: Option<u64>,
    pub now_playing: Option<Arc<NowPlaying>>,
    pub prepared_id: Option<u64>,
    pub decoding_finished: bool,
    pub stopped: bool,
    pub playing: bool,
}

pub struct Player {
    output: Output,
    queue: Vec<Entry>,
    revision: u64,
    next_id: u64,
    mode: GainMode,
    cursor: usize,
    decoder: Option<Decoder>,
    block: Box<[Frame; BLOCK_FRAMES]>,
    pending: usize,
    decoded_position: u64,
    finished: bool,
    failure: Option<String>,
    playing: bool,
    started: bool,
    prefilled_frames: usize,
    stopped: bool,
    selection: Option<(u64, u64)>,
    gains: Vec<f64>,
    starts: Vec<u64>,
}

impl Player {
    pub fn new(paths: Vec<PathBuf>, output: OutputConfig, mode: GainMode) -> Result<Self> {
        let next_id = (paths.len() as u64)
            .checked_add(1)
            .context("queue ID exhausted")?;
        let queue = paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let id = index as u64 + 1;
                Entry {
                    id,
                    path,
                    token: TrackToken::new(id),
                    prefilled_frames: 0,
                    metadata: None,
                    run: None,
                }
            })
            .collect();
        Self::open(queue, 1, next_id, output, mode)
    }

    /// Restore queue identity and order without opening any source or playing.
    pub fn from_queue(
        entries: Vec<(u64, PathBuf)>,
        revision: u64,
        next_id: u64,
        output: OutputConfig,
        mode: GainMode,
    ) -> Result<Self> {
        let entries = entries
            .into_iter()
            .map(|(id, path)| LoadedEntry {
                id,
                path,
                run: None,
                played_seq: None,
            })
            .collect();
        Self::from_loaded(entries, revision, next_id, output, mode)
    }

    pub(crate) fn from_loaded(
        entries: Vec<LoadedEntry>,
        revision: u64,
        next_id: u64,
        output: OutputConfig,
        mode: GainMode,
    ) -> Result<Self> {
        ensure!(next_id != 0, "next queue ID must be nonzero");
        let mut ids = HashSet::with_capacity(entries.len());
        for LoadedEntry { id, .. } in &entries {
            ensure!(*id != 0 && *id < next_id, "invalid restored queue ID {id}");
            ensure!(ids.insert(*id), "duplicate restored queue ID {id}");
        }
        let queue = entries
            .into_iter()
            .map(|entry| Entry {
                id: entry.id,
                path: entry.path,
                token: TrackToken::new(entry.id),
                prefilled_frames: 0,
                metadata: None,
                run: entry.run,
            })
            .collect();
        Self::open(queue, revision, next_id, output, mode)
    }

    fn open(
        queue: Vec<Entry>,
        revision: u64,
        next_id: u64,
        output: OutputConfig,
        mode: GainMode,
    ) -> Result<Self> {
        Ok(Self {
            output: Output::open(output)?,
            queue,
            revision,
            next_id,
            mode,
            cursor: 0,
            decoder: None,
            block: Box::new([[0.0; 2]; BLOCK_FRAMES]),
            pending: 0,
            decoded_position: 0,
            finished: false,
            failure: None,
            playing: false,
            started: false,
            prefilled_frames: 0,
            stopped: true,
            selection: None,
            gains: Vec::new(),
            starts: Vec::new(),
        })
    }

    pub fn play(&mut self) {
        if self.completed() {
            self.stop();
        }
        self.playing = true;
        self.stopped = false;
        self.maybe_start();
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.output.set_paused(true);
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.stopped = true;
        self.output.set_paused(true);
        self.invalidate_buffer();
        self.decoder = None;
        self.pending = 0;
        self.cursor = 0;
        self.decoded_position = 0;
        self.finished = false;
        self.failure = None;
        self.selection = None;
        self.output.mark_end(false);
    }

    fn invalidate_buffer(&mut self) {
        self.started = false;
        self.prefilled_frames = 0;
        for entry in &mut self.queue {
            entry.token.invalidate();
            entry.token = TrackToken::new(entry.id);
            entry.prefilled_frames = 0;
            entry.metadata = None;
        }
    }

    /// Returns after at most one decoded/submitted block, so commands can be
    /// serialized between calls even when a paused output has filled its pool.
    pub fn pump(&mut self) -> Result<()> {
        self.output.check_error()?;
        while let Some(id) = self.output.take_started() {
            self.starts.push(id);
        }
        if self.stopped {
            return Ok(());
        }
        if !self.finished
            && self.failure.is_none()
            && let Err(error) = self.produce()
        {
            self.failure = Some(format!("{error:#}"));
            self.decoder = None;
            self.pending = 0;
            self.finished = true;
            self.output.mark_end(true);
        }
        self.maybe_start();
        Ok(())
    }

    fn produce(&mut self) -> Result<()> {
        if self.cursor >= self.queue.len() {
            self.finished = true;
            self.output.mark_end(true);
            return Ok(());
        }
        if self.decoder.is_none() {
            let entry = &mut self.queue[self.cursor];
            let mut decoder = Decoder::open(&entry.path, self.mode).with_context(|| {
                format!("opening queue entry {}: {}", entry.id, entry.path.display())
            })?;
            entry.metadata = Some(Self::source_metadata(&mut decoder, entry.id, self.mode));
            self.gains.push(decoder.gain_db());
            self.decoder = Some(decoder);
            self.decoded_position = 0;
        }
        if self.pending == 0 {
            self.pending = self.decoder.as_mut().unwrap().read(&mut self.block[..])?;
            if self.pending == 0 {
                self.decoder = None;
                self.cursor += 1;
                self.decoded_position = 0;
                return Ok(());
            }
        }
        if self.output.try_submit(
            &self.queue[self.cursor].token,
            self.decoded_position,
            &self.block[..self.pending],
        )? {
            if !self.started {
                self.prefilled_frames += self.pending;
                self.queue[self.cursor].prefilled_frames += self.pending;
            }
            self.decoded_position += self.pending as u64;
            self.pending = 0;
        }
        Ok(())
    }

    fn maybe_start(&mut self) {
        if !self.playing || self.stopped {
            return;
        }
        if self.started || self.finished || self.prefilled_frames >= PREBUFFER_FRAMES {
            self.output.set_paused(false);
            self.started = true;
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let output = self.output.snapshot();
        // A callback already in flight may publish old PCM after stop/selection.
        // Only the currently valid token can make that publication authoritative.
        let consumed = self
            .queue
            .iter()
            .enumerate()
            .find(|(_, entry)| {
                entry.id == output.entry_id && entry.token.consumed_by(output.rendered_frames)
            })
            .map(|(index, entry)| (index, entry, output.position_frames));
        let selected = if self.stopped {
            None
        } else {
            consumed.or_else(|| {
                let (id, position) = self.selection?;
                self.queue
                    .iter()
                    .enumerate()
                    .find(|(_, entry)| {
                        entry.id == id && !entry.token.consumed_by(output.rendered_frames)
                    })
                    .map(|(index, entry)| (index, entry, position))
            })
        };
        let current_id = selected.map(|(_, entry, _)| entry.id);
        let position = selected.map_or(0, |(_, _, position)| position);
        let next_id = selected
            .and_then(|(index, _, _)| self.queue.get(index + 1))
            .map(|entry| entry.id);
        let previous_id = selected.map(|(index, entry, position)| {
            if position >= u64::from(SAMPLE_RATE) * 3 || index == 0 {
                entry.id
            } else {
                self.queue[index - 1].id
            }
        });
        let now_playing = selected.and_then(|(_, entry, _)| entry.metadata.clone());
        let prepared_id = self
            .queue
            .get(self.cursor)
            .filter(|entry| Some(entry.id) != current_id)
            .filter(|_| self.decoder.is_some())
            .map(|entry| entry.id);
        Snapshot {
            output,
            queue_revision: self.revision,
            current_id,
            position_frames: position,
            next_id,
            previous_id,
            now_playing,
            prepared_id,
            decoding_finished: self.finished,
            stopped: self.stopped,
            playing: self.playing,
        }
    }

    pub fn completed(&self) -> bool {
        self.finished && self.output.snapshot().drained
    }
    pub fn error(&self) -> Option<&str> {
        self.failure.as_deref()
    }
    pub fn gains(&self) -> &[f64] {
        &self.gains
    }
    pub fn started_entries(&self) -> &[u64] {
        &self.starts
    }

    /// The persistent owner clears these after collecting each pump's state;
    /// probes may retain the complete run for numerical/transition diagnostics.
    pub fn clear_diagnostics(&mut self) {
        self.gains.clear();
        self.starts.clear();
    }

    pub fn queue_entries(&self) -> impl ExactSizeIterator<Item = (u64, &Path)> {
        self.queue
            .iter()
            .map(|entry| (entry.id, entry.path.as_path()))
    }

    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Change the mode while stopped/empty, without opening or replacing audio.
    pub fn set_idle_gain(&mut self, mode: GainMode) {
        self.mode = mode;
    }
    pub fn queue_ids(&self) -> Vec<u64> {
        self.queue.iter().map(|entry| entry.id).collect()
    }

    fn check_revision(&self, expected: u64) -> Result<()> {
        ensure!(
            expected == self.revision,
            "stale queue revision: expected {expected}, current {}",
            self.revision
        );
        Ok(())
    }

    pub fn append(&mut self, path: PathBuf, expected_revision: u64) -> Result<u64> {
        self.check_revision(expected_revision)?;
        let revision = self
            .revision
            .checked_add(1)
            .context("queue revision exhausted")?;
        let id = self.next_id;
        let next_id = self.next_id.checked_add(1).context("queue ID exhausted")?;
        self.queue.push(Entry {
            id,
            path,
            token: TrackToken::new(id),
            prefilled_frames: 0,
            metadata: None,
            run: Some(RunInfo {
                run_id: id,
                scope: RepeatScope::Queue,
            }),
        });
        self.next_id = next_id;
        self.revision = revision;
        if self.failure.is_none() {
            self.finished = false;
            self.output.mark_end(false);
        }
        Ok(id)
    }

    pub(crate) fn prepare_append(
        &mut self,
        paths: impl ExactSizeIterator<Item = PathBuf>,
        expected_revision: u64,
        play: bool,
        scope: RepeatScope,
    ) -> Result<PreparedAppend> {
        self.check_revision(expected_revision)?;
        ensure!(paths.len() > 0, "cannot append an empty playlist");
        let revision = self
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= i64::MAX as u64)
            .context("queue revision exhausted")?;
        let next_id = self
            .next_id
            .checked_add(paths.len() as u64)
            .filter(|id| *id <= i64::MAX as u64)
            .context("queue ID exhausted")?;
        self.queue.try_reserve(paths.len())?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(paths.len())?;
        for (index, path) in paths.enumerate() {
            let id = self.next_id + index as u64;
            entries.push(Entry {
                id,
                path,
                token: TrackToken::new(id),
                prefilled_frames: 0,
                metadata: None,
                run: Some(RunInfo {
                    run_id: self.next_id,
                    scope,
                }),
            });
        }
        let (selection, replacement_tokens) = if play {
            let first = &entries[0];
            let mut decoder = Decoder::open(&first.path, self.mode).with_context(|| {
                format!("opening queue entry {}: {}", first.id, first.path.display())
            })?;
            let metadata = Self::source_metadata(&mut decoder, first.id, self.mode);
            let mut tokens = Vec::new();
            tokens.try_reserve_exact(self.queue.len())?;
            tokens.extend(self.queue.iter().map(|entry| TrackToken::new(entry.id)));
            (
                Some(PreparedSelection {
                    revision,
                    index: self.queue.len(),
                    id: first.id,
                    frame: 0,
                    mode: self.mode,
                    decoder,
                    metadata,
                }),
                tokens,
            )
        } else {
            (None, Vec::new())
        };
        Ok(PreparedAppend {
            revision,
            next_id,
            entries,
            selection,
            replacement_tokens,
        })
    }

    /// No allocation, reopening, or fallible work after the durable transaction.
    /// Plain enqueue leaves existing PCM/tokens and play intent untouched.
    pub(crate) fn commit_append(&mut self, prepared: PreparedAppend) {
        assert_eq!(self.revision + 1, prepared.revision);
        assert_eq!(self.next_id, prepared.first_id());
        if prepared.selection.is_some() {
            self.output.set_paused(true);
            for (entry, token) in self.queue.iter_mut().zip(prepared.replacement_tokens) {
                entry.token.invalidate();
                entry.token = token;
                entry.prefilled_frames = 0;
                entry.metadata = None;
            }
        }
        self.queue.extend(prepared.entries);
        self.next_id = prepared.next_id;
        self.revision = prepared.revision;
        if let Some(selection) = prepared.selection {
            self.started = false;
            self.prefilled_frames = 0;
            self.queue[selection.index].metadata = Some(selection.metadata);
            self.decoder = Some(selection.decoder);
            self.cursor = selection.index;
            self.pending = 0;
            self.decoded_position = 0;
            self.finished = false;
            self.failure = None;
            self.stopped = false;
            self.playing = true;
            self.selection = Some((selection.id, 0));
            self.output.mark_end(false);
        } else if self.failure.is_none() {
            self.finished = false;
            self.output.mark_end(false);
        }
    }

    /// Removing the active entry stops playback. Future edits invalidate all
    /// prepared future tokens, leaving the current entry's PCM untouched.
    pub fn remove(&mut self, id: u64, expected_revision: u64) -> Result<()> {
        self.check_revision(expected_revision)?;
        let index = self
            .queue
            .iter()
            .position(|entry| entry.id == id)
            .context("queue entry not found")?;
        let revision = self
            .revision
            .checked_add(1)
            .context("queue revision exhausted")?;
        let current = self.snapshot().current_id;
        let decoding = self.queue.get(self.cursor).map(|entry| entry.id);
        self.queue[index].token.invalidate();
        self.prefilled_frames -= self.queue[index].prefilled_frames;
        self.queue.remove(index);
        self.revision = revision;
        if self.selection.is_some_and(|(selected, _)| selected == id) {
            self.selection = None;
        }
        if current == Some(id) {
            self.stop();
        } else {
            self.rebuild_future(current, decoding);
        }
        Ok(())
    }

    pub(crate) fn prepare_move(
        &self,
        ids: &[u64],
        before: Option<u64>,
        expected_revision: u64,
    ) -> Result<Option<PreparedMove>> {
        self.check_revision(expected_revision)?;
        ensure!(
            !ids.is_empty() && ids.len() <= 500,
            "move requires 1..=500 occurrences"
        );
        let selected: HashSet<_> = ids.iter().copied().collect();
        ensure!(
            selected.len() == ids.len(),
            "move occurrences must be distinct"
        );
        let mut found = 0;
        let mut anchor = None;
        for (index, entry) in self.queue.iter().enumerate() {
            found += usize::from(selected.contains(&entry.id));
            if Some(entry.id) == before {
                anchor = Some(index);
            }
        }
        ensure!(found == ids.len(), "queue entry not found");
        ensure!(
            before.is_none() || anchor.is_some(),
            "destination entry not found"
        );
        if before.is_some_and(|id| selected.contains(&id)) {
            return Ok(None);
        }
        let insertion = anchor.unwrap_or(self.queue.len());
        let mut destinations = vec![0; self.queue.len()];
        let mut next = 0;
        for (index, entry) in self.queue.iter().enumerate().take(insertion) {
            if !selected.contains(&entry.id) {
                destinations[index] = next;
                next += 1;
            }
        }
        for (index, entry) in self.queue.iter().enumerate() {
            if selected.contains(&entry.id) {
                destinations[index] = next;
                next += 1;
            }
        }
        for (index, entry) in self.queue.iter().enumerate().skip(insertion) {
            if !selected.contains(&entry.id) {
                destinations[index] = next;
                next += 1;
            }
        }
        if destinations
            .iter()
            .enumerate()
            .all(|(index, destination)| index == *destination)
        {
            return Ok(None);
        }
        let revision = self
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= i64::MAX as u64)
            .context("queue revision exhausted")?;
        let tokens = self
            .queue
            .iter()
            .map(|entry| TrackToken::new(entry.id))
            .collect();
        Ok(Some(PreparedMove {
            revision,
            destinations,
            tokens,
        }))
    }

    pub(crate) fn commit_move(&mut self, mut prepared: PreparedMove) {
        assert_eq!(self.revision + 1, prepared.revision);
        let current = self.snapshot().current_id;
        let decoding = self.queue.get(self.cursor).map(|entry| entry.id);
        self.replace_future_tokens(current, prepared.tokens);
        for index in 0..prepared.destinations.len() {
            while prepared.destinations[index] != index {
                let destination = prepared.destinations[index];
                self.queue.swap(index, destination);
                prepared.destinations.swap(index, destination);
            }
        }
        self.revision = prepared.revision;
        self.relocate_decoder(current, decoding);
    }

    pub fn move_before(
        &mut self,
        id: u64,
        before: Option<u64>,
        expected_revision: u64,
    ) -> Result<()> {
        if let Some(prepared) = self.prepare_move(&[id], before, expected_revision)? {
            self.commit_move(prepared);
        }
        Ok(())
    }

    pub(crate) fn prepare_insert(
        &mut self,
        paths: impl ExactSizeIterator<Item = PathBuf>,
        before: Option<u64>,
        expected_revision: u64,
    ) -> Result<PreparedInsert> {
        self.check_revision(expected_revision)?;
        ensure!(
            paths.len() > 0 && paths.len() <= 500,
            "insert requires 1..=500 tracks"
        );
        let destination = match before {
            Some(id) => self
                .queue
                .iter()
                .position(|entry| entry.id == id)
                .context("destination entry not found")?,
            None => self.queue.len(),
        };
        let append = self.prepare_append(paths, expected_revision, false, RepeatScope::Queue)?;
        let tokens = self
            .queue
            .iter()
            .map(|entry| TrackToken::new(entry.id))
            .collect();
        Ok(PreparedInsert {
            append,
            destination,
            tokens,
        })
    }

    pub(crate) fn commit_insert(&mut self, prepared: PreparedInsert) {
        let current = self.snapshot().current_id;
        let decoding = self.queue.get(self.cursor).map(|entry| entry.id);
        let count = prepared.append.entries.len();
        self.replace_future_tokens(current, prepared.tokens);
        self.commit_append(prepared.append);
        self.queue[prepared.destination..].rotate_right(count);
        self.relocate_decoder(current, decoding);
    }

    pub fn clear(&mut self, expected_revision: u64) -> Result<()> {
        self.check_revision(expected_revision)?;
        let revision = self
            .revision
            .checked_add(1)
            .context("queue revision exhausted")?;
        self.output.set_paused(true);
        for entry in self.queue.drain(..) {
            entry.token.invalidate();
        }
        self.stop();
        self.revision = revision;
        Ok(())
    }

    fn rebuild_future(&mut self, current: Option<u64>, decoding: Option<u64>) {
        for entry in &mut self.queue {
            if Some(entry.id) != current {
                entry.token.invalidate();
                entry.token = TrackToken::new(entry.id);
                self.prefilled_frames -= entry.prefilled_frames;
                entry.prefilled_frames = 0;
                entry.metadata = None;
            }
        }
        self.relocate_decoder(current, decoding);
    }

    fn replace_future_tokens(&mut self, current: Option<u64>, tokens: Vec<Arc<TrackToken>>) {
        for (entry, token) in self.queue.iter_mut().zip(tokens) {
            if Some(entry.id) != current {
                entry.token.invalidate();
                entry.token = token;
                self.prefilled_frames -= entry.prefilled_frames;
                entry.prefilled_frames = 0;
                entry.metadata = None;
            }
        }
    }

    fn relocate_decoder(&mut self, current: Option<u64>, decoding: Option<u64>) {
        let current_index =
            current.and_then(|id| self.queue.iter().position(|entry| entry.id == id));
        if current.is_some() && decoding == current && self.decoder.is_some() {
            self.cursor = current_index.unwrap();
        } else {
            self.decoder = None;
            self.pending = 0;
            self.decoded_position = 0;
            self.cursor = current_index.map_or(0, |index| index + 1);
        }
        self.finished = false;
        self.failure = None;
        self.output.mark_end(false);
    }

    fn source_metadata(decoder: &mut Decoder, entry_id: u64, mode: GainMode) -> Arc<NowPlaying> {
        let metadata = decoder.take_metadata();
        Arc::new(NowPlaying {
            entry_id,
            title: metadata.title,
            artist: metadata.artist,
            album: metadata.album,
            duration_frames: decoder.duration_frames(),
            metadata_error: metadata.metadata_error,
            track_id: None,
            album_id: None,
            format: String::new(),
            gain_db: (mode != GainMode::Off).then(|| decoder.gain_db()),
        })
    }

    /// Open/seek in isolation so persistence can commit before any audible edit.
    pub fn prepare_selection(
        &self,
        entry_id: u64,
        frame: u64,
        mode: GainMode,
    ) -> Result<PreparedSelection> {
        let index = self
            .queue
            .iter()
            .position(|entry| entry.id == entry_id)
            .context("queue entry not found")?;
        let entry = &self.queue[index];
        let mut decoder = Decoder::open(&entry.path, mode).with_context(|| {
            format!("opening queue entry {}: {}", entry.id, entry.path.display())
        })?;
        if frame != 0 {
            if let Some(duration) = decoder.duration_frames() {
                ensure!(frame < duration, "seek target exceeds track duration");
            }
            decoder.seek(frame)?;
        }
        let metadata = Self::source_metadata(&mut decoder, entry_id, mode);
        Ok(PreparedSelection {
            revision: self.revision,
            index,
            id: entry_id,
            frame,
            mode,
            decoder,
            metadata,
        })
    }

    /// Infallible after preparation on this player with no intervening queue
    /// mutation. Keeps play intent; a stopped/paused player remains paused.
    pub fn commit_selection(&mut self, prepared: PreparedSelection) {
        assert_eq!(
            self.revision, prepared.revision,
            "queue changed after selection preparation"
        );
        assert_eq!(self.queue[prepared.index].id, prepared.id);
        self.output.set_paused(true);
        self.invalidate_buffer();
        self.queue[prepared.index].metadata = Some(prepared.metadata);
        self.decoder = Some(prepared.decoder);
        self.cursor = prepared.index;
        self.mode = prepared.mode;
        self.pending = 0;
        self.decoded_position = prepared.frame;
        self.finished = false;
        self.failure = None;
        self.stopped = false;
        self.selection = Some((prepared.id, prepared.frame));
        self.output.mark_end(false);
    }

    pub fn seek(&mut self, frame: u64) -> Result<()> {
        let id = self
            .snapshot()
            .current_id
            .context("no current entry to seek")?;
        let prepared = self.prepare_selection(id, frame, self.mode)?;
        self.commit_selection(prepared);
        Ok(())
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::{
        fs, thread,
        time::{Duration, Instant},
    };

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn source(directory: &Path, name: &str, frames: u32, sample: i16) -> PathBuf {
        let path = directory.join(name);
        let mut bytes = Vec::with_capacity(44 + frames as usize * 4);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&48_000_u32.to_le_bytes());
        bytes.extend_from_slice(&192_000_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(frames * 4).to_le_bytes());
        for _ in 0..frames * 2 {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn prepared_batch_edits_keep_current_pcm_and_replace_the_prepared_future() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-batch-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let current = source(&scratch.0, "current.wav", 48_000, 1000);
        let second = source(&scratch.0, "second.wav", 311, 2000);
        let third = source(&scratch.0, "third.wav", 137, 3000);
        let fourth = source(&scratch.0, "fourth.wav", 199, 4000);
        let inserted = source(&scratch.0, "inserted.wav", 223, 5000);
        let capture = scratch.0.join("capture.f32");
        let mut player = Player::new(
            vec![current, second, third, fourth],
            OutputConfig::Capture {
                path: capture.clone(),
                clocked: true,
            },
            GainMode::Off,
        )?;
        player.play();
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.snapshot().current_id != Some(1) || player.snapshot().prepared_id != Some(2) {
            assert!(
                Instant::now() < deadline,
                "next occurrence was not prepared"
            );
            player.pump()?;
            thread::sleep(Duration::from_millis(1));
        }
        player.pause();
        let before = player.snapshot();
        assert!(player.prepare_move(&[2, 999], Some(2), 1).is_err());
        assert!(player.prepare_move(&[2, 2], None, 1).is_err());
        assert!(player.prepare_move(&[2], Some(999), 1).is_err());
        assert!(player.prepare_move(&[2, 3], Some(3), 1)?.is_none());
        let abandoned = player.prepare_move(&[3, 2], None, 1)?.unwrap();
        drop(abandoned);
        let abandoned = player.prepare_insert(vec![inserted.clone()].into_iter(), Some(2), 1)?;
        drop(abandoned);
        assert_eq!(player.queue_ids(), [1, 2, 3, 4]);
        assert_eq!(player.snapshot().position_frames, before.position_frames);
        assert_eq!(player.snapshot().prepared_id, Some(2));
        let prepared = player.prepare_move(&[3, 2], None, 1)?.unwrap();
        player.commit_move(prepared);
        assert_eq!(player.queue_ids(), [1, 4, 2, 3]);
        assert_eq!(player.snapshot().current_id, Some(1));
        assert_eq!(player.snapshot().position_frames, before.position_frames);
        assert!(!player.snapshot().playing);
        let prepared =
            player.prepare_insert(vec![inserted.clone(), inserted].into_iter(), Some(4), 2)?;
        assert_eq!(prepared.first_id(), 5);
        player.commit_insert(prepared);
        assert_eq!(player.queue_ids(), [1, 5, 6, 4, 2, 3]);
        assert_eq!(player.snapshot().position_frames, before.position_frames);
        assert_eq!(player.snapshot().current_id, Some(1));
        assert_eq!(player.snapshot().queue_revision, 3);
        player.play();
        while !player.completed() {
            assert!(Instant::now() < deadline, "edited playback did not finish");
            player.pump()?;
            thread::sleep(Duration::from_millis(1));
        }
        player.pump()?;
        assert_eq!(player.started_entries(), &[1, 5, 6, 4, 2, 3]);
        drop(player);
        let pcm = fs::read(capture)?;
        assert_eq!(pcm.len(), (48_000 + 223 * 2 + 199 + 311 + 137) * 8);
        let mut frames = pcm.as_chunks::<8>().0.iter();
        for (count, sample) in [
            (48_000, 1000),
            (446, 5000),
            (199, 4000),
            (311, 2000),
            (137, 3000),
        ] {
            for _ in 0..count {
                let frame = frames.next().unwrap();
                assert_eq!(
                    f32::from_le_bytes(frame[..4].try_into().unwrap()),
                    sample as f32 / 32768.0
                );
                assert_eq!(
                    f32::from_le_bytes(frame[4..].try_into().unwrap()),
                    sample as f32 / 32768.0
                );
            }
        }
        Ok(())
    }
}
