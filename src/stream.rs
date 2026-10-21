//! Live HTTP decoding stays entirely off the playback owner. Four recycled PCM
//! blocks provide backpressure (including while paused); cancellation never joins
//! a worker. A process-wide worker lease also bounds retiring native DNS calls,
//! which the OS resolver cannot always interrupt.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, Thread},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{
    BLOCK_FRAMES, Frame, SAMPLE_RATE,
    decoder::{Decoder, GainMode},
    metadata::PlaybackMetadata,
    protocol::StreamSpec,
};

const PCM_BLOCKS: usize = 4;
const MAX_WORKERS: usize = 4;
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ATTEMPTS: usize = 4;
// Brief bursts before another EOF must not grant an unavailable source infinite
// retries. Ten seconds of decoded audio marks a recovered connection.
const RECOVERED_FRAMES: usize = SAMPLE_RATE as usize * 10;
const MAX_ICY_BYTES: usize = 255 * 16;
static WORKERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct PcmBlock {
    frames: [Frame; BLOCK_FRAMES],
    length: usize,
    discontinuity: bool,
}

#[derive(Default)]
struct Updates {
    metadata: Option<PlaybackMetadata>,
    error: Option<String>,
    /// The title the latest publish showed, or one carried over from an
    /// earlier decoder of the entry. A connection shows it instead of the
    /// station name until the station announces a title.
    title: Option<String>,
    /// The latest publish showed a title the station announced.
    announced: bool,
}

struct Shared {
    cancelled: AtomicBool,
    finished: AtomicBool,
    started: Instant,
    deadline_ms: AtomicU64,
    gain_bits: AtomicU64,
    updates: Mutex<Updates>,
}

impl Shared {
    fn arm(&self, timeout: Duration) {
        let deadline = self.started.elapsed().as_millis() + timeout.as_millis();
        self.deadline_ms.store(deadline as u64, Ordering::Release);
    }

    fn interrupted(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self.started.elapsed().as_millis()
                >= u128::from(self.deadline_ms.load(Ordering::Acquire))
    }

    /// Publishes `metadata`. Until the connection's station has `announced`
    /// a title, the last title shown stays: a reconnect does not flash the
    /// station name.
    fn publish(&self, metadata: &PlaybackMetadata, announced: bool) {
        let mut updates = self.updates.lock().unwrap();
        let mut metadata = copy_metadata(metadata);
        if !announced && let Some(title) = &updates.title {
            metadata.title.clone_from(title);
        }
        updates.title = Some(metadata.title.clone());
        updates.announced = announced;
        updates.metadata = Some(metadata);
    }
}

struct WorkerLease;

impl WorkerLease {
    fn acquire() -> Result<Self> {
        WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_WORKERS).then_some(count + 1)
            })
            .map(|_| Self)
            .map_err(|_| anyhow::anyhow!("stream workers are still stopping; try again shortly"))
    }
}

impl Drop for WorkerLease {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Finish(Arc<Shared>);

impl Drop for Finish {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
    }
}

pub(crate) struct StreamDecoder {
    ready: Consumer<Box<PcmBlock>>,
    recycle: Producer<Box<PcmBlock>>,
    current: Option<Box<PcmBlock>>,
    offset: usize,
    shared: Arc<Shared>,
    worker: Thread,
}

impl StreamDecoder {
    /// Validate and launch only: opening, probing, priming and retries happen on
    /// the worker, never on the owner. A stream has no finite duration or seek.
    /// A decoder that replaces another passes the title it showed, which
    /// stays until the station announces one.
    pub(crate) fn open(spec: &StreamSpec, mode: GainMode, title: Option<String>) -> Result<Self> {
        let spec = StreamSpec::new(spec.url.clone(), spec.title.clone())?;
        let lease = WorkerLease::acquire()?;
        let (ready_writer, ready) = RingBuffer::new(PCM_BLOCKS);
        let (mut recycle, recycled) = RingBuffer::new(PCM_BLOCKS);
        for _ in 0..PCM_BLOCKS {
            recycle
                .push(Box::new(PcmBlock {
                    frames: [[0.0; 2]; BLOCK_FRAMES],
                    length: 0,
                    discontinuity: false,
                }))
                .expect("new stream block pool has room");
        }
        let shared = Arc::new(Shared {
            cancelled: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            started: Instant::now(),
            deadline_ms: AtomicU64::new(0),
            gain_bits: AtomicU64::new(0.0_f64.to_bits()),
            updates: Mutex::new(Updates {
                title,
                ..Updates::default()
            }),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("http-audio".to_owned())
            .spawn(move || {
                let _lease = lease;
                let _finish = Finish(Arc::clone(&worker_shared));
                if let Err(error) = run(&spec, mode, &worker_shared, ready_writer, recycled)
                    && !worker_shared.cancelled.load(Ordering::Acquire)
                {
                    worker_shared.updates.lock().unwrap().error = Some(format!("{error:#}"));
                }
            })
            .context("starting HTTP audio worker")?
            .thread()
            .clone();
        Ok(Self {
            ready,
            recycle,
            current: None,
            offset: 0,
            shared,
            worker,
        })
    }

    /// Pending is not EOF. Live-source disconnects (even clean HTTP EOF) retry
    /// on the worker and eventually fail, rather than advancing the queue.
    /// The boolean marks the first PCM after a decoder reconnect, aligned with
    /// the block rather than a worker notification that could overtake old PCM.
    pub(crate) fn poll(&mut self, destination: &mut [Frame]) -> Result<Option<(usize, bool)>> {
        ensure!(
            !destination.is_empty(),
            "stream destination must be nonempty"
        );
        if self.current.is_none() {
            self.current = self.ready.pop().ok();
            self.offset = 0;
        }
        if let Some(block) = &self.current {
            let count = (block.length - self.offset).min(destination.len());
            let discontinuity = self.offset == 0 && block.discontinuity;
            destination[..count].copy_from_slice(&block.frames[self.offset..self.offset + count]);
            self.offset += count;
            if self.offset == block.length {
                self.recycle
                    .push(self.current.take().unwrap())
                    .expect("consumed block has a free recycle slot");
                self.worker.unpark();
            }
            return Ok(Some((count, discontinuity)));
        }
        if self.shared.finished.load(Ordering::Acquire) {
            // The worker may have queued its final block between our empty pop
            // and the finished load. Observe it before reporting termination.
            if let Ok(block) = self.ready.pop() {
                self.current = Some(block);
                return self.poll(destination);
            }
            // Even terminal status retrieval must never wait on the caller.
            if let Ok(updates) = self.shared.updates.try_lock() {
                bail!(
                    "{}",
                    updates
                        .error
                        .as_deref()
                        .unwrap_or("HTTP audio worker stopped")
                );
            }
        }
        Ok(None)
    }

    pub(crate) fn take_metadata(&mut self) -> Option<PlaybackMetadata> {
        self.shared.updates.try_lock().ok()?.metadata.take()
    }

    /// Shows `title` until the station announces one, as if an earlier
    /// decoder had shown it. Waits only for a publish in progress.
    pub(crate) fn carry_title(&self, title: &str) {
        let mut updates = self.shared.updates.lock().unwrap();
        if updates.announced {
            return;
        }
        updates.title = Some(title.to_owned());
        if let Some(metadata) = &mut updates.metadata {
            title.clone_into(&mut metadata.title);
        }
    }

    pub(crate) fn gain_db(&self) -> f64 {
        f64::from_bits(self.shared.gain_bits.load(Ordering::Acquire))
    }
}

impl Drop for StreamDecoder {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::Release);
        self.worker.unpark();
    }
}

fn run(
    spec: &StreamSpec,
    mode: GainMode,
    shared: &Arc<Shared>,
    mut ready: Producer<Box<PcmBlock>>,
    mut recycled: Consumer<Box<PcmBlock>>,
) -> Result<()> {
    let mut failures = 0;
    let mut last_metadata: Option<PlaybackMetadata> = None;
    let mut spare = None;
    let mut opened_once = false;
    loop {
        if shared.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        shared.arm(OPEN_TIMEOUT);
        let interrupt = Arc::clone(shared);
        let opened = Decoder::open_stream(&spec.url, &spec.title, mode, move || {
            interrupt.interrupted()
        });
        let error = match opened {
            Err(error) => error,
            Ok(mut decoder) => {
                let mut discontinuity = opened_once;
                opened_once = true;
                shared
                    .gain_bits
                    .store(decoder.gain_db().to_bits(), Ordering::Release);
                let mut metadata = decoder.take_metadata();
                let mut icy_packet = Vec::with_capacity(MAX_ICY_BYTES);
                let mut announced = update_icy(&decoder, spec, &mut metadata, &mut icy_packet);
                publish_changed(shared, &mut last_metadata, &metadata, announced);
                let mut healthy_frames: usize = 0;
                loop {
                    if shared.cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let mut block = match spare.take().or_else(|| recycled.pop().ok()) {
                        Some(block) => block,
                        None => {
                            // No reads or metadata churn while the consumer is
                            // paused/backpressured; only a fixed PCM pool exists.
                            thread::park_timeout(Duration::from_millis(50));
                            continue;
                        }
                    };
                    shared.arm(READ_TIMEOUT);
                    let result = decoder.read(&mut block.frames);
                    announced |= update_icy(&decoder, spec, &mut metadata, &mut icy_packet);
                    publish_changed(shared, &mut last_metadata, &metadata, announced);
                    match result {
                        Ok(length) if length > 0 => {
                            block.length = length;
                            block.discontinuity = std::mem::take(&mut discontinuity);
                            ready
                                .push(block)
                                .expect("recycled block has a free ready slot");
                            healthy_frames = healthy_frames.saturating_add(length);
                            if healthy_frames >= RECOVERED_FRAMES {
                                failures = 0;
                            }
                        }
                        result => {
                            // Retain ownership across reconnects; a disconnect
                            // must never put a zero-length/EOF block on ready.
                            spare = Some(block);
                            break match result {
                                Err(error) => error,
                                Ok(_) => anyhow::anyhow!("live HTTP stream disconnected"),
                            };
                        }
                    }
                }
            }
        };
        if shared.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        failures += 1;
        if failures >= MAX_ATTEMPTS {
            return Err(error).context("HTTP stream unavailable after four attempts");
        }
        let delay = Duration::from_millis(250 << (failures - 1));
        let deadline = Instant::now() + delay;
        while !shared.cancelled.load(Ordering::Acquire) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            thread::park_timeout(remaining);
        }
    }
}

fn copy_metadata(metadata: &PlaybackMetadata) -> PlaybackMetadata {
    PlaybackMetadata {
        title: metadata.title.clone(),
        artist: metadata.artist.clone(),
        album: metadata.album.clone(),
        metadata_error: metadata.metadata_error.clone(),
    }
}

fn publish_changed(
    shared: &Shared,
    previous: &mut Option<PlaybackMetadata>,
    metadata: &PlaybackMetadata,
    announced: bool,
) {
    if previous.as_ref().is_some_and(|old| {
        old.title == metadata.title
            && old.artist == metadata.artist
            && old.album == metadata.album
            && old.metadata_error == metadata.metadata_error
    }) {
        return;
    }
    shared.publish(metadata, announced);
    *previous = Some(copy_metadata(metadata));
}

/// Applies a new ICY packet's title. Returns whether the station announced
/// one.
fn update_icy(
    decoder: &Decoder,
    spec: &StreamSpec,
    metadata: &mut PlaybackMetadata,
    previous: &mut Vec<u8>,
) -> bool {
    let Some(packet) = decoder.icy_metadata() else {
        return false;
    };
    if packet == previous.as_slice() {
        return false;
    }
    previous.clear();
    previous.extend_from_slice(packet);
    let Some(title) = icy_title(packet) else {
        return false;
    };
    let title = if title.is_empty() { &spec.title } else { title };
    if title != metadata.title {
        metadata.title.clear();
        metadata.title.push_str(title);
    }
    true
}

fn icy_title(packet: &[u8]) -> Option<&str> {
    if packet.len() > MAX_ICY_BYTES {
        return None;
    }
    let mut fields = std::str::from_utf8(packet).ok()?.trim_end_matches('\0');
    while !fields.is_empty() {
        let (key, rest) = fields.split_once("='")?;
        let (value, rest) = rest.split_once("';")?;
        if key.trim().eq_ignore_ascii_case("StreamTitle") {
            return (!value.chars().any(char::is_control)).then_some(value.trim());
        }
        fields = rest;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_poll_and_metadata_never_wait_for_worker_mailbox() {
        let (mut writer, ready) = RingBuffer::new(PCM_BLOCKS);
        let (recycle, _recycled) = RingBuffer::new(PCM_BLOCKS);
        let shared = Arc::new(Shared {
            cancelled: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            started: Instant::now(),
            deadline_ms: AtomicU64::new(0),
            gain_bits: AtomicU64::new(0.0_f64.to_bits()),
            updates: Mutex::new(Updates::default()),
        });
        let mut decoder = StreamDecoder {
            ready,
            recycle,
            current: None,
            offset: 0,
            shared: Arc::clone(&shared),
            worker: thread::current(),
        };
        let mut output = [[0.0; 2]; 2];
        let busy = shared.updates.lock().unwrap();
        assert_eq!(decoder.poll(&mut output).unwrap(), None);
        assert!(decoder.take_metadata().is_none());
        drop(busy);

        let mut previous = None;
        let mut metadata = PlaybackMetadata {
            title: "Station".to_owned(),
            artist: String::new(),
            album: String::new(),
            metadata_error: None,
        };
        publish_changed(&shared, &mut previous, &metadata, true);
        assert_eq!(decoder.take_metadata().unwrap().title, "Station");
        publish_changed(&shared, &mut previous, &metadata, true);
        assert!(decoder.take_metadata().is_none());
        metadata.title = "Live title".to_owned();
        publish_changed(&shared, &mut previous, &metadata, true);
        assert_eq!(decoder.take_metadata().unwrap().title, "Live title");
        assert!(decoder.take_metadata().is_none());

        // A reconnect queued behind old PCM must not reset the old block, and
        // a split read emits the marker only on the new connection's first PCM.
        writer
            .push(Box::new(PcmBlock {
                frames: [[0.125, -0.125]; BLOCK_FRAMES],
                length: 1,
                discontinuity: false,
            }))
            .unwrap();
        let mut block = Box::new(PcmBlock {
            frames: [[0.25, -0.25]; BLOCK_FRAMES],
            length: 3,
            discontinuity: true,
        });
        block.frames[2] = [0.5, -0.5];
        writer.push(block).unwrap();
        assert_eq!(decoder.poll(&mut output).unwrap(), Some((1, false)));
        assert_eq!(output[0], [0.125, -0.125]);
        assert_eq!(decoder.poll(&mut output).unwrap(), Some((2, true)));
        assert_eq!(output, [[0.25, -0.25]; 2]);
        assert_eq!(decoder.poll(&mut output).unwrap(), Some((1, false)));
        assert_eq!(output[0], [0.5, -0.5]);
        assert_eq!(decoder.poll(&mut output).unwrap(), None);
        drop(decoder);
        assert!(shared.cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn icy_titles_preserve_unicode_and_embedded_quotes() {
        assert_eq!(
            icy_title(b"StreamUrl='https://radio.test/';StreamTitle='Artist - Song';"),
            Some("Artist - Song")
        );
        assert_eq!(
            icy_title("StreamTitle='Björk - It's Oh So Quiet';\0\0".as_bytes()),
            Some("Björk - It's Oh So Quiet")
        );
        assert_eq!(icy_title(b"StreamTitle='';"), Some(""));
        assert_eq!(icy_title(b"streamtitle='  Song  ';"), Some("Song"));
    }

    #[test]
    fn a_new_connection_shows_the_last_title_until_the_station_announces_one() {
        let (_writer, ready) = RingBuffer::new(PCM_BLOCKS);
        let (recycle, _recycled) = RingBuffer::new(PCM_BLOCKS);
        let shared = Arc::new(Shared {
            cancelled: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            started: Instant::now(),
            deadline_ms: AtomicU64::new(0),
            gain_bits: AtomicU64::new(0.0_f64.to_bits()),
            updates: Mutex::new(Updates::default()),
        });
        let mut decoder = StreamDecoder {
            ready,
            recycle,
            current: None,
            offset: 0,
            shared: Arc::clone(&shared),
            worker: thread::current(),
        };
        let metadata = |title: &str| PlaybackMetadata {
            title: title.to_owned(),
            artist: String::new(),
            album: String::new(),
            metadata_error: None,
        };
        let mut previous = None;
        let mut publish = |title: &str, announced: bool| {
            publish_changed(&shared, &mut previous, &metadata(title), announced);
        };
        // The first connection shows the station name until a song is announced.
        publish("Station", false);
        assert_eq!(decoder.take_metadata().unwrap().title, "Station");
        publish("Song", true);
        assert_eq!(decoder.take_metadata().unwrap().title, "Song");
        // A reconnect starts from the station name: the song stays.
        publish("Station", false);
        assert_eq!(decoder.take_metadata().unwrap().title, "Song");
        publish("Next", true);
        assert_eq!(decoder.take_metadata().unwrap().title, "Next");
        // A carried title replaces an unannounced one, not an announced one.
        publish("Station", false);
        decoder.carry_title("Restored");
        assert_eq!(decoder.take_metadata().unwrap().title, "Restored");
        publish("Live", true);
        decoder.carry_title("Other");
        assert_eq!(decoder.take_metadata().unwrap().title, "Live");
    }

    #[test]
    fn icy_rejects_malformed_or_excessive_metadata() {
        for packet in [
            b"StreamTitle='missing terminator".as_slice(),
            b"StreamTitle='bad\nname';",
            b"StreamTitle='\xff';",
            b"StreamUrl='other';",
        ] {
            assert_eq!(icy_title(packet), None);
        }
        assert_eq!(icy_title(&vec![b'x'; MAX_ICY_BYTES + 1]), None);
    }

    #[test]
    fn invalid_protocol_is_rejected_before_spawning_a_worker() {
        for url in [
            "file:///tmp/music.mp3",
            "ftp://radio.test/live",
            "concat:http://radio.test/one|file:/tmp/two",
        ] {
            let spec = StreamSpec {
                url: url.to_owned(),
                title: "Station".to_owned(),
            };
            assert!(StreamDecoder::open(&spec, GainMode::Off, None).is_err());
        }
    }
}
