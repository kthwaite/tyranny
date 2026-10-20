//! Guards the claim that every scanner extension we can encode a fixture for
//! indexes and decodes with the linked FFmpeg. Extensions with no local encoder
//! are listed in the manifest and only checked for coverage.
//!
//! Run against the pinned release FFmpeg (spike method):
//!
//! ```text
//! PKG_CONFIG_LIBDIR=<prefix>/lib/pkgconfig CARGO_TARGET_DIR=target/release-ffmpeg \
//!   cargo test --test ffmpeg_formats --test gapless
//! ```
//!
//! `<prefix>` is what `scripts/build_ffmpeg.py` prints, for example
//! `target/native/ffmpeg-9.0.2-arm64-...`.
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tyranny::{
    Frame, SAMPLE_RATE,
    decoder::{Decoder, GainMode},
    library::Library,
    library_types::{AUDIO_EXTENSIONS, LibraryCommand, ScanPhase},
    output::OutputConfig,
    owner::{Owner, OwnerConfig},
};

struct Fixture {
    extension: String,
    file: String,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn manifest() -> (Vec<Fixture>, Vec<String>) {
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
        })
        .collect();
    let missing = formats["no_local_encoder"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap().to_owned())
        .collect();
    (files, missing)
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
    let (files, no_encoder) = manifest();
    let fixtures: BTreeSet<&str> = files.iter().map(|f| f.extension.as_str()).collect();
    let unencodable: BTreeSet<&str> = no_encoder.iter().map(String::as_str).collect();
    let scanner: BTreeSet<&str> = AUDIO_EXTENSIONS.iter().copied().collect();
    let both: Vec<_> = fixtures.intersection(&unencodable).collect();
    let covered: BTreeSet<&str> = fixtures.union(&unencodable).copied().collect();
    let missing: Vec<_> = scanner.difference(&covered).collect();
    let extra: Vec<_> = covered.difference(&scanner).collect();
    assert!(
        both.is_empty() && missing.is_empty() && extra.is_empty(),
        "in both fixtures and no_local_encoder: {both:?}; scanner extensions without coverage: {missing:?}; covered but not scanned: {extra:?}"
    );
}

#[test]
fn every_format_fixture_decodes_to_eof() {
    let (files, _) = manifest();
    // Fixtures are 0.25 s. The ffmpeg CLI also drops the last partial packet of
    // wma (10240 of 12000 frames), so the floor is 80%, not 90%.
    let floor = (0.8 * 0.25 * f64::from(SAMPLE_RATE)) as usize;
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
        if total != usize::MAX && total < floor {
            failures.push(format!(
                "{}: decoded {total} frames, expected at least {floor}",
                fixture.file
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
    let state = loop {
        let state = library.state();
        if state.scan.phase == ScanPhase::Complete {
            break state;
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
