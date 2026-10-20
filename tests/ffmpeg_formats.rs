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
    library_types::{AUDIO_EXTENSIONS, LibraryCommand, ScanPhase},
    output::OutputConfig,
    owner::{Owner, OwnerConfig},
};

struct Fixture {
    extension: String,
    file: String,
    /// Stereo frames the ffmpeg CLI decodes at the engine rate, or, where the
    /// manifest says `"frames_from": "packet durations"`, that the packet
    /// durations add up to.
    frames: usize,
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
                "manifest entry lacks frames; regenerate with scripts/make_test_fixtures.py",
            ) as usize,
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
        }
        if total != usize::MAX && total != fixture.frames {
            failures.push(format!(
                "{}: decoded {total} frames, the manifest records {} from the ffmpeg CLI \
                 (see manifest encoders; regenerate with scripts/make_test_fixtures.py if FFmpeg changed)",
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
    library.shutdown().unwrap();
}
