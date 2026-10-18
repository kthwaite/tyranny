//! One continuous stereo/48 kHz output with a 64 x 1024-frame PCM pool.
//!
//! Pause and invalidation affect data not yet handed to the device; a previously
//! scheduled device buffer cannot be recalled. Position describes consumption,
//! not the DAC cursor. `drained` additionally waits until the native backend's
//! predicted final playback deadline has passed. The estimate includes device
//! buffer depth, safety offset, hardware latency and AudioUnit processing latency;
//! it is not an acoustic measurement. Wait for `drained` before graceful teardown.
//! Dropping an output instead performs immediate cancellation and joins its worker.
//!
//! The PCM callback never allocates, frees, locks, logs or performs I/O. Native
//! render errors are latched atomically; querying and formatting errors happens
//! on the owner thread in `check_error`.

use crate::{BLOCK_FRAMES, Frame, SAMPLE_RATE};
use anyhow::{Context, Result, bail, ensure};
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod coreaudio;

const POOL_BLOCKS: usize = 64;
const START_EVENTS: usize = 64;
const EVENT_OVERFLOW: u64 = 1;
const POOL_INVARIANT: u64 = 2;
const CAPTURE_IO: u64 = 4;
const TIMESTAMP_OVERFLOW: u64 = 8;
const INVALID_DEVICE_BUFFER: u64 = 16;
#[cfg(target_os = "macos")]
const BACKEND_FAILURE: u64 = 32;

pub enum OutputConfig {
    CoreAudio,
    /// Raw little-endian f32 stereo, without a WAV header or starvation silence.
    Capture {
        path: PathBuf,
        clocked: bool,
    },
}

/// A permit for one queue occurrence. Invalidation is irreversible.
pub struct TrackToken {
    id: u64,
    valid: AtomicBool,
    first_rendered: AtomicU64,
}

impl TrackToken {
    pub fn new(id: u64) -> Arc<Self> {
        assert_ne!(id, 0, "queue occurrence IDs must be nonzero");
        Arc::new(Self {
            id,
            valid: AtomicBool::new(true),
            first_rendered: AtomicU64::new(0),
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
    }

    pub fn is_valid(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }

    /// True only after a published consumption snapshot includes this token.
    /// An older token for the same occurrence cannot satisfy this check.
    pub(crate) fn consumed_by(&self, rendered_frames: u64) -> bool {
        let first = self.first_rendered.load(Ordering::Acquire);
        first != 0 && first <= rendered_frames
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OutputSnapshot {
    pub entry_id: u64,
    pub position_frames: u64,
    pub rendered_frames: u64,
    pub buffered_frames: usize,
    pub underruns: u64,
    pub paused: bool,
    pub drained: bool,
}

struct Block {
    samples: [Frame; BLOCK_FRAMES],
    token: Option<Arc<TrackToken>>,
    start_frame: u64,
    len: usize,
    offset: usize,
}

struct Shared {
    paused: AtomicBool,
    end: AtomicBool,
    stop: AtomicBool,
    buffered: AtomicUsize,
    submitted_generation: AtomicU64,
    faults: AtomicU64,
    capture_error: Mutex<Option<io::Error>>,
    sequence: AtomicU64,
    entry_id: AtomicU64,
    position: AtomicU64,
    rendered: AtomicU64,
    underruns: AtomicU64,
    drained: AtomicBool,
    drained_generation: AtomicU64,
    /// Target amplitude `(percent / 100)^2` as `f32` bits.
    volume_bits: AtomicU32,
}

impl Shared {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(true),
            end: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            buffered: AtomicUsize::new(0),
            submitted_generation: AtomicU64::new(0),
            faults: AtomicU64::new(0),
            capture_error: Mutex::new(None),
            sequence: AtomicU64::new(0),
            entry_id: AtomicU64::new(0),
            position: AtomicU64::new(0),
            rendered: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            drained: AtomicBool::new(false),
            drained_generation: AtomicU64::new(0),
            volume_bits: AtomicU32::new(1.0_f32.to_bits()),
        }
    }

    fn failed(&self) -> bool {
        self.faults.load(Ordering::Acquire) != 0
    }
}

/// Owns only preallocated blocks. Even invalid blocks keep their token Arc until
/// the producer recycles them, so the consumer never runs a token destructor.
struct BlockConsumer {
    ready: Consumer<Box<Block>>,
    recycle: Producer<Box<Block>>,
    starts: Producer<u64>,
    shared: Arc<Shared>,
    pending: [Option<Box<Block>>; POOL_BLOCKS],
    head: usize,
    len: usize,
    entry_id: u64,
    position: u64,
    rendered: u64,
    underruns: u64,
    starving: bool,
    /// Amplitude the last call ended on; the next call ramps from it.
    amplitude: f32,
}

impl BlockConsumer {
    fn pop_front(&mut self) -> Option<Box<Block>> {
        if self.len == 0 {
            return None;
        }
        let block = self.pending[self.head].take();
        self.head = (self.head + 1) % POOL_BLOCKS;
        self.len -= 1;
        block
    }

    fn push_back(&mut self, block: Box<Block>) {
        // Total ownership is exactly POOL_BLOCKS, including ready/recycle/free.
        let index = (self.head + self.len) % POOL_BLOCKS;
        self.pending[index] = Some(block);
        self.len += 1;
    }

    fn recycle_block(&mut self, block: Box<Block>) {
        let remaining = block.len - block.offset;
        match self.recycle.push(block) {
            Ok(()) => {
                self.shared.buffered.fetch_sub(remaining, Ordering::AcqRel);
            }
            Err(PushError::Full(block)) => {
                // Impossible with a ring as large as the entire pool. Retain
                // ownership rather than freeing a Box/Arc in the callback.
                self.push_back(block);
                self.shared
                    .faults
                    .fetch_or(POOL_INVARIANT, Ordering::Release);
            }
        }
    }

    fn maintain(&mut self) {
        // Scan all retained blocks, not just the front: deleting a future track
        // must free its pool slots even when a valid current track is paused.
        for _ in 0..self.len {
            if let Some(block) = self.pop_front() {
                if block.token.as_ref().is_some_and(|token| token.is_valid()) {
                    self.push_back(block);
                } else {
                    self.recycle_block(block);
                }
            }
        }
        // The producer can refill concurrently; cap work to one pool per tick.
        for _ in 0..POOL_BLOCKS {
            if self.len == POOL_BLOCKS {
                break;
            }
            let Ok(block) = self.ready.pop() else { break };
            if block.token.as_ref().is_some_and(|token| token.is_valid()) {
                self.push_back(block);
            } else {
                self.recycle_block(block);
            }
        }
    }

    /// Writes only real PCM to a contiguous prefix, shared by both adapters.
    fn consume(&mut self, destination: &mut [f32], clocked: bool) -> usize {
        self.maintain();
        if self.shared.paused.load(Ordering::Acquire) {
            self.starving = false;
            return 0;
        }
        if self.shared.stop.load(Ordering::Acquire) || self.shared.failed() {
            return 0;
        }
        let requested = destination.len() / 2;
        let mut written = 0;
        while written < requested && self.len != 0 {
            let Some(block) = self.pending[self.head].as_mut() else {
                self.shared
                    .faults
                    .fetch_or(POOL_INVARIANT, Ordering::Release);
                break;
            };
            let Some(token) = block.token.as_ref() else {
                self.shared
                    .faults
                    .fetch_or(POOL_INVARIANT, Ordering::Release);
                break;
            };
            // Also check when entering each block, including a partial block.
            if !token.is_valid() {
                if let Some(block) = self.pop_front() {
                    self.recycle_block(block);
                }
                continue;
            }
            let count = (block.len - block.offset).min(requested - written);
            if count == 0 {
                if let Some(block) = self.pop_front() {
                    self.recycle_block(block);
                }
                continue;
            }
            if self.entry_id != token.id() {
                if self.starts.push(token.id()).is_err() {
                    // No dropped starts: report bounded event backpressure as
                    // an error before consuming the new occurrence's first PCM.
                    self.shared
                        .faults
                        .fetch_or(EVENT_OVERFLOW, Ordering::Release);
                    break;
                }
                self.entry_id = token.id();
            }
            let source = &block.samples[block.offset..block.offset + count];
            let target = &mut destination[written * 2..(written + count) * 2];
            target.as_chunks_mut::<2>().0.copy_from_slice(source);
            block.offset += count;
            self.position = block.start_frame + block.offset as u64;
            self.rendered = self.rendered.saturating_add(count as u64);
            if token.first_rendered.load(Ordering::Relaxed) == 0 {
                token.first_rendered.store(self.rendered, Ordering::Release);
            }
            self.shared.buffered.fetch_sub(count, Ordering::AcqRel);
            written += count;
            self.starving = false;
            if block.offset == block.len
                && let Some(block) = self.pop_front()
            {
                self.recycle_block(block);
            }
        }
        self.apply_volume(&mut destination[..written * 2]);
        if written < requested
            && clocked
            && self.rendered != 0
            && !self.shared.end.load(Ordering::Acquire)
            && !self.shared.failed()
            && !self.starving
        {
            self.underruns = self.underruns.saturating_add(1);
            self.starving = true;
        }
        written
    }

    /// Scales real PCM only. Ramps linearly from the previous amplitude to the
    /// target across these frames, then holds it. Atomics and arithmetic only.
    fn apply_volume(&mut self, pcm: &mut [f32]) {
        let target = f32::from_bits(self.shared.volume_bits.load(Ordering::Relaxed));
        let previous = self.amplitude;
        let frames = pcm.len() / 2;
        if frames == 0 {
            return;
        }
        self.amplitude = target;
        if previous == 1.0 && target == 1.0 {
            return;
        }
        if previous == target {
            for sample in pcm {
                *sample *= target;
            }
            return;
        }
        let step = (target - previous) / frames as f32;
        for (index, frame) in pcm.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            // The last frame lands exactly on the target.
            let amplitude = if index + 1 == frames {
                target
            } else {
                previous + step * (index + 1) as f32
            };
            frame[0] *= amplitude;
            frame[1] *= amplitude;
        }
    }

    fn at_end(&self) -> bool {
        self.shared.end.load(Ordering::Acquire)
            && self.shared.buffered.load(Ordering::Acquire) == 0
            && !self.shared.failed()
    }

    fn publish(&self, device_tail_played: bool) {
        let generation = self.shared.submitted_generation.load(Ordering::Acquire);
        // Single writer, atomic fields, and an SC sequence give readers a
        // coherent occurrence/position/rendered tuple without a callback lock.
        self.shared.sequence.fetch_add(1, Ordering::SeqCst);
        self.shared.entry_id.store(self.entry_id, Ordering::SeqCst);
        self.shared.position.store(self.position, Ordering::SeqCst);
        self.shared.rendered.store(self.rendered, Ordering::SeqCst);
        self.shared
            .underruns
            .store(self.underruns, Ordering::SeqCst);
        self.shared
            .drained
            .store(self.at_end() && device_tail_played, Ordering::SeqCst);
        self.shared
            .drained_generation
            .store(generation, Ordering::SeqCst);
        self.shared.sequence.fetch_add(1, Ordering::SeqCst);
    }
}

enum Backend {
    #[cfg(target_os = "macos")]
    CoreAudio(coreaudio::CoreAudioOutput),
    Capture(JoinHandle<()>),
}

pub struct Output {
    backend: Option<Backend>,
    shared: Arc<Shared>,
    ready: Producer<Box<Block>>,
    recycle: Consumer<Box<Block>>,
    starts: Consumer<u64>,
    free: Vec<Box<Block>>,
}

impl Output {
    pub fn open(config: OutputConfig) -> Result<Self> {
        let shared = Arc::new(Shared::new());
        let (ready_tx, ready_rx) = RingBuffer::new(POOL_BLOCKS);
        let (recycle_tx, recycle_rx) = RingBuffer::new(POOL_BLOCKS);
        let (starts_tx, starts_rx) = RingBuffer::new(START_EVENTS);
        let free = (0..POOL_BLOCKS)
            .map(|_| {
                Box::new(Block {
                    samples: [[0.0; 2]; BLOCK_FRAMES],
                    token: None,
                    start_frame: 0,
                    len: 0,
                    offset: 0,
                })
            })
            .collect();
        let consumer = BlockConsumer {
            ready: ready_rx,
            recycle: recycle_tx,
            starts: starts_tx,
            shared: Arc::clone(&shared),
            pending: std::array::from_fn(|_| None),
            head: 0,
            len: 0,
            entry_id: 0,
            position: 0,
            rendered: 0,
            underruns: 0,
            starving: false,
            amplitude: 1.0,
        };
        let backend = match config {
            OutputConfig::CoreAudio => open_coreaudio(consumer)?,
            OutputConfig::Capture { path, clocked } => {
                let (initialized_tx, initialized_rx) = mpsc::sync_channel(1);
                let worker = thread::Builder::new()
                    .name("pcm-capture".into())
                    .spawn(move || capture_worker(consumer, path, clocked, initialized_tx))
                    .context("starting PCM capture worker")?;
                match initialized_rx.recv() {
                    Ok(Ok(())) => Backend::Capture(worker),
                    Ok(Err(error)) => {
                        let _ = worker.join();
                        return Err(error).context("opening PCM capture file");
                    }
                    Err(error) => {
                        let _ = worker.join();
                        return Err(error)
                            .context("PCM capture worker exited during initialization");
                    }
                }
            }
        };
        Ok(Self {
            backend: Some(backend),
            shared,
            ready: ready_tx,
            recycle: recycle_rx,
            starts: starts_rx,
            free,
        })
    }

    /// All-or-nothing submission. Pool exhaustion returns false, never blocks.
    pub fn try_submit(
        &mut self,
        token: &Arc<TrackToken>,
        start_frame: u64,
        frames: &[Frame],
    ) -> Result<bool> {
        self.check_error()?;
        ensure!(
            !frames.is_empty() && frames.len() <= BLOCK_FRAMES,
            "PCM submission must contain 1..={BLOCK_FRAMES} frames"
        );
        ensure!(
            start_frame.checked_add(frames.len() as u64).is_some(),
            "PCM position overflow"
        );
        ensure!(
            token.is_valid(),
            "cannot submit PCM for an invalidated queue occurrence"
        );
        while let Ok(mut block) = self.recycle.pop() {
            block.token = None; // Arc destruction belongs exclusively to producer.
            self.free.push(block);
        }
        let Some(mut block) = self.free.pop() else {
            return Ok(false);
        };
        block.samples[..frames.len()].copy_from_slice(frames);
        block.token = Some(Arc::clone(token));
        block.start_frame = start_frame;
        block.len = frames.len();
        block.offset = 0;
        // Increment before publication: the consumer can immediately pop it.
        self.shared
            .buffered
            .fetch_add(frames.len(), Ordering::AcqRel);
        // A prior drained snapshot must not survive consumption of a new block
        // during the interval before the consumer publishes its next snapshot.
        self.shared
            .submitted_generation
            .fetch_add(1, Ordering::AcqRel);
        if let Err(PushError::Full(mut block)) = self.ready.push(block) {
            self.shared
                .buffered
                .fetch_sub(frames.len(), Ordering::AcqRel);
            block.token = None;
            self.free.push(block);
            return Ok(false);
        }
        self.wake_capture();
        Ok(true)
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Release);
        self.wake_capture();
    }

    /// Output amplitude is `(percent / 100)^2`, applied after buffering. The
    /// consumer ramps to it across the frames of its next call.
    pub fn set_volume(&self, percent: u8) {
        let linear = f32::from(percent.min(100)) / 100.0;
        self.shared
            .volume_bits
            .store((linear * linear).to_bits(), Ordering::Relaxed);
    }

    pub fn mark_end(&self, end: bool) {
        self.shared.end.store(end, Ordering::Release);
        self.wake_capture();
    }

    /// Consumption fields are coherent. Buffered count and requested pause are
    /// live independent atomics; invalidated PCM remains counted until reclaimed.
    pub fn snapshot(&self) -> OutputSnapshot {
        loop {
            let sequence = self.shared.sequence.load(Ordering::SeqCst);
            if sequence & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let entry_id = self.shared.entry_id.load(Ordering::SeqCst);
            let position_frames = self.shared.position.load(Ordering::SeqCst);
            let rendered_frames = self.shared.rendered.load(Ordering::SeqCst);
            let underruns = self.shared.underruns.load(Ordering::SeqCst);
            let drained = self.shared.drained.load(Ordering::SeqCst);
            let drained_generation = self.shared.drained_generation.load(Ordering::SeqCst);
            if sequence != self.shared.sequence.load(Ordering::SeqCst) {
                continue;
            }
            let buffered_frames = self.shared.buffered.load(Ordering::Acquire);
            return OutputSnapshot {
                entry_id,
                position_frames,
                rendered_frames,
                buffered_frames,
                underruns,
                paused: self.shared.paused.load(Ordering::Acquire),
                drained: drained
                    && buffered_frames == 0
                    && drained_generation
                        == self.shared.submitted_generation.load(Ordering::Acquire)
                    && self.shared.end.load(Ordering::Acquire)
                    && !self.shared.failed(),
            };
        }
    }

    /// At most 64 uncollected starts. Seeking the currently consumed ID does not
    /// emit another start; different occurrence IDs always do, even for one path.
    pub fn take_started(&mut self) -> Option<u64> {
        self.starts.pop().ok()
    }

    pub fn check_error(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(Backend::CoreAudio(stream)) = self.backend.as_ref() {
            stream.check_error()?;
        }
        let faults = self.shared.faults.load(Ordering::Acquire);
        if faults & CAPTURE_IO != 0 {
            let error = self
                .shared
                .capture_error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(error) = error.as_ref() {
                bail!("PCM capture write failed: {error}");
            }
            bail!("PCM capture write failed");
        }
        if faults & EVENT_OVERFLOW != 0 {
            bail!(
                "track-start event queue exhausted ({START_EVENTS}); collect take_started() regularly"
            );
        }
        if faults & POOL_INVARIANT != 0 {
            bail!("PCM pool ownership invariant failed");
        }
        if faults & TIMESTAMP_OVERFLOW != 0 {
            bail!("CoreAudio final playback timestamp overflowed");
        }
        if faults & INVALID_DEVICE_BUFFER != 0 {
            bail!("CoreAudio supplied a non-stereo sample buffer");
        }
        if let Some(Backend::Capture(worker)) = self.backend.as_ref() {
            ensure!(
                !worker.is_finished(),
                "PCM capture worker terminated unexpectedly"
            );
        }
        Ok(())
    }

    fn wake_capture(&self) {
        if let Some(Backend::Capture(worker)) = self.backend.as_ref() {
            worker.thread().unpark();
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(backend) = self.backend.take() {
            match backend {
                #[cfg(target_os = "macos")]
                Backend::CoreAudio(stream) => drop(stream),
                Backend::Capture(worker) => {
                    worker.thread().unpark();
                    // A prior worker panic is visible through check_error(); do
                    // not introduce a second panic while shutting down.
                    let _ = worker.join();
                }
            }
        }
    }
}

fn frame_duration(frames: usize) -> Duration {
    // Round up so drain never cuts a fractional final sample short.
    Duration::from_nanos((frames as u64 * 1_000_000_000).div_ceil(SAMPLE_RATE as u64))
}

#[cfg(target_os = "macos")]
fn open_coreaudio(consumer: BlockConsumer) -> Result<Backend> {
    coreaudio::open(consumer).map(Backend::CoreAudio)
}

#[cfg(not(target_os = "macos"))]
fn open_coreaudio(_consumer: BlockConsumer) -> Result<Backend> {
    bail!("CoreAudio output is available only on macOS")
}

fn capture_worker(
    mut consumer: BlockConsumer,
    path: PathBuf,
    clocked: bool,
    initialized: mpsc::SyncSender<io::Result<()>>,
) {
    let mut file = match File::create(path) {
        Ok(file) => file,
        Err(error) => {
            let _ = initialized.send(Err(error));
            return;
        }
    };
    if initialized.send(Ok(())).is_err() {
        return;
    }
    let shared = Arc::clone(&consumer.shared);
    let mut samples = [0.0_f32; BLOCK_FRAMES * 2];
    let mut bytes = [0_u8; BLOCK_FRAMES * 8];
    let quantum = frame_duration(BLOCK_FRAMES);
    let mut next_tick = Instant::now();
    let mut last_sample_end: Option<Instant> = None;
    let mut was_paused = true;
    while !shared.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        let paused = shared.paused.load(Ordering::Acquire);
        if paused || (clocked && !was_paused && now < next_tick) {
            consumer.maintain();
            if paused {
                consumer.starving = false;
                was_paused = true;
            }
            let played = last_sample_end.is_none_or(|end| now >= end);
            consumer.publish(played);
            let wait = if paused {
                Duration::from_millis(1)
            } else if consumer.at_end() && !played {
                last_sample_end
                    .map_or(quantum, |end| end.saturating_duration_since(now))
                    .min(next_tick.saturating_duration_since(now))
            } else {
                next_tick.saturating_duration_since(now)
            };
            thread::park_timeout(wait);
            continue;
        }
        if was_paused {
            next_tick = now;
            was_paused = false;
        }
        let tick = next_tick;
        let frames = consumer.consume(&mut samples, clocked);
        if frames != 0 {
            for (sample, bytes) in samples[..frames * 2]
                .iter()
                .zip(bytes.as_chunks_mut::<4>().0)
            {
                bytes.copy_from_slice(&sample.to_le_bytes());
            }
            if let Err(error) = file.write_all(&bytes[..frames * 8]) {
                *shared
                    .capture_error
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(error);
                shared.faults.fetch_or(CAPTURE_IO, Ordering::Release);
                consumer.publish(false);
                return;
            }
            if clocked {
                let end = tick + frame_duration(frames);
                last_sample_end = Some(last_sample_end.map_or(end, |previous| previous.max(end)));
            }
        }
        let played = !clocked || last_sample_end.is_none_or(|end| Instant::now() >= end);
        // File is unbuffered: every reported consumed frame has completed its
        // write_all before drain is published. No durability/fsync promise.
        consumer.publish(played);
        if clocked {
            next_tick += quantum;
        } else if frames == 0 {
            thread::park_timeout(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    fn ramp_wave(frame: usize) -> Frame {
        let value = ((frame % 977) as f32 - 488.0) / 500.0;
        [value, -value * 0.75]
    }

    /// Renders `blocks` full blocks through an unclocked capture at `percent`
    /// (None leaves the default) and returns the captured samples.
    fn capture(percent: Option<u8>, blocks: usize, source: impl Fn(usize) -> Frame) -> Vec<f32> {
        let path = std::env::temp_dir().join(format!(
            "tyranny-volume-{}-{}.f32",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut output = Output::open(OutputConfig::Capture {
            path: path.clone(),
            clocked: false,
        })
        .unwrap();
        if let Some(percent) = percent {
            output.set_volume(percent);
        }
        let token = TrackToken::new(1);
        for block in 0..blocks {
            let frames: Vec<Frame> = (0..BLOCK_FRAMES)
                .map(|frame| source(block * BLOCK_FRAMES + frame))
                .collect();
            assert!(
                output
                    .try_submit(&token, (block * BLOCK_FRAMES) as u64, &frames)
                    .unwrap()
            );
        }
        output.mark_end(true);
        output.set_paused(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !output.snapshot().drained {
            assert!(Instant::now() < deadline, "capture did not drain");
            output.check_error().unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        drop(output);
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect()
    }

    #[test]
    fn volume_scales_captured_pcm_by_square_law() {
        let settled = BLOCK_FRAMES * 2; // After the first block's ramp.
        let unscaled = capture(None, 4, ramp_wave);
        assert_eq!(unscaled.len(), 4 * BLOCK_FRAMES * 2);

        // 100% is bit-identical to the source, set explicitly or by default.
        let full = capture(Some(100), 4, ramp_wave);
        let source: Vec<f32> = (0..4 * BLOCK_FRAMES).flat_map(ramp_wave).collect();
        assert!(
            unscaled
                .iter()
                .zip(&source)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "default volume altered the PCM"
        );
        assert!(
            full.iter()
                .zip(&source)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "explicit 100% altered the PCM"
        );

        // 50% is 0.25x amplitude once the ramp is over.
        let half = capture(Some(50), 4, ramp_wave);
        assert_eq!(half.len(), source.len());
        for (index, (got, want)) in half.iter().zip(&source).enumerate().skip(settled) {
            assert_eq!(got.to_bits(), (want * 0.25).to_bits(), "sample {index}");
        }

        // 0% is silence once the ramp is over.
        let mute = capture(Some(0), 4, ramp_wave);
        assert!(mute[settled..].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn volume_change_ramps_without_a_step() {
        let constant = |_: usize| [1.0_f32, 1.0];
        let half = capture(Some(50), 3, constant);
        let first_block = &half[..BLOCK_FRAMES * 2];
        let left: Vec<f32> = first_block.iter().step_by(2).copied().collect();
        // From the previous amplitude (1.0) down to the target (0.25), linearly.
        let slope = (1.0 - 0.25) / BLOCK_FRAMES as f32;
        assert!(left[0] < 1.0 && left[0] > 1.0 - 2.0 * slope, "{}", left[0]);
        assert_eq!(*left.last().unwrap(), 0.25);
        for pair in left.windows(2) {
            let step = pair[0] - pair[1];
            assert!(step > 0.0 && step <= slope * 1.01, "step {step} vs {slope}");
        }
        // Then it holds the target.
        assert!(
            half[BLOCK_FRAMES * 2..]
                .iter()
                .all(|sample| *sample == 0.25)
        );
        // Both channels follow the same ramp.
        assert!(first_block.chunks(2).all(|frame| frame[0] == frame[1]));
    }
}
