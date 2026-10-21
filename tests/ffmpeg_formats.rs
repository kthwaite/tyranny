//! Guards the claim that every scanner extension indexes and decodes with the
//! linked FFmpeg. Each extension has a fixture, or a manifest exception that
//! names why it has none (tak: no encoder outside Windows); exceptions are only
//! checked for coverage.
//!
//! Run against the pinned release FFmpeg (spike method):
//!
//! ```text
//! PKG_CONFIG_LIBDIR=<prefix>/lib/pkgconfig CARGO_TARGET_DIR=target/release-ffmpeg \
//!   cargo test --test ffmpeg_formats --test gapless
//! ```
//!
//! `<prefix>` is what `scripts/build_ffmpeg.py` prints, for example
//! `target/native/ffmpeg-9.0.2-arm64-...` (directory pattern
//! `target/native/ffmpeg-9.0.2-arm64-*`).
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tyranny::{
    Frame,
    decoder::{Decoder, GainMode},
    library::Library,
    library_types::{AUDIO_EXTENSIONS, LibraryCommand, LibraryReply, PageRequest, ScanPhase},
    output::OutputConfig,
    owner::{Owner, OwnerConfig},
    playback::Player,
};

struct Fixture {
    extension: String,
    file: String,
    /// Stereo reference frames at the engine rate: FFmpeg CLI, packet-duration
    /// sums where marked, or the independent libopenmpt C API for trackers.
    frames: usize,
    /// Present on the self-authored tracker fixtures, not filename-derived.
    title: Option<String>,
    seconds: f64,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A scanner extension with no fixture, and why.
struct Exception {
    extension: String,
    reason: String,
}

fn manifest() -> (Vec<Fixture>, Vec<Exception>) {
    let text = fs::read_to_string(fixture_root().join("manifest.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let formats = &json["formats"];
    let files = formats["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| Fixture {
            extension: entry["extension"].as_str().unwrap().to_owned(),
            file: entry["file"].as_str().unwrap().to_owned(),
            frames: entry["frames"].as_u64().expect(
                "manifest entry lacks frames; regenerate with scripts/make_test_fixtures.py \
                 (trackers only: scripts/make_tracker_fixtures.py --libopenmpt <pinned-library>)",
            ) as usize,
            title: entry["title"].as_str().map(str::to_owned),
            seconds: entry["seconds"].as_f64().unwrap(),
        })
        .collect();
    let exceptions = formats["exceptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| Exception {
            extension: entry["extension"].as_str().unwrap().to_owned(),
            reason: entry["reason"].as_str().unwrap_or_default().to_owned(),
        })
        .collect();
    (files, exceptions)
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tyranny-formats-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scanner_extensions_are_all_covered() {
    let (files, exceptions) = manifest();
    let fixtures: BTreeSet<&str> = files.iter().map(|f| f.extension.as_str()).collect();
    let excepted: BTreeSet<&str> = exceptions.iter().map(|e| e.extension.as_str()).collect();
    let unexplained: Vec<_> = exceptions
        .iter()
        .filter(|e| e.reason.trim().is_empty())
        .map(|e| e.extension.as_str())
        .collect();
    let scanner: BTreeSet<&str> = AUDIO_EXTENSIONS.iter().copied().collect();
    let both: Vec<_> = fixtures.intersection(&excepted).collect();
    let covered: BTreeSet<&str> = fixtures.union(&excepted).copied().collect();
    let missing: Vec<_> = scanner.difference(&covered).collect();
    let extra: Vec<_> = covered.difference(&scanner).collect();
    assert!(
        both.is_empty() && missing.is_empty() && extra.is_empty() && unexplained.is_empty(),
        "in both fixtures and exceptions: {both:?}; scanner extensions without coverage: {missing:?}; covered but not scanned: {extra:?}; exceptions without a reason: {unexplained:?}"
    );
}

#[test]
fn every_format_fixture_decodes_to_eof() {
    let (files, _) = manifest();
    // Each fixture must decode to exactly the frame count the ffmpeg CLI gives
    // at the engine rate (recorded by scripts/make_test_fixtures.py). The CLI,
    // not the nominal 0.25 s, is the reference: tone.wma is 10240 of 12000
    // frames even there. Exact counts catch a decoder that trims or pads every
    // packet, as millisecond container durations once did for ASF and Matroska.
    // tone.spx is the exception: the CLI decodes its final Ogg Speex packet
    // whole, while `Decoder` cuts it to the duration the end granule gives, so
    // its reference is the packet durations' sum (the 0.25 s source exactly).
    // Trackers use a direct libopenmpt reference at 48 kHz stereo, all subsongs.
    let mut failures = Vec::new();
    let mut buffer = vec![Frame::default(); 4096];
    for fixture in &files {
        let path = fixture_root().join(&fixture.file);
        let mut decoder = match Decoder::open(&path, GainMode::Off) {
            Ok(decoder) => decoder,
            Err(error) => {
                failures.push(format!("{}: open failed: {error:#}", fixture.file));
                continue;
            }
        };
        let mut total = 0;
        loop {
            match decoder.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => total += count,
                Err(error) => {
                    failures.push(format!("{}: read failed: {error:#}", fixture.file));
                    total = usize::MAX;
                    break;
                }
            }
            if total > fixture.frames + 4096 {
                failures.push(format!("{}: did not reach bounded EOF", fixture.file));
                total = usize::MAX;
                break;
            }
        }
        if total != usize::MAX && total != fixture.frames {
            failures.push(format!(
                "{}: decoded {total} frames, the manifest records {} from its reference decoder \
                 (see manifest encoders and frames_from; regenerate if the pinned decoder changed)",
                fixture.file, fixture.frames
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "decode failures:\n{}",
        failures.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_format_fixture_indexes_without_failures() {
    let (files, _) = manifest();
    let scratch = Scratch::new();
    let library_dir = scratch.0.join("library");
    fs::create_dir(&library_dir).unwrap();
    for fixture in &files {
        let name = Path::new(&fixture.file).file_name().unwrap();
        fs::copy(fixture_root().join(&fixture.file), library_dir.join(name)).unwrap();
    }
    // `Store` is private and creates the schema `Library::start` expects, so
    // bootstrap the database through a short-lived owner, as the server does.
    let database = scratch.0.join("state.sqlite3");
    let owner = Owner::start(OwnerConfig {
        database_path: database.clone(),
        output: OutputConfig::Capture {
            path: scratch.0.join("capture.f32"),
            clocked: false,
        },
    })
    .unwrap();
    owner.shutdown().await.unwrap();
    let library = Library::start(database).unwrap();
    library
        .handle()
        .request(LibraryCommand::AddRoot {
            path: library_dir.to_str().unwrap().to_owned(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    // The deadline guards a hang, not timing.
    let state = loop {
        let state = library.state();
        match state.scan.phase {
            ScanPhase::Complete => break state,
            ScanPhase::Failed | ScanPhase::Interrupted => panic!(
                "scan ended {:?}: {:?}",
                state.scan.phase, state.scan.last_error
            ),
            ScanPhase::Idle | ScanPhase::Discovering | ScanPhase::Indexing => {}
        }
        assert!(
            Instant::now() < deadline,
            "scan did not complete in 30 s: {:?}",
            state.scan
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(state.scan.failures, 0, "scan: {:?}", state.scan);
    assert_eq!(state.scan.skipped, 0, "scan: {:?}", state.scan);
    assert_eq!(state.tracks, files.len() as u64, "scan: {:?}", state.scan);
    for fixture in files.iter().filter(|fixture| fixture.title.is_some()) {
        let title = fixture.title.as_ref().unwrap();
        let reply = library
            .handle()
            .request(LibraryCommand::Tracks {
                album_id: None,
                page: PageRequest {
                    revision: None,
                    offset: 0,
                    limit: 10,
                    query: title.clone(),
                },
            })
            .await
            .unwrap();
        let LibraryReply::Tracks(page) = reply else {
            panic!("expected tracks query result");
        };
        assert_eq!(page.total, 1, "embedded module title: {title}");
        let track = &page.entries[0];
        assert_eq!(&track.metadata.title, title);
        assert!(track.error.is_none(), "{:?}", track.error);
        let duration = track.metadata.duration_ms.expect("module duration");
        let expected = (fixture.seconds * 1000.0).round() as u64;
        assert!(
            duration.abs_diff(expected) <= 20,
            "{title}: duration {duration} ms, expected {expected} ms across all subsongs"
        );
    }
    library.shutdown().unwrap();
}

fn decode_to_eof(decoder: &mut Decoder) -> usize {
    let mut buffer = vec![Frame::default(); 4096];
    let mut total = 0;
    loop {
        match decoder.read(&mut buffer).unwrap() {
            0 => return total,
            count => total += count,
        }
    }
}

#[test]
fn seeking_back_after_eof_decodes_like_a_fresh_seek() {
    // HE-AAC in CAF only matches a frame duration on its end-trimmed final frame.
    // That match must not carry over a seek and halve the frames decoded after it.
    let path = fixture_root().join("formats/tone-heaac.caf");
    let mut fresh = Decoder::open(&path, GainMode::Off).unwrap();
    fresh.seek(0).unwrap();
    let expected = decode_to_eof(&mut fresh);
    let mut reused = Decoder::open(&path, GainMode::Off).unwrap();
    decode_to_eof(&mut reused);
    reused.seek(0).unwrap();
    assert_eq!(decode_to_eof(&mut reused), expected);
}

/// Bounded independently of the manifest so a song loop regression cannot hang
/// the test. Sample-looped triangle notes must be audible, finite, and finite-valued.
fn tracker_pcm(decoder: &mut Decoder) -> Vec<Frame> {
    let mut pcm = Vec::new();
    let mut buffer = [Frame::default(); 4096];
    loop {
        let count = decoder.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        assert!(
            pcm.len() + count <= 5 * 48_000,
            "tracker repeated past five seconds"
        );
        assert!(
            buffer[..count]
                .iter()
                .flatten()
                .all(|sample| sample.is_finite()),
            "tracker emitted non-finite PCM"
        );
        pcm.extend_from_slice(&buffer[..count]);
    }
    assert!(
        pcm.iter().flatten().any(|sample| sample.abs() > 0.001),
        "tracker emitted no audible signal"
    );
    pcm
}

#[test]
fn trackers_stop_backward_jumps_and_seek_after_eof() {
    let (files, _) = manifest();
    let trackers: Vec<_> = files
        .iter()
        .filter(|fixture| fixture.title.is_some())
        .collect();
    assert_eq!(trackers.len(), 4);
    for fixture in trackers {
        let path = fixture_root().join(&fixture.file);
        let mut decoder = Decoder::open(&path, GainMode::Off).unwrap();
        let duration = decoder.duration_frames().expect("module duration");
        let expected_duration = (fixture.seconds * 48_000.0).round() as u64;
        assert!(
            duration.abs_diff(expected_duration) <= 960,
            "{}: duration {duration}, expected {expected_duration}",
            fixture.file
        );
        let pcm = tracker_pcm(&mut decoder);
        assert_eq!(pcm.len(), fixture.frames, "{}", fixture.file);
        // libopenmpt has a short end fade; the measured frame reference, not
        // nominal song duration, is the EOF boundary.
        assert!(pcm.len() as u64 >= duration);
        if fixture.extension == "it" {
            assert!(pcm.len() > 86_400, "second subsong was dropped");
            assert!(
                pcm[60_000..72_000]
                    .iter()
                    .flatten()
                    .any(|sample| sample.abs() > 0.001),
                "second subsong is silent"
            );
        }
        let mut buffer = [Frame::default(); 16];
        assert_eq!(decoder.read(&mut buffer).unwrap(), 0, "EOF must stay EOF");
        decoder.seek(0).unwrap();
        assert_eq!(
            tracker_pcm(&mut decoder).len(),
            fixture.frames,
            "restart {}",
            fixture.file
        );
        // IT's target is beyond the first subsong; others seek within the note.
        // Tracker seeking is row-aligned, not promised to be sample-exact.
        let target = if fixture.extension == "it" {
            60_000
        } else {
            12_000
        };
        decoder.seek(target).unwrap();
        let remaining = tracker_pcm(&mut decoder);
        let mut fresh = Decoder::open(&path, GainMode::Off).unwrap();
        fresh.seek(target).unwrap();
        assert_eq!(
            remaining.len(),
            tracker_pcm(&mut fresh).len(),
            "seek {}",
            fixture.file
        );
        assert!(
            remaining.len() < pcm.len(),
            "seek did not advance {}",
            fixture.file
        );
    }
}

#[test]
fn tracker_eof_advances_every_queue_entry_once() {
    let (files, _) = manifest();
    let paths: Vec<_> = files
        .iter()
        .filter(|fixture| fixture.title.is_some())
        .map(|fixture| fixture_root().join(&fixture.file))
        .collect();
    assert_eq!(paths.len(), 4);
    let scratch = Scratch::new();
    let capture = scratch.0.join("trackers.f32");
    let mut player = Player::new(
        paths,
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: false,
        },
        GainMode::Off,
    )
    .unwrap();
    player.play();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !player.completed() {
        assert!(
            Instant::now() < deadline,
            "tracker queue did not end: {:?}",
            player.snapshot()
        );
        player.pump().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    player.pump().unwrap();
    assert!(player.error().is_none(), "{:?}", player.error());
    assert_eq!(player.started_entries(), &[1, 2, 3, 4]);
    drop(player);
    let captured = fs::read(capture).unwrap();
    assert!(
        captured
            .as_chunks::<4>()
            .0
            .iter()
            .any(|sample| f32::from_le_bytes(*sample).abs() > 0.001),
        "queue capture is silent"
    );
}
