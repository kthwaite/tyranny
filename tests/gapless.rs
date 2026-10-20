//! Guards gapless playback: whenever a file carries gapless data, the player's
//! PCM equals decoding each track alone and concatenating the results.
//! Also pins the decoded length of each track against its source length.
//!
//! Three ffmpeg-encoder cases are absent on purpose, because the files
//! themselves are not gapless (see docs/superpowers/specs/2026-10-20-format-and-gapless-tests-design.md):
//! - ffmpeg `aac_at` m4a: the edit list includes the encoder padding and there
//!   is no iTunSMPB, so the file itself has a gap.
//! - ffmpeg native Vorbis encoder: it writes final granule positions rounded to
//!   multiples of 64, so the file has a 26-47 sample gap.
//! - ffmpeg libmp3lame at 44.1 kHz: its LAME tag recorded the wrong padding for
//!   one track (+5 source samples).
//!
//! Apple afconvert AAC and the lame CLI are exact and are tested instead.
//! `mp3notag48` is kept only to pin the no-codec-constant policy.
//! `mp4opus48` gets no end-trim side data from the mov demuxer, because Opus has
//! no fixed frame size; it pins the packet-duration cut in `src/decoder.rs`.
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
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use tyranny::{
    Frame,
    decoder::{Decoder, GainMode},
    output::OutputConfig,
    playback::Player,
};

const SETS: [&str; 15] = [
    "wav48",
    "flac48",
    "alac48",
    "mp3cbr48",
    "mp3vbr48",
    "mp3lame48",
    "aac48",
    "aacapple48",
    "opus48",
    "mp4opus48",
    "mp3notag48",
    "wav44",
    "flac44",
    "mp3lame44",
    "aacapple44",
];
const LENGTHS_48: [usize; 4] = [24011, 26003, 25013, 24989];
const LENGTHS_44: [usize; 4] = [22063, 23887, 22979, 22961];

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tyranny-gapless-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Set {
    name: String,
    rate: u32,
    files: Vec<PathBuf>,
    lengths: Vec<usize>,
}

fn sets() -> Vec<Set> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).unwrap()).unwrap();
    manifest["gapless"]["sets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|set| Set {
            name: set["name"].as_str().unwrap().to_owned(),
            rate: set["rate"].as_u64().unwrap() as u32,
            files: set["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|file| root.join(file.as_str().unwrap()))
                .collect(),
            lengths: set["lengths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|length| length.as_u64().unwrap() as usize)
                .collect(),
        })
        .collect()
}

fn named<'a>(sets: &'a [Set], name: &str) -> &'a Set {
    sets.iter().find(|set| set.name == name).unwrap()
}

fn decode_all(path: &Path) -> Vec<Frame> {
    let name = path.display();
    let mut decoder =
        Decoder::open(path, GainMode::Off).unwrap_or_else(|error| panic!("{name}: {error:#}"));
    let mut out = Vec::new();
    let mut buffer = vec![[0.0; 2]; 4096];
    loop {
        let count = decoder
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        if count == 0 {
            return out;
        }
        out.extend_from_slice(&buffer[..count]);
    }
}

fn capture_run(paths: &[PathBuf]) -> Vec<Frame> {
    let scratch = Scratch::new();
    let capture = scratch.path("capture.f32");
    let mut player = Player::new(
        paths.to_vec(),
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: false,
        },
        GainMode::Off,
    )
    .unwrap();
    // The deadline guards a hang, not timing: unclocked capture runs fast.
    let deadline = Instant::now() + Duration::from_secs(30);
    player.play();
    while !player.completed() {
        assert!(
            Instant::now() < deadline,
            "playback did not finish: {:?}",
            player.snapshot()
        );
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    player.pump().unwrap();
    assert!(player.error().is_none(), "{:?}", player.error());
    drop(player);
    fs::read(&capture)
        .unwrap()
        .as_chunks::<8>()
        .0
        .iter()
        .map(|bytes| {
            [
                f32::from_le_bytes(bytes[..4].try_into().unwrap()),
                f32::from_le_bytes(bytes[4..].try_into().unwrap()),
            ]
        })
        .collect()
}

/// Int16 samples of a stereo pcm_s16le WAV, as f32 frames.
fn wav_frames(path: &Path) -> Vec<Frame> {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        if id == b"data" {
            return bytes[body..body + size]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|frame| {
                    [
                        i16::from_le_bytes([frame[0], frame[1]]) as f32 / 32768.0,
                        i16::from_le_bytes([frame[2], frame[3]]) as f32 / 32768.0,
                    ]
                })
                .collect();
        }
        at = body + size + (size & 1);
    }
    panic!("{}: no data chunk", path.display());
}

fn first_difference(a: &[Frame], b: &[Frame]) -> Option<String> {
    if let Some(index) = a.iter().zip(b).position(|(x, y)| x != y) {
        return Some(format!(
            "first differing frame {index}: {:?} vs {:?}",
            a[index], b[index]
        ));
    }
    (a.len() != b.len()).then(|| format!("lengths differ: {} vs {}", a.len(), b.len()))
}

#[test]
fn manifest_matches_the_spec_lengths() {
    let sets = sets();
    let names: Vec<&str> = sets.iter().map(|set| set.name.as_str()).collect();
    assert_eq!(names, SETS);
    for set in &sets {
        let expected = if set.name.ends_with("48") {
            LENGTHS_48
        } else {
            LENGTHS_44
        };
        assert_eq!(set.lengths, expected, "{}", set.name);
        assert_eq!(set.files.len(), 4, "{}", set.name);
        assert_eq!(
            set.rate,
            if set.name.ends_with("48") {
                48_000
            } else {
                44_100
            }
        );
    }
}

#[test]
fn tagged_48k_tracks_decode_to_their_source_length() {
    let mut mismatches = Vec::new();
    for set in sets()
        .iter()
        .filter(|set| set.rate == 48_000 && set.name != "mp3notag48")
    {
        for (file, &expected) in set.files.iter().zip(&set.lengths) {
            let actual = decode_all(file).len();
            if actual != expected {
                mismatches.push(format!(
                    "{} {}: expected {expected}, got {actual}",
                    set.name,
                    file.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn resampled_44k_tracks_decode_within_one_frame() {
    let mut mismatches = Vec::new();
    for set in sets().iter().filter(|set| set.rate == 44_100) {
        for (file, &length) in set.files.iter().zip(&set.lengths) {
            let expected = length as f64 * 48_000.0 / 44_100.0;
            let actual = decode_all(file).len();
            if (actual as f64 - expected).abs() > 1.0 {
                mismatches.push(format!(
                    "{} {}: expected {expected:.2}±1, got {actual}",
                    set.name,
                    file.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn untagged_mp3_keeps_its_encoder_delay() {
    // Pins the no-codec-constant policy (src/decoder.rs:6-7): without gapless
    // data we do not guess the delay, so the 1105 delay frames stay in.
    let sets = sets();
    let set = named(&sets, "mp3notag48");
    for (file, &length) in set.files.iter().zip(&set.lengths) {
        let actual = decode_all(file).len();
        assert!(
            actual >= length + 1105,
            "{}: {actual} < {length} + 1105",
            file.display()
        );
    }
}

#[test]
fn lossless_decodes_are_bit_exact() {
    let sets = sets();
    let wav = named(&sets, "wav48");
    for (index, wav_file) in wav.files.iter().enumerate() {
        let source = wav_frames(wav_file);
        let decoded = decode_all(wav_file);
        if let Some(diff) = first_difference(&source, &decoded) {
            panic!("wav48 track {}: {diff}", index + 1);
        }
        let flac44 = decode_all(&named(&sets, "flac44").files[index]);
        let wav44 = decode_all(&named(&sets, "wav44").files[index]);
        if let Some(diff) = first_difference(&wav44, &flac44) {
            panic!("flac44 track {} differs from wav44: {diff}", index + 1);
        }
        for name in ["flac48", "alac48"] {
            let other = decode_all(&named(&sets, name).files[index]);
            if let Some(diff) = first_difference(&decoded, &other) {
                panic!("{name} track {} differs from wav48: {diff}", index + 1);
            }
        }
    }
}

#[test]
fn player_run_equals_the_tracks_end_to_end() {
    // Exact for every set, 44.1k included: per-track `decode_all` restarts the
    // resampler per track just as the player does, so the capture equals the
    // concatenation. (Against a *continuous* decode the per-track restart is
    // still a known +-1 frame difference; that waits on the deferred swr-carry
    // follow-up.)
    let mut failures = Vec::new();
    for set in sets() {
        let expected: Vec<Frame> = set.files.iter().flat_map(|file| decode_all(file)).collect();
        let actual = capture_run(&set.files);
        if let Some(diff) = first_difference(&expected, &actual) {
            failures.push(format!("{}: {diff}", set.name));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
