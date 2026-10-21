//! Single-owner playback state. Drive `pump` on a worker, never the UI/audio thread.
//! Decoding may perform disk I/O; output consumption is independent and bounded.
use crate::{
    BLOCK_FRAMES, Frame, SAMPLE_RATE,
    decoder::{Decoder, GainMode},
    equalizer::{Equalizer, EqualizerSettings},
    output::{Output, OutputConfig, OutputSnapshot, TrackToken},
    play_order::{PlayOrder, RepeatMode, RepeatScope, RunInfo, ShuffleStrategy},
    protocol::{NowPlaying, Placement, StreamSpec},
    store::LoadedEntry,
    stream::StreamDecoder,
};
use anyhow::{Context, Result, ensure};
use order::{Pass, Plan};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

mod order;
mod radio;
pub use order::CountedPlay;

const PREBUFFER_FRAMES: usize = SAMPLE_RATE as usize / 2;

/// Local reads retain their synchronous fast path; live reads only poll a bounded worker.
struct SourceDecoder {
    // Exactly one slot is populated. Keep both in-place rather than adding a
    // heap allocation to every local track just to shrink an enum variant.
    file: Option<Decoder>,
    stream: Option<StreamDecoder>,
    initial: Option<crate::metadata::PlaybackMetadata>,
}

impl SourceDecoder {
    fn open(entry: &Entry, mode: GainMode) -> Result<Self> {
        match &entry.stream {
            Some(spec) => Ok(Self {
                file: None,
                stream: Some(StreamDecoder::open(spec, mode)?),
                initial: Some(crate::metadata::PlaybackMetadata {
                    title: spec.title.clone(),
                    artist: String::new(),
                    album: String::new(),
                    metadata_error: None,
                }),
            }),
            None => Ok(Self {
                file: Some(Decoder::open(&entry.path, mode)?),
                stream: None,
                initial: None,
            }),
        }
    }

    fn read(&mut self, destination: &mut [Frame]) -> Result<Option<(usize, bool)>> {
        match &mut self.file {
            Some(decoder) => decoder.read(destination).map(|count| Some((count, false))),
            None => self
                .stream
                .as_mut()
                .expect("stream source")
                .poll(destination),
        }
    }

    fn take_metadata(&mut self) -> crate::metadata::PlaybackMetadata {
        match &mut self.file {
            Some(decoder) => decoder.take_metadata(),
            None => self
                .stream
                .as_mut()
                .expect("stream source")
                .take_metadata()
                .or_else(|| self.initial.take())
                .expect("new stream has initial display metadata"),
        }
    }

    fn take_update(&mut self) -> Option<crate::metadata::PlaybackMetadata> {
        self.stream.as_mut().and_then(StreamDecoder::take_metadata)
    }

    fn duration_frames(&self) -> Option<u64> {
        self.file.as_ref().and_then(Decoder::duration_frames)
    }

    fn gain_db(&self) -> f64 {
        match &self.file {
            Some(decoder) => decoder.gain_db(),
            None => self.stream.as_ref().expect("stream source").gain_db(),
        }
    }

    fn seek(&mut self, frame: u64) -> Result<()> {
        match &mut self.file {
            Some(decoder) => decoder.seek(frame),
            None => anyhow::bail!("live streams cannot seek"),
        }
    }
}

struct Entry {
    id: u64,
    path: PathBuf,
    stream: Option<StreamSpec>,
    /// The token of the entry's latest pass, or a fresh one.
    token: Arc<TrackToken>,
    metadata: Option<Arc<NowPlaying>>,
    /// The run the entry repeats within under Repeat All.
    run: Option<RunInfo>,
    /// The entry has been current. Upcoming edits with nothing current keep it.
    /// Kept in step with the store's `played_seq`.
    played: bool,
    /// Played in the current pass over its context, as the play order sees it.
    /// Output hearing the entry sets it; a fresh Repeat All pass clears it.
    visited: bool,
}

/// A validated decoder replacement. Commit on the same player without an
/// intervening queue mutation; output consumption may continue in the meantime.
pub struct PreparedSelection {
    revision: u64,
    index: usize,
    id: u64,
    frame: u64,
    mode: GainMode,
    decoder: SourceDecoder,
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

/// A reorder of the upcoming entries, relative to the entry current at preparation.
pub(crate) struct PreparedReorder {
    change: PreparedMove,
    /// The entry current at preparation. It keeps its PCM, even if output moves
    /// on into a reordered entry before the commit.
    anchor: Option<u64>,
    order: Vec<u64>,
}
impl PreparedReorder {
    /// The reordered entries in their new order.
    pub fn order(&self) -> &[u64] {
        &self.order
    }
}

pub(crate) struct PreparedInsert {
    append: PreparedAppend,
    destination: usize,
    tokens: Vec<Arc<TrackToken>>,
}
/// An enqueue prepared without touching audible state: the entries it removes,
/// the new entries and where they go, and with `play` the first one opened.
pub(crate) struct PreparedReplace {
    append: PreparedAppend,
    /// Index among the new entries of the one played first.
    start: usize,
    /// The entry current at preparation. The edit is relative to it, even if
    /// output moves on before the commit.
    anchor: Option<u64>,
    /// Per queue index: whether this edit removes the entry.
    removed: Vec<bool>,
    removed_ids: Vec<u64>,
    /// Index of the first new entry once the removed entries are gone.
    destination: usize,
    before: Option<u64>,
    /// Fresh tokens for the kept entries, in order. Empty when the edit only
    /// appends after everything, which leaves all prepared PCM valid.
    tokens: Vec<Arc<TrackToken>>,
}
impl PreparedReplace {
    pub fn entries(&self) -> impl ExactSizeIterator<Item = (u64, &Path)> {
        self.append.entries()
    }
    pub fn first_id(&self) -> u64 {
        self.append.first_id()
    }
    /// The new entry played first, with `play`.
    pub fn start_id(&self) -> u64 {
        self.append.entries[self.start].id
    }
    /// Entries removed before the new ones are inserted, in queue order.
    pub fn removed(&self) -> &[u64] {
        &self.removed_ids
    }
    /// The kept entry the new entries go before; `None` puts them last.
    pub fn before(&self) -> Option<u64> {
        self.before
    }
}

/// Where an enqueue that plays starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// The first new entry.
    First,
    /// Under shuffle, a shuffle pick among the new entries; else the first.
    Pick,
}

/// A batch removal prepared without touching audible state.
pub(crate) struct PreparedRemove {
    revision: u64,
    /// The entry current at preparation. The edit is relative to it.
    anchor: Option<u64>,
    /// Per queue index: whether this edit removes the entry.
    removed: Vec<bool>,
    removed_ids: Vec<u64>,
    /// Fresh tokens for the kept entries, in order. Empty when every removed
    /// entry is before the anchor, which leaves all prepared PCM valid.
    tokens: Vec<Arc<TrackToken>>,
}
impl PreparedRemove {
    /// The removed entries, in queue order.
    pub fn removed(&self) -> &[u64] {
        &self.removed_ids
    }
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
    equalizer: Equalizer,
    cursor: usize,
    decoder: Option<SourceDecoder>,
    stream_buffering: bool,
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
    order: PlayOrder,
    repeat: RepeatMode,
    shuffled: bool,
    /// Passes whose PCM may still be in the output, oldest first. While a
    /// decoder is open, the last is the pass it decodes.
    passes: VecDeque<Pass>,
    /// What follows the pass being decoded.
    plan: Plan,
    /// With no decoder open: the entries the cursor's next pass starts a fresh
    /// Repeat All pass over.
    opening: Option<Vec<u64>>,
    /// Passes that ended with no PCM since one with PCM; bounds an empty loop.
    silent: usize,
    /// Each pass output started: its entry, and the entries its fresh Repeat
    /// All pass clears.
    advances: Vec<(u64, Vec<u64>)>,
    /// The entry last recorded in the play order's history.
    last_heard: Option<u64>,
    /// The entry of each pass that counted as a play, not yet taken.
    counted_plays: Vec<CountedPlay>,
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
                    stream: None,
                    token: TrackToken::new(id),
                    metadata: None,
                    run: None,
                    played: false,
                    visited: false,
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
                stream: None,
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
                stream: entry.stream,
                token: TrackToken::new(entry.id),
                metadata: None,
                run: entry.run,
                played: entry.played_seq.is_some(),
                visited: entry.played_seq.is_some(),
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
            equalizer: Equalizer::default(),
            cursor: 0,
            decoder: None,
            stream_buffering: false,
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
            order: PlayOrder::new(RepeatMode::Off, None),
            repeat: RepeatMode::Off,
            shuffled: false,
            passes: VecDeque::new(),
            plan: Plan::default(),
            opening: None,
            silent: 0,
            advances: Vec::new(),
            last_heard: None,
            counted_plays: Vec::new(),
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

    /// Output volume, `0..=100` percent.
    pub fn set_volume(&mut self, percent: u8) {
        self.output.set_volume(percent);
    }

    /// Apply validated EQ to newly decoded PCM, retaining the pending block and
    /// output pool. At 48 kHz those buffers can delay the change by about 1.4 s,
    /// plus device buffering; when paused, that audio plays first on resume.
    pub fn set_equalizer(&mut self, settings: &EqualizerSettings) {
        self.equalizer.set_settings(settings);
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
        self.drop_passes();
        self.equalizer.reset();
        self.started = false;
        self.stream_buffering = false;
        self.prefilled_frames = 0;
        for entry in &mut self.queue {
            entry.token.invalidate();
            entry.token = TrackToken::new(entry.id);
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
        self.hear();
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
            self.equalizer.reset();
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
            let mut decoder = SourceDecoder::open(entry, self.mode).with_context(|| {
                format!("opening queue entry {}: {}", entry.id, entry.path.display())
            })?;
            entry.metadata = Some(Self::source_metadata(&mut decoder, entry.id, self.mode));
            self.gains.push(decoder.gain_db());
            self.decoder = Some(decoder);
            self.decoded_position = 0;
            self.begin_pass();
        }
        if self.pending == 0 {
            let decoder = self.decoder.as_mut().unwrap();
            let read = decoder.read(&mut self.block[..])?;
            if let Some(metadata) = decoder.take_update() {
                self.queue[self.cursor].metadata = Some(Self::display_metadata(
                    metadata,
                    decoder,
                    self.queue[self.cursor].id,
                    self.mode,
                ));
            }
            let Some((count, discontinuity)) = read else {
                // Rebuffer before a live source empties the ring. Keep the same
                // token/position: silence during a stall is not end-of-track.
                if self.output.snapshot().buffered_frames < BLOCK_FRAMES * 2 {
                    self.stream_buffering = true;
                    self.output.set_paused(true);
                }
                return Ok(());
            };
            self.pending = count;
            if self.pending == 0 {
                self.decoder = None;
                self.decoded_position = 0;
                self.end_pass();
                return Ok(());
            }
            if discontinuity {
                self.equalizer.reset();
            }
            // Both local and live PCM pass here once. A full output pool keeps
            // this processed block pending; enqueue retries must not filter it.
            self.equalizer.process(&mut self.block[..count]);
        }
        if self.output.try_submit(
            &self.queue[self.cursor].token,
            self.decoded_position,
            &self.block[..self.pending],
        )? {
            let pass = self.passes.back_mut().expect("an open decoder has a pass");
            pass.frames += self.pending;
            if !self.started {
                self.prefilled_frames += self.pending;
                pass.prefilled += self.pending;
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
        if self.stream_buffering {
            if !self.finished && self.output.snapshot().buffered_frames < PREBUFFER_FRAMES {
                return;
            }
            self.stream_buffering = false;
        }
        if self.started || self.finished || self.prefilled_frames >= PREBUFFER_FRAMES {
            self.output.set_paused(false);
            self.started = true;
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let output = self.output.snapshot();
        let selected = self
            .current_pass(&output)
            .map(|(index, pass, position)| (index, &self.queue[index], pass, position));
        let current_id = selected.map(|(_, entry, _, _)| entry.id);
        let position = selected.map_or(0, |(_, _, _, position)| position);
        let next_id = selected.and_then(|(_, _, pass, _)| self.following(pass?).0);
        let previous_id = selected.map(|(index, entry, _, position)| {
            if position >= u64::from(SAMPLE_RATE) * 3 {
                return entry.id;
            }
            self.previous_index(index)
                .map_or(entry.id, |previous| self.queue[previous].id)
        });
        let now_playing = selected.and_then(|(_, entry, _, _)| entry.metadata.clone());
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
        self.advances.clear();
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
            stream: None,
            token: TrackToken::new(id),
            metadata: None,
            run: Some(RunInfo {
                run_id: id,
                scope: RepeatScope::Queue,
            }),
            played: false,
            visited: false,
        });
        self.next_id = next_id;
        self.revision = revision;
        self.resume_after_append();
        Ok(id)
    }

    /// Appends `paths` as one run with `scope`, as a run enqueue does, and
    /// returns the first new entry.
    pub fn append_run(
        &mut self,
        paths: Vec<PathBuf>,
        expected_revision: u64,
        scope: RepeatScope,
    ) -> Result<u64> {
        let prepared = self.prepare_append(paths.into_iter(), expected_revision, false, scope)?;
        let first = prepared.first_id();
        self.commit_append(prepared);
        Ok(first)
    }

    /// New entries follow the queue. Decoding resumes if it had ended, and
    /// the next entry is decided again.
    fn resume_after_append(&mut self) {
        if self.completed() {
            // Appending before output drains remains gapless; appending after
            // it drains starts a new audible sequence, not an old filter tail.
            self.equalizer.reset();
        }
        if self.failure.is_none() {
            self.finished = false;
            self.output.mark_end(false);
        }
        self.replan();
    }

    pub(crate) fn is_stream(&self, entry_id: u64) -> bool {
        self.stream_title(entry_id).is_some()
    }

    /// The saved station title of a live stream entry.
    pub(crate) fn stream_title(&self, entry_id: u64) -> Option<&str> {
        self.queue
            .iter()
            .find(|entry| entry.id == entry_id)
            .and_then(|entry| entry.stream.as_ref())
            .map(|stream| stream.title.as_str())
    }

    /// No network work during the durable append preparation.
    pub(crate) fn prepare_stream(
        &mut self,
        stream: StreamSpec,
        expected_revision: u64,
    ) -> Result<PreparedAppend> {
        let mut prepared = self.prepare_append(
            std::iter::once(PathBuf::from(&stream.url)),
            expected_revision,
            false,
            RepeatScope::Queue,
        )?;
        prepared.entries[0].stream = Some(stream);
        Ok(prepared)
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
                stream: None,
                token: TrackToken::new(id),
                metadata: None,
                run: Some(RunInfo {
                    run_id: self.next_id,
                    scope,
                }),
                played: false,
                visited: false,
            });
        }
        let (selection, replacement_tokens) = if play {
            let selection = self.prepare_first(&entries[0], revision, self.queue.len())?;
            let mut tokens = Vec::new();
            tokens.try_reserve_exact(self.queue.len())?;
            tokens.extend(self.queue.iter().map(|entry| TrackToken::new(entry.id)));
            (Some(selection), tokens)
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

    /// Opens a new entry from frame 0, to be selected at `index` once committed.
    fn prepare_first(
        &self,
        first: &Entry,
        revision: u64,
        index: usize,
    ) -> Result<PreparedSelection> {
        let mut decoder = SourceDecoder::open(first, self.mode).with_context(|| {
            format!("opening queue entry {}: {}", first.id, first.path.display())
        })?;
        let metadata = Self::source_metadata(&mut decoder, first.id, self.mode);
        Ok(PreparedSelection {
            revision,
            index,
            id: first.id,
            frame: 0,
            mode: self.mode,
            decoder,
            metadata,
        })
    }

    /// Plays a selection prepared by `prepare_first`. Every token was already
    /// replaced, so no old PCM can play.
    fn adopt_first(&mut self, selection: PreparedSelection) {
        self.equalizer.reset();
        self.started = false;
        self.stream_buffering = false;
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
        self.begin_pass();
    }

    /// No reopening or fallible work after the durable transaction. Plain
    /// enqueue leaves existing PCM/tokens and play intent untouched.
    pub(crate) fn commit_append(&mut self, prepared: PreparedAppend) {
        if let Some(selection) = self.extend(prepared) {
            self.adopt_first(selection);
        } else {
            self.resume_after_append();
        }
    }

    /// Adds the prepared entries last. With a selection, every old token and
    /// pass is invalidated first, and the selection is returned to adopt.
    fn extend(&mut self, prepared: PreparedAppend) -> Option<PreparedSelection> {
        assert_eq!(self.revision + 1, prepared.revision);
        assert_eq!(self.next_id, prepared.first_id());
        if prepared.selection.is_some() {
            self.output.set_paused(true);
            self.drop_passes();
            for (entry, token) in self.queue.iter_mut().zip(prepared.replacement_tokens) {
                entry.token.invalidate();
                entry.token = token;
                entry.metadata = None;
            }
        }
        self.queue.extend(prepared.entries);
        self.next_id = prepared.next_id;
        self.revision = prepared.revision;
        prepared.selection
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
        self.queue[index].token.invalidate();
        self.forget_passes(&[id]);
        self.queue.remove(index);
        self.revision = revision;
        if self.selection.is_some_and(|(selected, _)| selected == id) {
            self.selection = None;
        }
        if current == Some(id) {
            self.stop();
        } else {
            self.rebuild_future(current);
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

    pub(crate) fn commit_move(&mut self, prepared: PreparedMove) {
        let current = self.snapshot().current_id;
        self.apply_move(prepared, current);
    }

    /// Permutes the queue. `current` keeps its PCM; every other entry gets a
    /// fresh token, and decoding resumes after `current`.
    fn apply_move(&mut self, mut prepared: PreparedMove, current: Option<u64>) {
        assert_eq!(self.revision + 1, prepared.revision);
        self.replace_future_tokens(current, prepared.tokens);
        for index in 0..prepared.destinations.len() {
            while prepared.destinations[index] != index {
                let destination = prepared.destinations[index];
                self.queue.swap(index, destination);
                prepared.destinations.swap(index, destination);
            }
        }
        self.revision = prepared.revision;
        self.relocate_decoder(current);
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
        let count = prepared.append.entries.len();
        self.replace_future_tokens(current, prepared.tokens);
        let selection = self.extend(prepared.append);
        debug_assert!(selection.is_none(), "an insert never plays");
        self.queue[prepared.destination..].rotate_right(count);
        self.relocate_decoder(current);
    }

    /// Whether the entry at `index` is upcoming, given the current entry's
    /// index. Shuffle off: after the current entry in queue order. Shuffle on:
    /// unplayed and not current. Nothing current: unplayed.
    fn is_upcoming(&self, current: Option<usize>, index: usize, entry: &Entry) -> bool {
        match current {
            Some(current) if !self.shuffled => index > current,
            Some(current) => index != current && !entry.played,
            None => !entry.played,
        }
    }

    /// The current entry, and per queue index whether the entry is upcoming
    /// (see `is_upcoming`).
    fn upcoming(&self) -> Result<(Option<u64>, Option<usize>, Vec<bool>)> {
        let anchor = self.snapshot().current_id;
        let current = anchor.and_then(|id| self.queue.iter().position(|entry| entry.id == id));
        let mut upcoming = Vec::new();
        upcoming.try_reserve_exact(self.queue.len())?;
        upcoming.extend(
            self.queue
                .iter()
                .enumerate()
                .map(|(index, entry)| self.is_upcoming(current, index, entry)),
        );
        Ok((anchor, current, upcoming))
    }

    /// The queue index of `current`, the entry the snapshot names current, and
    /// the number of upcoming entries (see `upcoming`). Allocates nothing.
    pub(crate) fn upcoming_summary(&self, current: Option<u64>) -> (Option<usize>, usize) {
        let index = current.and_then(|id| self.queue.iter().position(|entry| entry.id == id));
        let count = self
            .queue
            .iter()
            .enumerate()
            .filter(|(position, entry)| self.is_upcoming(index, *position, entry))
            .count();
        (index, count)
    }

    /// The upcoming entries in queue order (see `upcoming`).
    pub(crate) fn upcoming_ids(&self) -> Result<Vec<u64>> {
        let (_, _, upcoming) = self.upcoming()?;
        let mut ids = Vec::new();
        ids.try_reserve_exact(upcoming.iter().filter(|next| **next).count())?;
        ids.extend(
            self.queue
                .iter()
                .zip(&upcoming)
                .filter(|(_, next)| **next)
                .map(|(entry, _)| entry.id),
        );
        Ok(ids)
    }

    /// Prepares one enqueue: `placement` decides what is removed and where the
    /// new entries go, relative to the current entry. With `play` the new entry
    /// it names is opened, as selecting it would, and is played; `trim` names
    /// played entries the same edit removes.
    pub(crate) fn prepare_replace_upcoming(
        &mut self,
        paths: impl ExactSizeIterator<Item = PathBuf>,
        placement: Placement,
        expected_revision: u64,
        play: Option<Start>,
        scope: RepeatScope,
        trim: &[u64],
    ) -> Result<PreparedReplace> {
        self.check_revision(expected_revision)?;
        let (anchor, current, mut removed) = self.upcoming()?;
        if placement != Placement::ReplaceUpcoming {
            removed.fill(false);
        }
        let trim: HashSet<u64> = trim.iter().copied().collect();
        let mut trimmed = 0;
        for (gone, entry) in removed.iter_mut().zip(&self.queue) {
            if trim.contains(&entry.id) {
                trimmed += 1;
                *gone = true;
            }
        }
        ensure!(trimmed == trim.len(), "trimmed queue entry not found");
        let mut removed_ids = Vec::new();
        removed_ids.try_reserve_exact(removed.iter().filter(|gone| **gone).count())?;
        removed_ids.extend(
            self.queue
                .iter()
                .zip(&removed)
                .filter(|(_, gone)| **gone)
                .map(|(entry, _)| entry.id),
        );
        let kept = self.queue.len() - removed_ids.len();
        let destination = match (placement, current) {
            // Straight after the current entry, among the kept entries.
            (Placement::Next, Some(current)) => {
                removed[..=current].iter().filter(|gone| !**gone).count()
            }
            _ => kept,
        };
        let before = self
            .queue
            .iter()
            .zip(&removed)
            .filter(|(_, gone)| !**gone)
            .nth(destination)
            .map(|(entry, _)| entry.id);
        let mut append = self.prepare_append(paths, expected_revision, false, scope)?;
        let mut tokens = Vec::new();
        if play.is_some() || destination < kept || !removed_ids.is_empty() {
            tokens.try_reserve_exact(kept)?;
            tokens.extend(
                self.queue
                    .iter()
                    .zip(&removed)
                    .filter(|(_, gone)| !**gone)
                    .map(|(entry, _)| TrackToken::new(entry.id)),
            );
        }
        let mut start = 0;
        if let Some(play) = play {
            if play == Start::Pick {
                let ids: Vec<u64> = append.entries.iter().map(|entry| entry.id).collect();
                start = self.order.pick(&ids).unwrap_or(0);
            }
            let first = &append.entries[start];
            append.selection =
                Some(self.prepare_first(first, append.revision, destination + start)?);
            append.entries[start].played = true;
            append.entries[start].visited = true;
        }
        Ok(PreparedReplace {
            append,
            start,
            anchor,
            removed,
            removed_ids,
            destination,
            before,
            tokens,
        })
    }

    /// Infallible after preparation with no intervening queue mutation. The
    /// anchor entry keeps its PCM unless the first new entry is selected.
    pub(crate) fn commit_replace_upcoming(&mut self, prepared: PreparedReplace) {
        let PreparedReplace {
            append,
            anchor,
            removed,
            removed_ids,
            destination,
            tokens,
            ..
        } = prepared;
        let PreparedAppend {
            revision,
            next_id,
            entries,
            selection,
            ..
        } = append;
        assert_eq!(self.revision + 1, revision);
        assert_eq!(self.next_id, entries[0].id);
        assert_eq!(self.queue.len(), removed.len());
        if selection.is_some() {
            self.output.set_paused(true);
            self.drop_passes();
        }
        for (entry, gone) in self.queue.iter().zip(&removed) {
            if *gone {
                entry.token.invalidate();
            }
        }
        self.forget_passes(&removed_ids);
        let mut index = 0;
        self.queue.retain(|_| {
            index += 1;
            !removed[index - 1]
        });
        if let Some((selected, _)) = self.selection
            && !self.queue.iter().any(|entry| entry.id == selected)
        {
            self.selection = None;
        }
        let rewrites = !tokens.is_empty();
        if selection.is_some() {
            for (entry, token) in self.queue.iter_mut().zip(tokens) {
                entry.token.invalidate();
                entry.token = token;
                entry.metadata = None;
            }
        } else {
            self.replace_future_tokens(anchor, tokens);
        }
        let count = entries.len();
        self.queue.extend(entries);
        self.queue[destination..].rotate_right(count);
        self.next_id = next_id;
        self.revision = revision;
        if let Some(selection) = selection {
            self.adopt_first(selection);
        } else if rewrites {
            self.relocate_decoder(anchor);
        } else {
            self.resume_after_append();
        }
    }

    /// Prepares removing every upcoming entry (see `upcoming`). `None` when
    /// nothing is upcoming.
    pub(crate) fn prepare_clear_upcoming(
        &self,
        expected_revision: u64,
    ) -> Result<Option<PreparedRemove>> {
        self.check_revision(expected_revision)?;
        let (anchor, _, removed) = self.upcoming()?;
        if !removed.contains(&true) {
            return Ok(None);
        }
        self.prepare_removal(anchor, removed).map(Some)
    }

    /// Prepares removing `ids` relative to `anchor`, the entry that is (or is
    /// about to be) current. The anchor keeps its PCM.
    pub(crate) fn prepare_remove_batch(
        &self,
        ids: &[u64],
        anchor: Option<u64>,
        expected_revision: u64,
    ) -> Result<PreparedRemove> {
        self.check_revision(expected_revision)?;
        let selected: HashSet<_> = ids.iter().copied().collect();
        ensure!(
            !ids.is_empty() && selected.len() == ids.len(),
            "removed occurrences must be distinct and non-empty"
        );
        ensure!(
            anchor.is_none_or(|anchor| !selected.contains(&anchor)),
            "a batch removal cannot remove its anchor"
        );
        let mut removed = Vec::new();
        removed.try_reserve_exact(self.queue.len())?;
        removed.extend(self.queue.iter().map(|entry| selected.contains(&entry.id)));
        ensure!(
            removed.iter().filter(|gone| **gone).count() == ids.len(),
            "queue entry not found"
        );
        self.prepare_removal(anchor, removed)
    }

    fn prepare_removal(&self, anchor: Option<u64>, removed: Vec<bool>) -> Result<PreparedRemove> {
        let revision = self
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= i64::MAX as u64)
            .context("queue revision exhausted")?;
        let mut removed_ids = Vec::new();
        removed_ids.try_reserve_exact(removed.iter().filter(|gone| **gone).count())?;
        removed_ids.extend(
            self.queue
                .iter()
                .zip(&removed)
                .filter(|(_, gone)| **gone)
                .map(|(entry, _)| entry.id),
        );
        let anchor_index = anchor.and_then(|id| self.queue.iter().position(|entry| entry.id == id));
        // Under repeat or shuffle an entry before the anchor can still be ahead.
        let rewrites = removed
            .iter()
            .enumerate()
            .any(|(index, gone)| *gone && anchor_index.is_none_or(|anchor| index > anchor))
            || removed_ids.iter().any(|id| self.ahead(*id));
        let mut tokens = Vec::new();
        if rewrites {
            tokens.try_reserve_exact(self.queue.len() - removed_ids.len())?;
            tokens.extend(
                self.queue
                    .iter()
                    .zip(&removed)
                    .filter(|(_, gone)| !**gone)
                    .map(|(entry, _)| TrackToken::new(entry.id)),
            );
        }
        Ok(PreparedRemove {
            revision,
            anchor,
            removed,
            removed_ids,
            tokens,
        })
    }

    /// Infallible after preparation with no intervening queue mutation. The
    /// anchor keeps its PCM; removing only entries before it keeps all PCM.
    pub(crate) fn commit_remove_batch(&mut self, prepared: PreparedRemove) {
        let PreparedRemove {
            revision,
            anchor,
            removed,
            removed_ids,
            tokens,
        } = prepared;
        assert_eq!(self.revision + 1, revision);
        assert_eq!(self.queue.len(), removed.len());
        let before_cursor = removed
            .iter()
            .take(self.cursor)
            .filter(|gone| **gone)
            .count();
        for (entry, gone) in self.queue.iter().zip(&removed) {
            if *gone {
                entry.token.invalidate();
            }
        }
        self.forget_passes(&removed_ids);
        let mut index = 0;
        self.queue.retain(|_| {
            index += 1;
            !removed[index - 1]
        });
        if let Some((selected, _)) = self.selection
            && !self.queue.iter().any(|entry| entry.id == selected)
        {
            self.selection = None;
        }
        self.revision = revision;
        if tokens.is_empty() {
            // Nothing removed was ahead, so the plan stands.
            self.cursor -= before_cursor;
        } else {
            self.replace_future_tokens(anchor, tokens);
            self.relocate_decoder(anchor);
        }
    }

    /// Prepares a uniform permutation of the upcoming entries (see `upcoming`),
    /// drawn by Fisher–Yates with `strategy`. `None` when the order does not change.
    pub(crate) fn prepare_shuffle_upcoming(
        &self,
        strategy: &mut dyn ShuffleStrategy,
        expected_revision: u64,
    ) -> Result<Option<PreparedReorder>> {
        self.check_revision(expected_revision)?;
        let (anchor, _, upcoming) = self.upcoming()?;
        let mut order: Vec<u64> = self
            .queue
            .iter()
            .zip(&upcoming)
            .filter(|(_, next)| **next)
            .map(|(entry, _)| entry.id)
            .collect();
        // Each place from the end takes a uniform pick of those left. A pick
        // out of range clamps to the last candidate, as `PlayOrder::pick` does.
        for end in (1..order.len()).rev() {
            let pick = strategy.choose(&order[..=end]).min(end);
            order.swap(pick, end);
        }
        Ok(self.prepare_reorder(&order)?.map(|change| PreparedReorder {
            change,
            anchor,
            order,
        }))
    }

    /// Prepares putting the entries `ids` names in that order, in the queue
    /// positions they already occupy. `None` when the order does not change.
    fn prepare_reorder(&self, ids: &[u64]) -> Result<Option<PreparedMove>> {
        let rank: HashMap<u64, usize> = ids
            .iter()
            .enumerate()
            .map(|(rank, id)| (*id, rank))
            .collect();
        ensure!(
            rank.len() == ids.len(),
            "reordered occurrences must be distinct"
        );
        let slots: Vec<usize> = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, entry)| rank.contains_key(&entry.id))
            .map(|(index, _)| index)
            .collect();
        ensure!(slots.len() == ids.len(), "queue entry not found");
        let mut destinations: Vec<usize> = (0..self.queue.len()).collect();
        for &slot in &slots {
            destinations[slot] = slots[rank[&self.queue[slot].id]];
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

    /// Infallible after preparation with no intervening queue mutation.
    /// The prepare-time anchor keeps its PCM. An entry output reached in the
    /// meantime is cut off, and playback resumes after the anchor.
    pub(crate) fn commit_reorder(&mut self, prepared: PreparedReorder) {
        self.apply_move(prepared.change, prepared.anchor);
    }

    /// Records that entry `id` became current, as the store's `played_seq` does.
    pub(crate) fn mark_played(&mut self, id: u64) {
        if let Some(entry) = self.queue.iter_mut().find(|entry| entry.id == id) {
            entry.played = true;
            entry.visited = true;
        }
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

    fn rebuild_future(&mut self, current: Option<u64>) {
        for entry in &mut self.queue {
            if Some(entry.id) != current {
                entry.token.invalidate();
                entry.token = TrackToken::new(entry.id);
                entry.metadata = None;
            }
        }
        self.relocate_decoder(current);
    }

    fn replace_future_tokens(&mut self, current: Option<u64>, tokens: Vec<Arc<TrackToken>>) {
        for (entry, token) in self.queue.iter_mut().zip(tokens) {
            if Some(entry.id) != current {
                entry.token.invalidate();
                entry.token = token;
                entry.metadata = None;
            }
        }
    }

    /// Keeps `current`'s heard pass and drops every pass after it. Decoding
    /// goes on with that pass, or resumes with the entry the order puts next.
    fn relocate_decoder(&mut self, current: Option<u64>) {
        self.cut_after(current);
        let current_index = current.and_then(|id| self.index_of(id));
        match (current_index, self.decoder.is_some()) {
            (Some(index), true) => {
                self.cursor = index;
                self.plan = self.decide(index);
            }
            (index, _) => {
                self.decoder = None;
                self.pending = 0;
                self.decoded_position = 0;
                self.plan = Plan::default();
                self.opening = None;
                self.cursor = 0;
                if let Some(index) = index {
                    self.plan = self.decide(index);
                    self.follow_plan();
                }
            }
        }
        self.finished = false;
        self.failure = None;
        self.output.mark_end(false);
    }

    fn source_metadata(
        decoder: &mut SourceDecoder,
        entry_id: u64,
        mode: GainMode,
    ) -> Arc<NowPlaying> {
        let metadata = decoder.take_metadata();
        Self::display_metadata(metadata, decoder, entry_id, mode)
    }

    fn display_metadata(
        metadata: crate::metadata::PlaybackMetadata,
        decoder: &SourceDecoder,
        entry_id: u64,
        mode: GainMode,
    ) -> Arc<NowPlaying> {
        Arc::new(NowPlaying {
            entry_id,
            title: metadata.title,
            artist: metadata.artist,
            album: metadata.album,
            duration_frames: decoder.duration_frames(),
            metadata_error: metadata.metadata_error,
            track_id: None,
            album_id: None,
            album_artist_id: None,
            compilation: None,
            format: if decoder.stream.is_some() {
                "HTTP stream".to_owned()
            } else {
                String::new()
            },
            gain_db: (mode != GainMode::Off).then(|| decoder.gain_db()),
            station: None,
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
        let mut decoder = SourceDecoder::open(entry, mode).with_context(|| {
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
        // A seek or gain change keeps what was planned after the entry, and
        // continues its pass, counted as output last played it.
        self.count_heard();
        let kept = self.current_plan().filter(|(id, _)| *id == prepared.id);
        let (counted, started_at) = self.continued_pass(prepared.id);
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
        self.begin_pass();
        let pass = self.passes.back_mut().expect("a pass just began");
        (pass.counted, pass.started_at) = (counted, started_at);
        if let Some((_, plan)) = kept {
            self.plan = plan;
        }
    }

    pub fn seek(&mut self, frame: u64) -> Result<()> {
        let id = self
            .snapshot()
            .current_id
            .context("no current entry to seek")?;
        ensure!(!self.is_stream(id), "live streams cannot seek");
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
        assert_pcm(
            &pcm,
            &[
                (48_000, 1000),
                (446, 5000),
                (199, 4000),
                (311, 2000),
                (137, 3000),
            ],
        );
        Ok(())
    }

    /// The capture holds exactly these `(frames, sample)` runs, in order.
    fn assert_pcm(pcm: &[u8], runs: &[(usize, i16)]) {
        let total: usize = runs.iter().map(|run| run.0).sum();
        assert_eq!(pcm.len(), total * 8);
        let mut frames = pcm.as_chunks::<8>().0.iter();
        for &(count, sample) in runs {
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
    }

    fn finish(player: &mut Player, deadline: Instant) -> Result<()> {
        while !player.completed() {
            assert!(Instant::now() < deadline, "playback did not finish");
            player.pump()?;
            thread::sleep(Duration::from_millis(1));
        }
        player.pump()
    }

    fn assert_filtered_pcm(pcm: &[u8], expected: &[Frame]) {
        assert_eq!(pcm.len(), expected.len() * 8);
        for (index, (bytes, frame)) in pcm.as_chunks::<8>().0.iter().zip(expected).enumerate() {
            for (channel, expected) in frame.iter().enumerate() {
                let start = channel * 4;
                let actual = f32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "frame {index}, channel {channel}"
                );
            }
        }
    }

    fn equalizer_curve() -> EqualizerSettings {
        EqualizerSettings {
            enabled: true,
            preamp_db: -6.0,
            bands_db: [12.0, 6.0, 0.0, -2.0, 0.0, 3.0, 0.0, 0.0, 0.0, -4.0],
        }
    }

    #[test]
    fn equalizer_processes_pending_pcm_once_and_changes_do_not_discard_buffers() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-eq-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let total = BLOCK_FRAMES * 70 + 137;
        let source = source(&scratch.0, "source.wav", total as u32, 1000);
        let capture = scratch.0.join("capture.f32");
        let mut player = Player::new(
            vec![source],
            OutputConfig::Capture {
                path: capture.clone(),
                clocked: false,
            },
            GainMode::Off,
        )?;
        let original = equalizer_curve();
        player.set_equalizer(&original);
        player.play();
        player.pause();
        for _ in 0..128 {
            player.pump()?;
            if player.pending != 0 {
                break;
            }
        }
        assert_eq!(player.pending, BLOCK_FRAMES, "output pool must be full");
        let pending = player.block.clone();
        let submitted = player.decoded_position;
        let cutoff = submitted as usize + player.pending;
        let buffered = player.output.snapshot().buffered_frames;
        for _ in 0..5 {
            player.pump()?;
            assert_eq!(&player.block[..], &pending[..]);
            assert_eq!(player.decoded_position, submitted);
        }
        let changed = EqualizerSettings {
            preamp_db: -12.0,
            bands_db: [-6.0; 10],
            ..original.clone()
        };
        player.set_equalizer(&changed);
        for _ in 0..5 {
            player.pump()?;
            assert_eq!(&player.block[..], &pending[..]);
            assert_eq!(player.pending, BLOCK_FRAMES);
            assert_eq!(player.decoded_position, submitted);
            assert_eq!(player.output.snapshot().buffered_frames, buffered);
        }
        player.play();
        finish(&mut player, Instant::now() + Duration::from_secs(5))?;
        assert!(player.error().is_none(), "{:?}", player.error());
        assert_eq!(player.started_entries(), &[1]);
        drop(player);
        let mut expected = vec![[1000.0 / 32768.0; 2]; total];
        let mut equalizer = Equalizer::default();
        equalizer.set_settings(&original);
        equalizer.process(&mut expected[..cutoff]);
        equalizer.set_settings(&changed);
        equalizer.process(&mut expected[cutoff..]);
        assert_filtered_pcm(&fs::read(capture)?, &expected);
        Ok(())
    }

    #[test]
    fn equalizer_history_survives_natural_gapless_track_boundaries() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-eq-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let first = source(&scratch.0, "first.wav", 137, 1000);
        let second = source(&scratch.0, "second.wav", 311, -2000);
        let capture = scratch.0.join("capture.f32");
        let mut player = Player::new(
            vec![first, second],
            OutputConfig::Capture {
                path: capture.clone(),
                clocked: false,
            },
            GainMode::Off,
        )?;
        let settings = equalizer_curve();
        player.set_equalizer(&settings);
        player.play();
        finish(&mut player, Instant::now() + Duration::from_secs(5))?;
        assert!(player.error().is_none(), "{:?}", player.error());
        assert_eq!(player.started_entries(), &[1, 2]);
        drop(player);
        let mut expected = vec![[1000.0 / 32768.0; 2]; 137];
        expected.extend_from_slice(&[[-2000.0 / 32768.0; 2]; 311]);
        let mut equalizer = Equalizer::default();
        equalizer.set_settings(&settings);
        equalizer.process(&mut expected);
        assert_filtered_pcm(&fs::read(capture)?, &expected);
        Ok(())
    }

    #[test]
    fn equalizer_history_resets_on_stop_seek_and_reselection() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-eq-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let source = source(&scratch.0, "source.wav", 10_000, 1000);
        for transition in 0..3 {
            let mut player = Player::new(
                vec![source.clone(), source.clone()],
                OutputConfig::Capture {
                    path: scratch.0.join(format!("capture-{transition}.f32")),
                    clocked: false,
                },
                GainMode::Off,
            )?;
            player.set_equalizer(&equalizer_curve());
            let selection = player.prepare_selection(1, 0, GainMode::Off)?;
            player.commit_selection(selection);
            player.play();
            player.pause();
            player.pump()?;
            let mut tail = [[0.0; 2]; 16];
            player.equalizer.process(&mut tail);
            assert!(tail.iter().any(|frame| frame[0] != 0.0));
            match transition {
                0 => player.stop(),
                1 => player.seek(211)?,
                _ => {
                    let selection = player.prepare_selection(2, 0, GainMode::Off)?;
                    player.commit_selection(selection);
                }
            }
            let mut silence = [[0.0; 2]; 16];
            player.equalizer.process(&mut silence);
            assert_eq!(silence, [[0.0; 2]; 16]);
        }
        Ok(())
    }

    #[test]
    fn replace_upcoming_keeps_current_pcm_and_played_entries() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-batch-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let played = source(&scratch.0, "played.wav", 97, 500);
        let current = source(&scratch.0, "current.wav", 48_000, 1000);
        let second = source(&scratch.0, "second.wav", 311, 2000);
        let third = source(&scratch.0, "third.wav", 137, 3000);
        let run_a = source(&scratch.0, "run-a.wav", 223, 5000);
        let run_b = source(&scratch.0, "run-b.wav", 101, 6000);
        let loaded = |id: u64, path: &PathBuf, played: bool| LoadedEntry {
            id,
            path: path.clone(),
            stream: None,
            run: None,
            played_seq: played.then_some(id),
        };
        let output = |name: &str| OutputConfig::Capture {
            path: scratch.0.join(name),
            clocked: true,
        };

        // Entry 2 is current; entry 3 is already decoding into the output.
        let mut player = Player::from_loaded(
            vec![
                loaded(1, &played, true),
                loaded(2, &current, false),
                loaded(3, &second, false),
                loaded(4, &third, false),
            ],
            1,
            5,
            output("current.f32"),
            GainMode::Off,
        )?;
        let selection = player.prepare_selection(2, 0, GainMode::Off)?;
        player.commit_selection(selection);
        player.play();
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.snapshot().current_id != Some(2) || player.snapshot().prepared_id != Some(3) {
            assert!(
                Instant::now() < deadline,
                "next occurrence was not prepared"
            );
            player.pump()?;
            thread::sleep(Duration::from_millis(1));
        }
        player.pause();
        let before = player.snapshot();
        let abandoned = player.prepare_replace_upcoming(
            vec![run_a.clone()].into_iter(),
            Placement::ReplaceUpcoming,
            1,
            None,
            RepeatScope::Queue,
            &[],
        )?;
        drop(abandoned);
        assert_eq!(player.queue_ids(), [1, 2, 3, 4]);
        assert_eq!(player.snapshot().prepared_id, Some(3));
        assert_eq!(player.snapshot().position_frames, before.position_frames);
        let prepared = player.prepare_replace_upcoming(
            vec![run_a, run_b.clone()].into_iter(),
            Placement::ReplaceUpcoming,
            1,
            None,
            RepeatScope::Queue,
            &[],
        )?;
        assert_eq!(prepared.first_id(), 5);
        assert_eq!(prepared.removed(), [3, 4]);
        assert_eq!(prepared.before(), None);
        player.commit_replace_upcoming(prepared);
        assert_eq!(player.queue_ids(), [1, 2, 5, 6]);
        let after = player.snapshot();
        assert_eq!(after.current_id, Some(2));
        assert_eq!(after.position_frames, before.position_frames);
        assert_eq!(after.queue_revision, 2);
        assert!(!after.playing);
        player.play();
        finish(&mut player, deadline)?;
        // The current entry's PCM plays out whole; the removed entries' prefetched
        // PCM never does.
        assert_eq!(player.started_entries(), &[2, 5, 6]);
        drop(player);
        assert_pcm(
            &fs::read(scratch.0.join("current.f32"))?,
            &[(48_000, 1000), (223, 5000), (101, 6000)],
        );

        // With nothing current, every unplayed entry goes and played ones stay.
        let mut player = Player::from_loaded(
            vec![
                loaded(1, &second, false),
                loaded(2, &played, true),
                loaded(3, &third, false),
            ],
            1,
            4,
            output("idle.f32"),
            GainMode::Off,
        )?;
        assert_eq!(player.snapshot().current_id, None);
        let prepared = player.prepare_replace_upcoming(
            vec![run_b].into_iter(),
            Placement::ReplaceUpcoming,
            1,
            Some(Start::First),
            RepeatScope::Queue,
            &[],
        )?;
        assert_eq!(prepared.removed(), [1, 3]);
        player.commit_replace_upcoming(prepared);
        assert_eq!(player.queue_ids(), [2, 4]);
        assert_eq!(player.snapshot().current_id, Some(4));
        assert!(player.snapshot().playing);
        finish(&mut player, deadline)?;
        assert_eq!(player.started_entries(), &[4]);
        drop(player);
        assert_pcm(&fs::read(scratch.0.join("idle.f32"))?, &[(101, 6000)]);
        Ok(())
    }
    #[test]
    fn clear_upcoming_keeps_current_audible() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-batch-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let played = source(&scratch.0, "played.wav", 97, 500);
        let current = source(&scratch.0, "current.wav", 48_000, 1000);
        let second = source(&scratch.0, "second.wav", 311, 2000);
        let third = source(&scratch.0, "third.wav", 137, 3000);
        let loaded = |id: u64, path: &PathBuf, played: bool| LoadedEntry {
            id,
            path: path.clone(),
            stream: None,
            run: None,
            played_seq: played.then_some(id),
        };
        let output = |name: &str| OutputConfig::Capture {
            path: scratch.0.join(name),
            clocked: true,
        };

        // Entry 2 is current and playing; entry 3 is already decoding into the output.
        let mut player = Player::from_loaded(
            vec![
                loaded(1, &played, true),
                loaded(2, &current, true),
                loaded(3, &second, false),
                loaded(4, &third, false),
            ],
            1,
            5,
            output("current.f32"),
            GainMode::Off,
        )?;
        let selection = player.prepare_selection(2, 0, GainMode::Off)?;
        player.commit_selection(selection);
        player.play();
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.snapshot().current_id != Some(2) || player.snapshot().prepared_id != Some(3) {
            assert!(
                Instant::now() < deadline,
                "next occurrence was not prepared"
            );
            player.pump()?;
            thread::sleep(Duration::from_millis(1));
        }
        assert!(player.prepare_clear_upcoming(2).is_err());
        let abandoned = player.prepare_clear_upcoming(1)?.unwrap();
        drop(abandoned);
        assert_eq!(player.queue_ids(), [1, 2, 3, 4]);
        assert_eq!(player.snapshot().prepared_id, Some(3));
        let prepared = player.prepare_clear_upcoming(1)?.unwrap();
        assert_eq!(prepared.removed(), [3, 4]);
        player.commit_remove_batch(prepared);
        assert_eq!(player.queue_ids(), [1, 2]);
        let after = player.snapshot();
        assert_eq!(after.current_id, Some(2));
        assert_eq!(after.queue_revision, 2);
        assert!(after.playing);
        // Nothing is upcoming now, so there is nothing to clear.
        assert!(player.prepare_clear_upcoming(2)?.is_none());
        // Trimming a played entry before the current one leaves its PCM alone.
        assert!(player.prepare_remove_batch(&[1, 1], Some(2), 2).is_err());
        assert!(player.prepare_remove_batch(&[9], Some(2), 2).is_err());
        assert!(player.prepare_remove_batch(&[1, 2], Some(2), 2).is_err());
        let prepared = player.prepare_remove_batch(&[1], Some(2), 2)?;
        player.commit_remove_batch(prepared);
        assert_eq!(player.queue_ids(), [2]);
        assert_eq!(player.snapshot().queue_revision, 3);
        finish(&mut player, deadline)?;
        // The current entry plays out whole; the cleared entries' prefetched PCM
        // never does.
        assert_eq!(player.started_entries(), &[2]);
        drop(player);
        assert_pcm(&fs::read(scratch.0.join("current.f32"))?, &[(48_000, 1000)]);

        // With nothing current, every unplayed entry goes and played ones stay.
        let mut player = Player::from_loaded(
            vec![
                loaded(1, &second, false),
                loaded(2, &played, true),
                loaded(3, &third, false),
            ],
            1,
            4,
            output("idle.f32"),
            GainMode::Off,
        )?;
        assert_eq!(player.snapshot().current_id, None);
        let prepared = player.prepare_clear_upcoming(1)?.unwrap();
        assert_eq!(prepared.removed(), [1, 3]);
        player.commit_remove_batch(prepared);
        assert_eq!(player.queue_ids(), [2]);
        Ok(())
    }
    #[test]
    fn upcoming_follows_queue_order_without_shuffle_and_played_marks_with_it() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-batch-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let skipped = source(&scratch.0, "skipped.wav", 97, 500);
        let current = source(&scratch.0, "current.wav", 48_000, 1000);
        let later = source(&scratch.0, "later.wav", 311, 2000);
        let loaded = |id: u64, path: &PathBuf, played: bool| LoadedEntry {
            id,
            path: path.clone(),
            stream: None,
            run: None,
            played_seq: played.then_some(id),
        };
        let open = |name: &str| -> Result<Player> {
            // Entry 1 was skipped (unplayed) before the current entry 2.
            let mut player = Player::from_loaded(
                vec![
                    loaded(1, &skipped, false),
                    loaded(2, &current, true),
                    loaded(3, &later, false),
                ],
                1,
                4,
                OutputConfig::Capture {
                    path: scratch.0.join(name),
                    clocked: true,
                },
                GainMode::Off,
            )?;
            let selection = player.prepare_selection(2, 0, GainMode::Off)?;
            player.commit_selection(selection);
            Ok(player)
        };

        // Shuffle off: only entries after the current one, in queue order.
        let mut player = open("off.f32")?;
        assert_eq!(player.upcoming_ids()?, [3]);
        assert_eq!(player.upcoming_summary(Some(2)), (Some(1), 1));
        let prepared = player.prepare_clear_upcoming(1)?.unwrap();
        assert_eq!(prepared.removed(), [3]);
        player.commit_remove_batch(prepared);
        assert_eq!(player.queue_ids(), [1, 2]);
        assert_eq!(player.upcoming_summary(Some(2)), (Some(1), 0));
        drop(player);

        // Shuffle on: every unplayed entry other than the current one.
        let mut player = open("on.f32")?;
        player.set_shuffle(Some(Box::new(First)));
        assert_eq!(player.upcoming_ids()?, [1, 3]);
        assert_eq!(player.upcoming_summary(Some(2)), (Some(1), 2));
        let prepared = player.prepare_clear_upcoming(1)?.unwrap();
        assert_eq!(prepared.removed(), [1, 3]);
        player.commit_remove_batch(prepared);
        assert_eq!(player.queue_ids(), [2]);
        assert_eq!(player.upcoming_summary(Some(2)), (Some(0), 0));
        drop(player);

        // Shuffle on, Replace Upcoming removes the skipped entry too.
        let mut player = open("replace.f32")?;
        player.set_shuffle(Some(Box::new(First)));
        let prepared = player.prepare_replace_upcoming(
            vec![later.clone()].into_iter(),
            Placement::ReplaceUpcoming,
            1,
            None,
            RepeatScope::Queue,
            &[],
        )?;
        assert_eq!(prepared.removed(), [1, 3]);
        player.commit_replace_upcoming(prepared);
        assert_eq!(player.queue_ids(), [2, 4]);
        drop(player);

        // Nothing current: every unplayed entry, whatever the shuffle state.
        let mut player = open("idle.f32")?;
        player.stop();
        assert_eq!(player.upcoming_summary(None), (None, 2));
        player.set_shuffle(Some(Box::new(First)));
        assert_eq!(player.upcoming_ids()?, [1, 3]);
        assert_eq!(player.upcoming_summary(None), (None, 2));
        Ok(())
    }
    #[test]
    fn played_run_with_trim_goes_straight_after_the_current_entry() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-batch-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let played = source(&scratch.0, "played.wav", 97, 500);
        let current = source(&scratch.0, "current.wav", 48_000, 1000);
        let second = source(&scratch.0, "second.wav", 311, 2000);
        let third = source(&scratch.0, "third.wav", 137, 3000);
        let run = source(&scratch.0, "run.wav", 223, 5000);
        let loaded = |id: u64, path: &PathBuf, played: bool| LoadedEntry {
            id,
            path: path.clone(),
            stream: None,
            run: None,
            played_seq: played.then_some(id),
        };
        let capture = scratch.0.join("capture.f32");
        // Entry 2 is current and paused; entry 1 is played and is trimmed.
        let mut player = Player::from_loaded(
            vec![
                loaded(1, &played, true),
                loaded(2, &current, true),
                loaded(3, &second, false),
                loaded(4, &third, false),
            ],
            1,
            5,
            OutputConfig::Capture {
                path: capture.clone(),
                clocked: true,
            },
            GainMode::Off,
        )?;
        let selection = player.prepare_selection(2, 0, GainMode::Off)?;
        player.commit_selection(selection);
        assert!(
            player
                .prepare_replace_upcoming(
                    vec![run.clone()].into_iter(),
                    Placement::Next,
                    1,
                    Some(Start::First),
                    RepeatScope::Queue,
                    &[9],
                )
                .is_err()
        );
        let prepared = player.prepare_replace_upcoming(
            vec![run].into_iter(),
            Placement::Next,
            1,
            Some(Start::First),
            RepeatScope::Queue,
            &[1],
        )?;
        assert_eq!(prepared.removed(), [1]);
        // Straight after the current entry, counted among the kept entries.
        assert_eq!(prepared.before(), Some(3));
        player.commit_replace_upcoming(prepared);
        assert_eq!(player.queue_ids(), [2, 5, 3, 4]);
        assert_eq!(player.snapshot().current_id, Some(5));
        let deadline = Instant::now() + Duration::from_secs(5);
        finish(&mut player, deadline)?;
        // The paused entry's prefetched PCM never plays; the run plays now and
        // the old upcoming entries follow it.
        assert_eq!(player.started_entries(), &[5, 3, 4]);
        // The new current entry is played: with nothing current, only the
        // entries the owner never marked count as upcoming.
        player.stop();
        assert_eq!(player.upcoming_ids()?, [3, 4]);
        drop(player);
        assert_pcm(
            &fs::read(capture)?,
            &[(223, 5000), (311, 2000), (137, 3000)],
        );
        Ok(())
    }
    /// Always picks the first candidate, so Fisher–Yates gives a fixed order.
    struct First;
    impl crate::play_order::ShuffleStrategy for First {
        fn choose(&mut self, _candidates: &[u64]) -> usize {
            0
        }
    }
    /// Answers out of range, as a misbehaving strategy might.
    struct Beyond;
    impl crate::play_order::ShuffleStrategy for Beyond {
        fn choose(&mut self, _candidates: &[u64]) -> usize {
            usize::MAX
        }
    }
    #[test]
    fn shuffle_upcoming_clamps_an_out_of_range_pick() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-batch-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let paths = (0..4)
            .map(|index| source(&scratch.0, &format!("{index}.wav"), 137, 1000))
            .collect();
        let player = Player::new(
            paths,
            OutputConfig::Capture {
                path: scratch.0.join("capture.f32"),
                clocked: false,
            },
            GainMode::Off,
        )?;
        // Each pick clamps to the last candidate, as `PlayOrder::pick` does:
        // every place keeps its entry, so the order does not change.
        assert!(player.prepare_shuffle_upcoming(&mut Beyond, 1)?.is_none());
        Ok(())
    }
    #[test]
    fn shuffle_upcoming_stays_anchored_when_output_reaches_a_shuffled_entry() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-batch-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let runs = [
            (4_800, 1000),
            (48_000, 2000),
            (137, 3000),
            (199, 4000),
            (223, 5000),
        ];
        let paths = runs
            .iter()
            .enumerate()
            .map(|(index, (frames, sample))| {
                source(&scratch.0, &format!("{index}.wav"), *frames, *sample)
            })
            .collect();
        let capture = scratch.0.join("capture.f32");
        let mut player = Player::new(
            paths,
            OutputConfig::Capture {
                path: capture.clone(),
                clocked: true,
            },
            GainMode::Off,
        )?;
        player.play();
        player.pause();
        let deadline = Instant::now() + Duration::from_secs(5);
        // Fill the transition while paused: selecting entry 1 alone does not
        // guarantee that a later pump has decoded entry 2.
        while player.snapshot().output.buffered_frames < PREBUFFER_FRAMES {
            assert!(Instant::now() < deadline, "transition did not prebuffer");
            player.pump()?;
        }
        player.play();
        while player.snapshot().current_id != Some(1) {
            assert!(Instant::now() < deadline, "playback did not start");
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(player.snapshot().prepared_id, Some(2));
        let prepared = player.prepare_shuffle_upcoming(&mut First, 1)?.unwrap();
        let order = prepared.order().to_vec();
        assert_eq!(order, [3, 4, 5, 2]);
        // The store commit takes time; output moves on into a shuffled entry.
        while player.snapshot().current_id != Some(2) {
            assert!(Instant::now() < deadline, "output did not reach entry 2");
            thread::sleep(Duration::from_millis(1));
        }
        player.commit_reorder(prepared);
        assert_eq!(player.queue_ids(), [1, 3, 4, 5, 2]);
        finish(&mut player, deadline)?;
        // Entry 2 is cut off and playback resumes at the new first upcoming
        // entry after entry 1; entry 2 then plays whole in its new place.
        assert_eq!(player.started_entries(), &[1, 2, 3, 4, 5, 2]);
        drop(player);
        let pcm = fs::read(capture)?;
        let cut = pcm.len() / 8 - 4_800 - 137 - 199 - 223 - 48_000;
        assert!(cut > 0 && cut < 48_000, "{cut} frames of entry 2 played");
        assert_pcm(
            &pcm,
            &[
                (4_800, 1000),
                (cut, 2000),
                (137, 3000),
                (199, 4000),
                (223, 5000),
                (48_000, 2000),
            ],
        );
        Ok(())
    }
    #[test]
    fn replace_upcoming_stays_anchored_when_output_reaches_a_removed_entry() -> Result<()> {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("tyranny-batch-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&scratch.0)?;
        let current = source(&scratch.0, "current.wav", 4_800, 1000);
        let second = source(&scratch.0, "second.wav", 48_000, 2000);
        let third = source(&scratch.0, "third.wav", 137, 3000);
        let run = source(&scratch.0, "run.wav", 223, 5000);
        let capture = scratch.0.join("capture.f32");
        let mut player = Player::new(
            vec![current, second, third],
            OutputConfig::Capture {
                path: capture.clone(),
                clocked: true,
            },
            GainMode::Off,
        )?;
        player.play();
        player.pause();
        let deadline = Instant::now() + Duration::from_secs(5);
        // Fill entry 1 and the beginning of entry 2 before allowing output to
        // consume the transition. Current identity is not a prebuffer barrier.
        while player.snapshot().output.buffered_frames < PREBUFFER_FRAMES {
            assert!(Instant::now() < deadline, "transition did not prebuffer");
            player.pump()?;
        }
        player.play();
        while player.snapshot().current_id != Some(1) {
            assert!(Instant::now() < deadline, "playback did not start");
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(player.snapshot().prepared_id, Some(2));
        let prepared = player.prepare_replace_upcoming(
            vec![run].into_iter(),
            Placement::ReplaceUpcoming,
            1,
            None,
            RepeatScope::Queue,
            &[],
        )?;
        assert_eq!(prepared.removed(), [2, 3]);
        // The store commit takes time; output moves on into a removed entry.
        while player.snapshot().current_id != Some(2) {
            assert!(Instant::now() < deadline, "output did not reach entry 2");
            thread::sleep(Duration::from_millis(1));
        }
        player.commit_replace_upcoming(prepared);
        assert_eq!(player.queue_ids(), [1, 4]);
        finish(&mut player, deadline)?;
        // The removed entry is cut off and the run follows the anchor entry; the
        // queue does not restart from its first entry.
        assert_eq!(player.started_entries(), &[1, 2, 4]);
        drop(player);
        let pcm = fs::read(capture)?;
        let cut = pcm.len() / 8 - 4_800 - 223;
        assert!(cut > 0 && cut < 48_000, "{cut} frames of entry 2 played");
        assert_pcm(&pcm, &[(4_800, 1000), (cut, 2000), (223, 5000)]);
        Ok(())
    }
}
