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
use base64::Engine as _;
use lofty::{
    ape::{ApeFile, ApeItem, ApeTag},
    config::{ParseOptions, WriteOptions},
    file::{AudioFile, TaggedFileExt},
    musepack::MpcFile,
    picture::{APE_PICTURE_TYPES, MimeType, Picture, PictureType},
    probe::Probe,
    tag::{Accessor, ItemValue, Tag, TagExt},
    wavpack::WavPackFile,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::BufReader,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tyranny::{
    Frame,
    decoder::{Decoder, GainMode},
    library::Library,
    library_types::{
        AUDIO_EXTENSIONS, LibraryCommand, LibraryReply, LibraryState, PageRequest, ScanPhase, Track,
    },
    output::OutputConfig,
    owner::{Owner, OwnerConfig},
    playback::Player,
    protocol::TagField,
    tagging::tags::{
        TagEdit, WritableFormat, edit_tags, read_tags, unwritable_fields, writable_format,
    },
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
    let (library, state) = scan(&scratch, &library_dir).await;
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

/// Scans `library_dir` into a fresh library in `scratch` and waits for the scan.
async fn scan(scratch: &Scratch, library_dir: &Path) -> (Library, LibraryState) {
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
    (library, state)
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

/// The round-trip values, keyed by `TagField`.
fn round_trip_values() -> BTreeMap<TagField, String> {
    let text = fs::read_to_string(fixture_root().join("tag_roundtrip_values.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

fn png() -> Picture {
    let data = base64::engine::general_purpose::STANDARD
        .decode(PNG_1X1)
        .unwrap();
    Picture::unchecked(data)
        .pic_type(PictureType::CoverFront)
        .mime_type(MimeType::Png)
        .build()
}

/// The writable format each fixture must be detected as, from its extension.
fn expected_format(file: &str) -> Option<WritableFormat> {
    let extension = Path::new(file).extension().unwrap().to_str().unwrap();
    match extension {
        "aac" => Some(WritableFormat::Aac),
        "aif" | "aiff" | "aifc" => Some(WritableFormat::Aiff),
        "alac" | "m4a" | "m4b" | "mp4" => Some(WritableFormat::Mp4),
        "ape" => Some(WritableFormat::Ape),
        "flac" => Some(WritableFormat::Flac),
        "mp2" | "mp3" => Some(WritableFormat::Mpeg),
        "mpc" => Some(WritableFormat::Mpc),
        "ogg" => Some(WritableFormat::Vorbis),
        "opus" => Some(WritableFormat::Opus),
        "spx" => Some(WritableFormat::Speex),
        "wav" | "wave" => Some(WritableFormat::Wav),
        "wv" => Some(WritableFormat::WavPack),
        "ac3" | "amr" | "au" | "awb" | "caf" | "dff" | "dsf" | "dts" | "it" | "mka" | "mod"
        | "mov" | "oga" | "ra" | "s3m" | "shn" | "tta" | "webm" | "wma" | "xm" => None,
        other => panic!("{file}: add .{other} to expected_format"),
    }
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
fn writable_formats_are_detected_from_content() {
    let (files, _) = manifest();
    let mut failures = Vec::new();
    for fixture in &files {
        let detected = writable_format(&fixture_root().join(&fixture.file)).unwrap();
        let expected = expected_format(&fixture.file);
        if detected != expected {
            failures.push(format!(
                "{}: detected {detected:?}, expected {expected:?}",
                fixture.file
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A writable fixture copied into a library directory and tagged.
struct Written {
    fixture: Fixture,
    format: WritableFormat,
    path: PathBuf,
    /// The fields written: the round-trip values minus the format's deny list.
    values: BTreeMap<TagField, String>,
}

/// APE, Musepack and WavPack keep pictures as binary APE items, which lofty's
/// generic tag neither reads nor writes.
fn uses_ape_pictures(format: WritableFormat) -> bool {
    matches!(
        format,
        WritableFormat::Ape | WritableFormat::Mpc | WritableFormat::WavPack
    )
}

fn ape_tag(path: &Path, format: WritableFormat) -> Option<ApeTag> {
    let reader = &mut BufReader::new(fs::File::open(path).unwrap());
    let options = ParseOptions::new();
    match format {
        WritableFormat::Ape => ApeFile::read_from(reader, options).unwrap().remove_ape(),
        WritableFormat::Mpc => MpcFile::read_from(reader, options).unwrap().remove_ape(),
        WritableFormat::WavPack => WavPackFile::read_from(reader, options)
            .unwrap()
            .remove_ape(),
        _ => unreachable!("{format:?} keeps no pictures in APE items"),
    }
}

/// Adds genre "Keep Genre" and a PNG front cover to the primary tag.
fn add_genre_and_cover(path: &Path, format: WritableFormat) {
    if uses_ape_pictures(format) {
        let mut tag = ape_tag(path, format).unwrap_or_default();
        tag.set_genre("Keep Genre".to_owned());
        let cover = png();
        let key = cover.pic_type().as_ape_key().unwrap().to_owned();
        tag.insert(ApeItem::new(key, ItemValue::Binary(cover.as_ape_bytes())).unwrap());
        tag.save_to_path(path, WriteOptions::default()).unwrap();
        return;
    }
    let file = fs::File::open(path).unwrap();
    let mut tagged = Probe::new(BufReader::new(file))
        .guess_file_type()
        .unwrap()
        .read()
        .unwrap();
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(tagged.primary_tag_type()));
    }
    let tag = tagged.primary_tag_mut().unwrap();
    tag.set_genre("Keep Genre".to_owned());
    tag.push_picture(png());
    tagged.save_to_path(path, WriteOptions::default()).unwrap();
}

/// The pictures in the file's primary tag.
fn pictures(path: &Path, format: WritableFormat) -> Vec<Picture> {
    if uses_ape_pictures(format) {
        let Some(tag) = ape_tag(path, format) else {
            return Vec::new();
        };
        return tag
            .into_iter()
            .filter(|item| APE_PICTURE_TYPES.contains(&item.key()))
            .map(|item| {
                let ItemValue::Binary(bytes) = item.value() else {
                    panic!("{} is not binary", item.key());
                };
                Picture::from_ape_bytes(item.key(), bytes).unwrap()
            })
            .collect();
    }
    let tagged = Probe::new(BufReader::new(fs::File::open(path).unwrap()))
        .guess_file_type()
        .unwrap()
        .read()
        .unwrap();
    tagged
        .primary_tag()
        .map(|tag| tag.pictures().to_vec())
        .unwrap_or_default()
}

fn set_all(values: &BTreeMap<TagField, String>) -> TagEdit {
    TagEdit {
        set: values
            .iter()
            .map(|(field, value)| (*field, Some(value.clone())))
            .collect(),
        restore: Vec::new(),
    }
}

/// Copies every writable fixture into `dir`, adds a genre and a cover, then
/// writes the round-trip values minus the format's deny list.
fn write_fixtures(dir: &Path) -> Vec<Written> {
    let (files, _) = manifest();
    let values = round_trip_values();
    fs::create_dir(dir).unwrap();
    let mut written = Vec::new();
    for fixture in files {
        let source = fixture_root().join(&fixture.file);
        let Some(format) = writable_format(&source).unwrap() else {
            continue;
        };
        let path = dir.join(Path::new(&fixture.file).file_name().unwrap());
        fs::copy(&source, &path).unwrap();
        add_genre_and_cover(&path, format);
        let denied = unwritable_fields(format);
        let values: BTreeMap<TagField, String> = values
            .iter()
            .filter(|(field, _)| !denied.contains(field))
            .map(|(field, value)| (*field, value.clone()))
            .collect();
        edit_tags(&path, &set_all(&values))
            .unwrap_or_else(|error| panic!("{}: {error:#}", fixture.file));
        written.push(Written {
            fixture,
            format,
            path,
            values,
        });
    }
    written
}

async fn library_tracks(library: &Library) -> Vec<Track> {
    let reply = library
        .handle()
        .request(LibraryCommand::Tracks {
            album_id: None,
            page: PageRequest {
                revision: None,
                offset: 0,
                limit: 500,
                query: String::new(),
            },
        })
        .await
        .unwrap();
    let LibraryReply::Tracks(page) = reply else {
        panic!("unexpected reply {reply:?}");
    };
    assert_eq!(page.total, page.entries.len() as u64);
    page.entries
}

/// The library's value for a field, or `None` when the library does not read
/// that field. Dates compare at year precision; the original date is not read.
fn library_value(track: &Track, field: TagField) -> Option<Option<String>> {
    let metadata = &track.metadata;
    let number = |value: Option<u32>| value.map(|number| number.to_string());
    Some(match field {
        TagField::Title => Some(metadata.title.clone()),
        TagField::Artist => Some(metadata.artist.clone()),
        TagField::Album => Some(metadata.album.clone()),
        TagField::AlbumArtist => metadata.album_artist_tag.clone(),
        TagField::TrackNumber => number(metadata.track_number),
        TagField::TrackTotal => number(metadata.track_total),
        TagField::DiscNumber => number(metadata.disc_number),
        TagField::DiscTotal => number(metadata.disc_total),
        TagField::Date => metadata.year.map(|year| year.to_string()),
        TagField::OriginalDate => return None,
        TagField::MusicbrainzRecordingId => metadata.musicbrainz_recording_id.clone(),
        TagField::MusicbrainzTrackId => metadata.musicbrainz_track_id.clone(),
        TagField::MusicbrainzReleaseId => metadata.musicbrainz_release_id.clone(),
        TagField::MusicbrainzReleaseGroupId => metadata.musicbrainz_release_group_id.clone(),
        TagField::MusicbrainzArtistId => metadata.musicbrainz_artist_id.clone(),
        TagField::MusicbrainzAlbumArtistId => metadata.musicbrainz_album_artist_id.clone(),
    })
}

/// The value the library should read for a written value.
fn library_expected(field: TagField, value: &str) -> String {
    match field {
        TagField::Date => value[..4].to_owned(),
        _ => value.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_writable_fixture_round_trips_every_field() {
    let scratch = Scratch::new();
    let written = write_fixtures(&scratch.0.join("written"));
    let (files, _) = manifest();
    let writable = files
        .iter()
        .filter(|fixture| expected_format(&fixture.file).is_some())
        .count();
    assert_eq!(written.len(), writable);
    let mut failures = Vec::new();
    for file in &written {
        let name = &file.fixture.file;
        let tags = read_tags(&file.path).unwrap();
        if tags.primary != file.values {
            failures.push(format!(
                "{name}: lofty reads {:?}, wrote {:?}",
                tags.primary, file.values
            ));
        }
        let tagged = Probe::new(BufReader::new(fs::File::open(&file.path).unwrap()))
            .guess_file_type()
            .unwrap()
            .read()
            .unwrap();
        let genre = tagged.primary_tag().and_then(|tag| tag.genre());
        if genre.as_deref() != Some("Keep Genre") {
            failures.push(format!("{name}: genre is {genre:?}"));
        }
        // MP4 `covr` atoms carry no picture type.
        let pictures = pictures(&file.path, file.format);
        let cover = pictures.first().filter(|picture| {
            pictures.len() == 1
                && picture.data() == png().data()
                && (file.format == WritableFormat::Mp4
                    || picture.pic_type() == PictureType::CoverFront)
        });
        if cover.is_none() {
            failures.push(format!("{name}: pictures {pictures:?}"));
        }
        let mut decoder = Decoder::open(&file.path, GainMode::Off).unwrap();
        let frames = decode_to_eof(&mut decoder);
        if frames != file.fixture.frames {
            failures.push(format!(
                "{name}: decodes {frames} frames, the manifest records {}",
                file.fixture.frames
            ));
        }
    }

    // Every deny-listed field must really fail to read back, through lofty or
    // through the library, or it does not belong in the deny list.
    let values = round_trip_values();
    let denied_dir = scratch.0.join("denied");
    fs::create_dir(&denied_dir).unwrap();
    let mut denied = Vec::new();
    for file in written
        .iter()
        .filter(|file| !unwritable_fields(file.format).is_empty())
    {
        let path = denied_dir.join(file.path.file_name().unwrap());
        fs::copy(fixture_root().join(&file.fixture.file), &path).unwrap();
        edit_tags(&path, &set_all(&values)).unwrap();
        denied.push((file, path));
    }
    if !denied.is_empty() {
        let (library, _) = scan(&scratch, &denied_dir).await;
        let tracks = library_tracks(&library).await;
        library.shutdown().unwrap();
        for (file, path) in &denied {
            let tags = read_tags(path).unwrap();
            let track = tracks
                .iter()
                .find(|track| Path::new(&track.path) == path)
                .unwrap();
            for field in unwritable_fields(file.format) {
                let value = &values[field];
                let lofty_equal = tags.primary.get(field) == Some(value);
                let library_equal = library_value(track, *field)
                    .is_none_or(|read| read.as_deref() == Some(&*library_expected(*field, value)));
                if lofty_equal && library_equal {
                    failures.push(format!(
                        "{}: {field:?} round-trips, so it is not unwritable",
                        file.fixture.file
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn library_reads_written_fields() {
    let scratch = Scratch::new();
    let library_dir = scratch.0.join("library");
    let written = write_fixtures(&library_dir);
    let (library, state) = scan(&scratch, &library_dir).await;
    assert_eq!(state.scan.failures, 0, "scan: {:?}", state.scan);
    let tracks = library_tracks(&library).await;
    library.shutdown().unwrap();
    assert_eq!(tracks.len(), written.len());
    let mut failures = Vec::new();
    for file in &written {
        let name = &file.fixture.file;
        let track = tracks
            .iter()
            .find(|track| Path::new(&track.path) == file.path)
            .unwrap_or_else(|| panic!("{name} is not in the library"));
        for (field, value) in &file.values {
            let Some(read) = library_value(track, *field) else {
                continue;
            };
            let expected = library_expected(*field, value);
            if read.as_deref() != Some(&*expected) {
                failures.push(format!(
                    "{name}: the library reads {field:?} as {read:?}, expected {expected:?}"
                ));
            }
        }
        if track.metadata.genres != ["Keep Genre"] {
            failures.push(format!("{name}: genres {:?}", track.metadata.genres));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
