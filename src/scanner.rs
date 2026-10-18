//! Two-phase, bounded library IO worker. Enumeration emits at most 128 paths per
//! event, then accepts at most 64 metadata probes per command. There are two event
//! slots and one command slot; no collection-sized in-memory worklist exists.
//!
//! Candidates are case-insensitive extensions: aac, ac3, aif, aiff, aifc, alac,
//! amr, ape, au, awb, caf, dff, dsf, dts, flac, m4a, m4b, mka, mov, mp2, mp3,
//! mp4, mpc, oga, ogg, opus, ra, shn, spx, tak, tta, wav, wave, webm, wma, wv.
//! Other extensions, symlinks and nonregular entries count as skipped. There is
//! no extensionless sniffing, playlist/CUE expansion, or symlink recursion.
//! Root + descendant directory handles are capped at 64. Any traversal error,
//! unrepresentable UTF-8 path/fingerprint, or depth overflow makes enumeration
//! incomplete, so the coordinator must NOT reconcile missing assets. Individual
//! corrupt candidates instead fail only their probe, not enumeration.
use std::{
    fs::{self, ReadDir},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};

use crate::{library_types::TrackMetadata, metadata};

const ENUMERATION_BATCH: usize = 128;
const PROBE_BATCH: usize = 64;
const EVENT_CAPACITY: usize = 2;
const MAX_DIRECTORY_DEPTH: usize = 64;
const MAX_ERROR_BYTES: usize = 2_048;
const CANCEL_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    pub modified_ns: i64,
}

#[derive(Debug)]
pub(crate) struct Enumeration {
    pub total: u64,
    pub skipped: u64,
    pub failures: u64,
    pub complete: bool,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ProbeResult {
    pub candidate: Candidate,
    pub result: std::result::Result<TrackMetadata, String>,
}

#[derive(Debug)]
pub(crate) enum ScanEvent {
    Candidates(Vec<Candidate>),
    Enumerated(Enumeration),
    Probed(Vec<ProbeResult>),
}

pub(crate) struct Scanner {
    events: Receiver<ScanEvent>,
    probes: SyncSender<Vec<Candidate>>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Scanner {
    pub fn start(root: PathBuf) -> Result<Self> {
        ensure!(root.is_absolute(), "scan root must be an absolute path");
        ensure!(root.to_str().is_some(), "scan root must be UTF-8");
        let (event_tx, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let (probes, probe_rx) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = thread::Builder::new()
            .name("library-scanner".into())
            .spawn(move || run(root, event_tx, probe_rx, worker_cancelled))
            .context("starting library scanner")?;
        Ok(Self {
            events,
            probes,
            cancelled,
            worker: Some(worker),
        })
    }

    pub fn try_recv(&self) -> std::result::Result<ScanEvent, TryRecvError> {
        self.events.try_recv()
    }

    /// Call only after Enumerated, with at most one batch in flight. Oversized
    /// batches are returned as Full without ever entering the bounded channel.
    pub fn try_probe(
        &self,
        batch: Vec<Candidate>,
    ) -> std::result::Result<(), TrySendError<Vec<Candidate>>> {
        if batch.len() > PROBE_BATCH {
            return Err(TrySendError::Full(batch));
        }
        self.probes.try_send(batch)
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.stop()
    }

    fn stop(&mut self) -> Result<()> {
        self.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("library scanner panicked"))?;
        }
        Ok(())
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run(
    root: PathBuf,
    events: SyncSender<ScanEvent>,
    probes: Receiver<Vec<Candidate>>,
    cancelled: Arc<AtomicBool>,
) {
    let Some(enumeration) = enumerate(&root, &events, &cancelled) else {
        return;
    };
    if !send_event(&events, ScanEvent::Enumerated(enumeration), &cancelled) {
        return;
    }
    while !cancelled.load(Ordering::Acquire) {
        let batch = match probes.recv_timeout(CANCEL_POLL) {
            Ok(batch) => batch,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut results = Vec::with_capacity(batch.len());
        for candidate in batch {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let result = probe_candidate(&candidate, &cancelled)
                .map_err(|error| bounded_error(format!("{}: {error:#}", candidate.path.display())));
            results.push(ProbeResult { candidate, result });
        }
        if !send_event(&events, ScanEvent::Probed(results), &cancelled) {
            return;
        }
    }
}

fn probe_candidate(candidate: &Candidate, cancelled: &Arc<AtomicBool>) -> Result<TrackMetadata> {
    let before = fingerprint(&candidate.path)?;
    ensure!(
        before == (candidate.size, candidate.modified_ns),
        "file changed since enumeration; retry on next scan"
    );
    let metadata = metadata::probe(&candidate.path, cancelled)?;
    let after = fingerprint(&candidate.path)?;
    ensure!(
        after == before,
        "file changed during metadata probe; retry on next scan"
    );
    Ok(metadata)
}

fn send_event(
    events: &SyncSender<ScanEvent>,
    mut event: ScanEvent,
    cancelled: &AtomicBool,
) -> bool {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return false;
        }
        match events.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => event = returned,
            Err(TrySendError::Disconnected(_)) => return false,
        }
        // Unlike SyncSender::send this can always escape a full channel during
        // shutdown. stop() unparks us instead of waiting out the poll interval.
        thread::park_timeout(CANCEL_POLL);
    }
}

struct Directory {
    path: PathBuf,
    entries: ReadDir,
}

fn open_directory(path: PathBuf) -> Result<Directory> {
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("reading directory attributes {}", path.display()))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "directory is missing, replaced or a symlink: {}",
        path.display()
    );
    let entries =
        fs::read_dir(&path).with_context(|| format!("enumerating directory {}", path.display()))?;
    Ok(Directory { path, entries })
}

fn enumerate(
    root: &Path,
    events: &SyncSender<ScanEvent>,
    cancelled: &AtomicBool,
) -> Option<Enumeration> {
    let mut result = Enumeration {
        total: 0,
        skipped: 0,
        failures: 0,
        complete: true,
        last_error: None,
    };
    let first = match open_directory(root.to_owned()) {
        Ok(directory) => directory,
        Err(error) => {
            result.failure(format!("{error:#}"));
            return Some(result);
        }
    };
    let mut directories = Vec::with_capacity(MAX_DIRECTORY_DEPTH);
    directories.push(first);
    let mut batch = Vec::with_capacity(ENUMERATION_BATCH);
    while let Some(directory) = directories.last_mut() {
        if cancelled.load(Ordering::Acquire) {
            return None;
        }
        let entry = match directory.entries.next() {
            Some(Ok(entry)) => entry,
            Some(Err(error)) => {
                result.failure(format!("enumerating {}: {error}", directory.path.display()));
                continue;
            }
            None => {
                directories.pop();
                continue;
            }
        };
        let path = entry.path();
        if path.to_str().is_none() {
            result.failure(format!("path is not UTF-8: {}", path.display()));
            continue;
        }
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) => {
                result.failure(format!("reading file type {}: {error}", path.display()));
                continue;
            }
        };
        if kind.is_symlink() {
            result.skipped += 1;
        } else if kind.is_dir() {
            if directories.len() >= MAX_DIRECTORY_DEPTH {
                result.failure(format!(
                    "directory depth exceeds {MAX_DIRECTORY_DEPTH}: {}",
                    path.display()
                ));
                continue;
            }
            match open_directory(path) {
                Ok(directory) => directories.push(directory),
                Err(error) => result.failure(format!("{error:#}")),
            }
        } else if !kind.is_file() || !is_candidate(&path) {
            result.skipped += 1;
        } else {
            match fingerprint(&path) {
                Ok((size, modified_ns)) => {
                    batch.push(Candidate {
                        path,
                        size,
                        modified_ns,
                    });
                    result.total += 1;
                    if batch.len() == ENUMERATION_BATCH {
                        if !send_event(events, ScanEvent::Candidates(batch), cancelled) {
                            return None;
                        }
                        batch = Vec::with_capacity(ENUMERATION_BATCH);
                    }
                }
                Err(error) => result.failure(format!("{error:#}")),
            }
        }
    }
    // A root removed/replaced while its directory handles were open is not a
    // trustworthy complete view, even if readdir itself reached EOF normally.
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => result.failure(format!("root is no longer a directory: {}", root.display())),
        Err(error) => result.failure(format!("checking scan root {}: {error}", root.display())),
    }
    if !batch.is_empty() && !send_event(events, ScanEvent::Candidates(batch), cancelled) {
        return None;
    }
    Some(result)
}

impl Enumeration {
    fn failure(&mut self, error: String) {
        self.complete = false;
        self.failures += 1;
        self.last_error = Some(bounded_error(error));
    }
}

fn bounded_error(mut error: String) -> String {
    if error.len() > MAX_ERROR_BYTES {
        let mut end = MAX_ERROR_BYTES;
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
    }
    error
}

fn fingerprint(path: &Path) -> Result<(u64, i64)> {
    let attributes = fs::symlink_metadata(path)
        .with_context(|| format!("reading file attributes {}", path.display()))?;
    ensure!(
        attributes.is_file() && !attributes.file_type().is_symlink(),
        "candidate is no longer a regular file: {}",
        path.display()
    );
    let modified = attributes
        .modified()
        .with_context(|| format!("reading modification time {}", path.display()))?;
    let modified_ns = timestamp_ns(modified)
        .with_context(|| format!("representing modification time {}", path.display()))?;
    Ok((attributes.len(), modified_ns))
}

fn timestamp_ns(time: SystemTime) -> Result<i64> {
    let nanos = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::try_from(duration.as_nanos())?,
        Err(error) => -i128::try_from(error.duration().as_nanos())?,
    };
    i64::try_from(nanos).context("modification time exceeds signed nanosecond range")
}

fn is_candidate(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    const EXTENSIONS: &[&str] = &[
        "aac", "ac3", "aif", "aiff", "aifc", "alac", "amr", "ape", "au", "awb", "caf", "dff",
        "dsf", "dts", "flac", "m4a", "m4b", "mka", "mov", "mp2", "mp3", "mp4", "mpc", "oga", "ogg",
        "opus", "ra", "shn", "spx", "tak", "tta", "wav", "wave", "webm", "wma", "wv",
    ];
    EXTENSIONS
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    struct TemporaryDirectory(PathBuf);
    impl TemporaryDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("tyranny-scanner-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn receive(scanner: &Scanner) -> ScanEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match scanner.try_recv() {
                Ok(event) => return event,
                Err(TryRecvError::Empty) if Instant::now() < deadline => thread::sleep(CANCEL_POLL),
                other => panic!("scanner did not emit expected event: {other:?}"),
            }
        }
    }

    #[test]
    fn enumeration_batches_are_bounded_and_corruption_does_not_hide_paths() {
        let directory = TemporaryDirectory::new();
        for index in 0..300 {
            fs::write(directory.0.join(format!("曲-{index}.FLAC")), b"corrupt").unwrap();
        }
        fs::write(directory.0.join("cover.jpg"), b"ignored").unwrap();
        let scanner = Scanner::start(directory.0.clone()).unwrap();
        let mut count = 0;
        let mut candidate = None;
        loop {
            match receive(&scanner) {
                ScanEvent::Candidates(batch) => {
                    assert!(batch.len() <= ENUMERATION_BATCH);
                    count += batch.len();
                    if candidate.is_none() {
                        candidate = batch.into_iter().next();
                    }
                }
                ScanEvent::Enumerated(result) => {
                    assert!(result.complete);
                    assert_eq!(result.total, 300);
                    assert_eq!(result.skipped, 1);
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(count, 300);
        scanner.try_probe(vec![candidate.unwrap()]).unwrap();
        let ScanEvent::Probed(results) = receive(&scanner) else {
            panic!("expected probes");
        };
        assert!(results[0].result.is_err());
        scanner.shutdown().unwrap();
    }

    #[test]
    fn changed_fingerprint_is_rejected_before_metadata_probe() {
        let directory = TemporaryDirectory::new();
        let path = directory.0.join("track.flac");
        fs::write(&path, b"old").unwrap();
        let (size, modified_ns) = fingerprint(&path).unwrap();
        let candidate = Candidate {
            path: path.clone(),
            size,
            modified_ns,
        };
        fs::write(&path, b"new data").unwrap();
        let result = probe_candidate(&candidate, &Arc::new(AtomicBool::new(false)));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("changed since enumeration")
        );
    }

    #[test]
    fn native_wave_probe_returns_real_audio_and_unicode_tags() {
        fn chunk(file: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
            file.extend_from_slice(kind);
            file.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            file.extend_from_slice(payload);
            if !payload.len().is_multiple_of(2) {
                file.push(0);
            }
        }
        let directory = TemporaryDirectory::new();
        let path = directory.0.join("音楽.wav");
        let mut body = b"WAVE".to_vec();
        let mut format = Vec::new();
        format.extend_from_slice(&1_u16.to_le_bytes());
        format.extend_from_slice(&2_u16.to_le_bytes());
        format.extend_from_slice(&48_000_u32.to_le_bytes());
        format.extend_from_slice(&192_000_u32.to_le_bytes());
        format.extend_from_slice(&4_u16.to_le_bytes());
        format.extend_from_slice(&16_u16.to_le_bytes());
        chunk(&mut body, b"fmt ", &format);
        let mut info = b"INFO".to_vec();
        chunk(&mut info, b"INAM", "夜の曲\0".as_bytes());
        chunk(&mut info, b"IART", "Björk\0".as_bytes());
        chunk(&mut info, b"IGNR", "Jazz\0".as_bytes());
        chunk(&mut body, b"LIST", &info);
        chunk(&mut body, b"data", &[0; 1_920]);
        let mut wave = b"RIFF".to_vec();
        wave.extend_from_slice(&(body.len() as u32).to_le_bytes());
        wave.extend_from_slice(&body);
        fs::write(&path, wave).unwrap();
        let scanner = Scanner::start(directory.0.clone()).unwrap();
        let ScanEvent::Candidates(candidates) = receive(&scanner) else {
            panic!("expected candidate");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, path);
        let ScanEvent::Enumerated(enumeration) = receive(&scanner) else {
            panic!("expected enumeration");
        };
        assert!(enumeration.complete);
        assert!(matches!(
            scanner.try_probe(vec![candidates[0].clone(); PROBE_BATCH + 1]),
            Err(TrySendError::Full(_))
        ));
        scanner.try_probe(candidates).unwrap();
        let ScanEvent::Probed(mut results) = receive(&scanner) else {
            panic!("expected probes");
        };
        let metadata = results.pop().unwrap().result.unwrap();
        assert_eq!(metadata.title, "夜の曲");
        assert_eq!(metadata.artist, "Björk");
        assert_eq!(metadata.album_artist, "Björk");
        assert_eq!(metadata.sample_rate, 48_000);
        assert_eq!(metadata.channels, 2);
        assert_eq!(metadata.duration_ms, Some(10));
        assert_eq!(metadata.codec.as_deref(), Some("WAV"));
        assert_eq!(metadata.bit_depth, Some(16));
        assert_eq!(metadata.genres, ["Jazz"]);
        scanner.shutdown().unwrap();
    }

    #[test]
    fn missing_root_and_excessive_depth_are_incomplete() {
        let directory = TemporaryDirectory::new();
        let scanner = Scanner::start(directory.0.join("missing")).unwrap();
        let ScanEvent::Enumerated(result) = receive(&scanner) else {
            panic!("expected enumeration");
        };
        assert!(!result.complete);
        assert_eq!(result.failures, 1);
        scanner.shutdown().unwrap();
        let mut nested = directory.0.clone();
        for _ in 0..MAX_DIRECTORY_DEPTH {
            nested.push("d");
            fs::create_dir(&nested).unwrap();
        }
        let scanner = Scanner::start(directory.0.clone()).unwrap();
        let ScanEvent::Enumerated(result) = receive(&scanner) else {
            panic!("expected enumeration");
        };
        assert!(!result.complete);
        assert!(result.last_error.unwrap().contains("depth exceeds"));
        scanner.shutdown().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_recursion_is_skipped() {
        use std::os::unix::fs::symlink;
        let directory = TemporaryDirectory::new();
        symlink(&directory.0, directory.0.join("loop")).unwrap();
        let scanner = Scanner::start(directory.0.clone()).unwrap();
        let ScanEvent::Enumerated(result) = receive(&scanner) else {
            panic!("expected enumeration");
        };
        assert!(result.complete);
        assert_eq!(result.skipped, 1);
        scanner.shutdown().unwrap();
    }

    // macOS filesystems reject this filename before the scanner can see it.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn non_utf8_entry_is_incomplete() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let directory = TemporaryDirectory::new();
        fs::write(
            directory
                .0
                .join(OsString::from_vec(b"invalid-\xff.flac".to_vec())),
            b"x",
        )
        .unwrap();
        let scanner = Scanner::start(directory.0.clone()).unwrap();
        let ScanEvent::Enumerated(result) = receive(&scanner) else {
            panic!("expected enumeration");
        };
        assert!(!result.complete);
        assert_eq!(result.failures, 1);
        scanner.shutdown().unwrap();
    }

    #[test]
    fn shutdown_and_drop_escape_a_full_event_channel() {
        let directory = TemporaryDirectory::new();
        for index in 0..600 {
            fs::write(directory.0.join(format!("{index}.mp3")), b"x").unwrap();
        }
        for explicit_shutdown in [true, false] {
            let scanner = Scanner::start(directory.0.clone()).unwrap();
            thread::sleep(Duration::from_millis(100));
            let started = Instant::now();
            if explicit_shutdown {
                scanner.shutdown().unwrap();
            } else {
                drop(scanner);
            }
            assert!(started.elapsed() < Duration::from_secs(2));
        }
    }

    #[test]
    fn candidate_extensions_timestamps_and_errors_have_exact_boundaries() {
        assert!(is_candidate(Path::new("/music/曲.OpUs")));
        assert!(!is_candidate(Path::new("/music/cover.jpg")));
        assert!(!is_candidate(Path::new("/music/playlist.m3u")));
        assert!(!is_candidate(Path::new("/music/no-extension")));
        assert_eq!(
            timestamp_ns(UNIX_EPOCH - Duration::from_nanos(1)).unwrap(),
            -1
        );
        assert_eq!(
            timestamp_ns(UNIX_EPOCH + Duration::from_nanos(123)).unwrap(),
            123
        );
        let error = bounded_error("音".repeat(MAX_ERROR_BYTES));
        assert!(error.len() <= MAX_ERROR_BYTES);
        assert!(error.chars().all(|character| character == '音'));
    }
}
