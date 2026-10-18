//! Native, read-only metadata probing; never launches a process or decodes a track.
//!
//! Stream tags override container tags. Display strings remain verbatim UTF-8;
//! invalid UTF-8 and excessive tags fail the probe rather than silently changing
//! identity. FFmpeg probing is limited to 2 MiB, 256 packets and 2 s of analysis,
//! with a 10 s wall-clock interrupt deadline. Blocking OS filesystem calls cannot
//! be forcibly interrupted. These limits intentionally reject some unusual media.
use std::{
    ffi::CStr,
    path::Path,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use ffmpeg_next as ffmpeg;

use crate::{
    decoder::{GainMetadata, initialize_ffmpeg},
    library_types::TrackMetadata,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TAGS: usize = 1_024;
const MAX_TAG_BYTES: usize = 256 * 1_024;
const MAX_VALUE_BYTES: usize = 64 * 1_024;

pub(crate) fn probe(path: &Path, cancelled: &Arc<AtomicBool>) -> Result<TrackMetadata> {
    probe_before(path, cancelled, Instant::now() + PROBE_TIMEOUT)
}

/// Artwork shares one discovery deadline across media probing and direct-path
/// release metadata, rather than starting another ten-second operation.
pub(crate) fn probe_before(
    path: &Path,
    cancelled: &Arc<AtomicBool>,
    deadline: Instant,
) -> Result<TrackMetadata> {
    ensure!(
        Instant::now() < deadline,
        "metadata probe deadline exceeded"
    );
    ensure!(
        path.is_absolute(),
        "metadata source must be an absolute path"
    );
    let path_text = path.to_str().context("metadata path is not UTF-8")?;
    ensure!(
        !path_text.as_bytes().contains(&0),
        "metadata path contains NUL"
    );
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "metadata probe cancelled"
    );
    initialize_ffmpeg()?;
    let interrupt = Arc::clone(cancelled);
    let mut options = ffmpeg::Dictionary::new();
    options.set("probesize", "2097152");
    options.set("analyzeduration", "2000000");
    options.set("max_probe_packets", "256");
    options.set("max_streams", "64");
    options.set("skip_estimate_duration_from_pts", "1");
    options.set("protocol_whitelist", "file");
    options.set("err_detect", "crccheck+bitstream+buffer+explode");
    let input = ffmpeg::format::input_with_interrupt_and_dictionary(
        path,
        move || interrupt.load(Ordering::Acquire) || Instant::now() >= deadline,
        options,
    )
    .with_context(|| format!("probing {}", path.display()))?;
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "metadata probe cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "metadata probe deadline exceeded"
    );
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .context("source has no audio stream")?;
    let parameters = stream.parameters();
    // Codec parameters already belong to the selected stream. Opening another
    // decoder or reading PCM solely to learn these fields would be unnecessary.
    let parameters = unsafe { &*parameters.as_ptr() };
    let sample_rate = u32::try_from(parameters.sample_rate)
        .ok()
        .filter(|value| *value != 0)
        .context("invalid audio sample rate")?;
    let channels = u16::try_from(parameters.ch_layout.nb_channels)
        .ok()
        .filter(|value| *value != 0)
        .context("invalid audio channel count")?;
    ensure!(
        parameters.codec_id != ffmpeg::ffi::AVCodecID::AV_CODEC_ID_NONE,
        "unknown audio codec"
    );
    let mut tags = Tags::default();
    tags.read(&input, &stream)?;
    let time_base = stream.time_base();
    let duration_ms = duration_ms(stream.duration(), time_base.0, time_base.1)
        .or_else(|| duration_ms(input.duration(), 1, ffmpeg::ffi::AV_TIME_BASE));
    let mut metadata = tags.finish(path)?;
    metadata.duration_ms = duration_ms;
    metadata.sample_rate = sample_rate;
    metadata.channels = channels;
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "metadata probe cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "metadata probe deadline exceeded"
    );
    Ok(metadata)
}

pub(crate) struct PlaybackMetadata {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) metadata_error: Option<String>,
}

/// Reuse the decoder's already-open input and the library's bounded tag parser.
/// Display failures are warnings, never a reason to discard valid audio.
pub(crate) fn playback_metadata(
    input: &ffmpeg::format::context::Input,
    stream: &ffmpeg::Stream<'_>,
    path: &Path,
) -> (PlaybackMetadata, GainMetadata) {
    let mut tags = Tags::default();
    let parsed = tags
        .read(input, stream)
        .and_then(|()| tags.finish_display(path));
    let gain = tags.gain;
    let (metadata, metadata_error) = match parsed {
        Ok(()) => (tags.metadata, None),
        Err(error) => (
            TrackMetadata {
                title: path
                    .file_stem()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned(),
                artist: "Unknown Artist".to_owned(),
                album: "Unknown Album".to_owned(),
                ..TrackMetadata::default()
            },
            Some(format!("{error:#}")),
        ),
    };
    (
        PlaybackMetadata {
            title: metadata.title,
            artist: metadata.artist,
            album: metadata.album,
            metadata_error,
        },
        gain,
    )
}

fn duration_ms(duration: i64, numerator: i32, denominator: i32) -> Option<u64> {
    if duration < 0 || numerator <= 0 || denominator <= 0 {
        return None;
    }
    u64::try_from(i128::from(duration) * i128::from(numerator) * 1_000 / i128::from(denominator))
        .ok()
}

#[derive(Default)]
struct TagBudget {
    count: usize,
    bytes: usize,
}

// ffmpeg-next's Dictionary iterator uses from_utf8_unchecked. Validate the
// native strings ourselves before constructing any Rust str, including keys.
fn visit_tags(
    dictionary: &ffmpeg::DictionaryRef<'_>,
    budget: &mut TagBudget,
    mut visit: impl FnMut(&str, &str),
) -> Result<()> {
    let mut entry = ptr::null_mut();
    loop {
        entry = unsafe {
            ffmpeg::ffi::av_dict_get(
                dictionary.as_ptr(),
                c"".as_ptr(),
                entry,
                ffmpeg::ffi::AV_DICT_IGNORE_SUFFIX,
            )
        };
        if entry.is_null() {
            return Ok(());
        }
        let (key, value) = unsafe {
            (
                CStr::from_ptr((*entry).key).to_bytes(),
                CStr::from_ptr((*entry).value).to_bytes(),
            )
        };
        budget.count += 1;
        budget.bytes = budget
            .bytes
            .saturating_add(key.len())
            .saturating_add(value.len());
        ensure!(
            budget.count <= MAX_TAGS
                && budget.bytes <= MAX_TAG_BYTES
                && key.len() <= MAX_VALUE_BYTES
                && value.len() <= MAX_VALUE_BYTES,
            "metadata exceeds tag limits (1024 tags, 256 KiB total, 64 KiB per value)"
        );
        visit(
            std::str::from_utf8(key).context("metadata key is not UTF-8")?,
            std::str::from_utf8(value).context("metadata value is not UTF-8")?,
        );
    }
}

#[derive(Default)]
struct Tags {
    metadata: TrackMetadata,
    gain: GainMetadata,
}

impl Tags {
    fn read(
        &mut self,
        input: &ffmpeg::format::context::Input,
        stream: &ffmpeg::Stream<'_>,
    ) -> Result<()> {
        let mut budget = TagBudget::default();
        self.dictionary(&input.metadata(), &mut budget)?;
        self.dictionary(&stream.metadata(), &mut budget)?;
        for side in stream.side_data() {
            if side.kind() == ffmpeg::packet::side_data::Type::ReplayGain {
                self.gain.side_data(side.data())?;
            }
        }
        Ok(())
    }

    fn dictionary(
        &mut self,
        dictionary: &ffmpeg::DictionaryRef<'_>,
        budget: &mut TagBudget,
    ) -> Result<()> {
        // Explicit totals win over slash totals within each dictionary, without
        // depending on native dictionary insertion order. Stream totals still
        // override container totals.
        let mut track_total = None;
        let mut disc_total = None;
        visit_tags(dictionary, budget, |key, value| {
            if key_is(key, "tracktotal") || key_is(key, "totaltracks") {
                track_total = positive_number(value).or(track_total);
            } else if key_is(key, "disctotal") || key_is(key, "totaldiscs") {
                disc_total = positive_number(value).or(disc_total);
            } else {
                self.tag(key, value);
            }
        })?;
        if let Some(total) = track_total {
            self.metadata.track_total = Some(total);
        }
        if let Some(total) = disc_total {
            self.metadata.disc_total = Some(total);
        }
        Ok(())
    }

    fn tag(&mut self, key: &str, value: &str) {
        if value.trim().is_empty() {
            return;
        }
        self.gain.tag(key, value);
        let metadata = &mut self.metadata;
        let text = if key_is(key, "title") {
            Some(&mut metadata.title)
        } else if key_is(key, "artist") {
            Some(&mut metadata.artist)
        } else if key_is(key, "albumartist") {
            Some(&mut metadata.album_artist)
        } else if key_is(key, "album") {
            Some(&mut metadata.album)
        } else {
            None
        };
        if let Some(text) = text {
            text.clear();
            text.push_str(value);
            return;
        }
        if key_is(key, "track") || key_is(key, "tracknumber") {
            let (number, total) = number_pair(value);
            if number.is_some() {
                metadata.track_number = number;
            }
            if total.is_some() {
                metadata.track_total = total;
            }
        } else if key_is(key, "disc") || key_is(key, "discnumber") || key_is(key, "disk") {
            let (number, total) = number_pair(value);
            if number.is_some() {
                metadata.disc_number = number;
            }
            if total.is_some() {
                metadata.disc_total = total;
            }
        } else if key_is(key, "compilation") || key_is(key, "tcmp") {
            if let Some(compilation) = compilation(value) {
                metadata.compilation = compilation;
            }
        } else {
            let field = if key_is(key, "musicbrainzalbumid") || key_is(key, "musicbrainzreleaseid")
            {
                &mut metadata.musicbrainz_release_id
            } else if key_is(key, "musicbrainzreleasegroupid") {
                &mut metadata.musicbrainz_release_group_id
            } else if key_is(key, "musicbrainztrackid")
                || key_is(key, "musicbrainzrecordingid")
                || key_is(key, "ufid:http://musicbrainz.org")
            {
                // Picard's legacy MUSICBRAINZ_TRACKID identifies the recording;
                // MUSICBRAINZ_RELEASETRACKID identifies its release occurrence.
                &mut metadata.musicbrainz_recording_id
            } else if key_is(key, "musicbrainzreleasetrackid") {
                &mut metadata.musicbrainz_track_id
            } else if key_is(key, "musicbrainzartistid") {
                &mut metadata.musicbrainz_artist_id
            } else if key_is(key, "musicbrainzalbumartistid") {
                &mut metadata.musicbrainz_album_artist_id
            } else {
                return;
            };
            *field = Some(value.to_owned());
        }
    }

    fn finish_display(&mut self, path: &Path) -> Result<()> {
        if self.metadata.title.is_empty() {
            self.metadata.title = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .context("audio filename has no UTF-8 stem")?
                .to_owned();
        }
        if self.metadata.artist.is_empty() {
            self.metadata.artist = "Unknown Artist".to_owned();
        }
        if self.metadata.album.is_empty() {
            self.metadata.album = "Unknown Album".to_owned();
        }
        Ok(())
    }

    fn finish(mut self, path: &Path) -> Result<TrackMetadata> {
        self.finish_display(path)?;
        if self.metadata.album_artist.is_empty() {
            self.metadata.album_artist = if self.metadata.compilation {
                "Various Artists".to_owned()
            } else {
                self.metadata.artist.clone()
            };
        }
        self.metadata.track_gain_db = self.gain.track_gain;
        self.metadata.album_gain_db = self.gain.album_gain;
        self.metadata.track_peak = self.gain.track_peak;
        self.metadata.album_peak = self.gain.album_peak;
        Ok(self.metadata)
    }
}

fn key_is(key: &str, normalized: &str) -> bool {
    key.bytes()
        .filter(|byte| !matches!(byte, b' ' | b'_' | b'-'))
        .map(|byte| byte.to_ascii_lowercase())
        .eq(normalized.bytes())
}

fn positive_number(value: &str) -> Option<u32> {
    value.trim().parse().ok().filter(|number| *number > 0)
}

fn number_pair(value: &str) -> (Option<u32>, Option<u32>) {
    match value.split_once('/') {
        Some((number, total)) => (positive_number(number), positive_number(total)),
        None => (positive_number(value), None),
    }
}

fn compilation(value: &str) -> Option<bool> {
    match value.trim() {
        "1" => Some(true),
        "0" => Some(false),
        value if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") => {
            Some(true)
        }
        value if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no") => {
            Some(false)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_display_tags_and_stream_overrides_are_preserved() {
        let mut tags = Tags::default();
        tags.tag("TITLE", "Original");
        tags.tag("title", "  夜の音楽  ");
        tags.tag("artist", "Björk");
        tags.tag("album", "Ágætis byrjun");
        tags.tag("title", "  ");
        let metadata = tags.finish(Path::new("/music/音楽.flac")).unwrap();
        assert_eq!(metadata.title, "  夜の音楽  ");
        assert_eq!(metadata.artist, "Björk");
        assert_eq!(metadata.album_artist, "Björk");
        assert_eq!(metadata.album, "Ágætis byrjun");
    }

    #[test]
    fn fallback_and_compilation_do_not_replace_track_artist() {
        let metadata = Tags::default().finish(Path::new("/music/夜.flac")).unwrap();
        assert_eq!(metadata.title, "夜");
        assert_eq!(metadata.artist, "Unknown Artist");
        assert_eq!(metadata.album, "Unknown Album");
        let mut tags = Tags::default();
        tags.tag("artist", "Performer");
        tags.tag("compilation", "yes");
        let metadata = tags.finish(Path::new("/music/track.wav")).unwrap();
        assert_eq!(metadata.artist, "Performer");
        assert_eq!(metadata.album_artist, "Various Artists");
        assert!(metadata.compilation);
    }

    #[test]
    fn explicit_totals_and_compilation_artist_win() {
        let mut dictionary = ffmpeg::Dictionary::new();
        dictionary.set("TOTALTRACKS", "15");
        dictionary.set("track", "03/12");
        dictionary.set("disc", "2/3");
        dictionary.set("album_artist", "Release Artist");
        dictionary.set("compilation", "1");
        let mut tags = Tags::default();
        tags.dictionary(&dictionary, &mut TagBudget::default())
            .unwrap();
        let metadata = tags.finish(Path::new("/music/track.mp3")).unwrap();
        assert_eq!(metadata.track_number, Some(3));
        assert_eq!(metadata.track_total, Some(15));
        assert_eq!(metadata.disc_number, Some(2));
        assert_eq!(metadata.disc_total, Some(3));
        assert_eq!(metadata.album_artist, "Release Artist");
    }

    #[test]
    fn identifier_mapping_and_gain_preserve_valid_values() {
        let mut tags = Tags::default();
        tags.tag("MusicBrainz Album Id", "Release-ID");
        tags.tag("MUSICBRAINZ_TRACKID", "Recording-ID");
        tags.tag("MUSICBRAINZ_RELEASETRACKID", "Track-ID");
        tags.tag("MusicBrainz Release Group Id", "Group-ID");
        tags.tag("MUSICBRAINZ_ARTISTID", "Artist-ID");
        tags.tag("MUSICBRAINZ_ALBUMARTISTID", "Album-Artist-ID");
        tags.tag("REPLAYGAIN_TRACK_GAIN", "-3.25 dB");
        tags.tag("replaygain_track_gain", "NaN");
        tags.tag("replaygain_album_peak", "0.95");
        let metadata = tags.finish(Path::new("/music/track.ogg")).unwrap();
        assert_eq!(
            metadata.musicbrainz_release_id.as_deref(),
            Some("Release-ID")
        );
        assert_eq!(
            metadata.musicbrainz_recording_id.as_deref(),
            Some("Recording-ID")
        );
        assert_eq!(metadata.musicbrainz_track_id.as_deref(), Some("Track-ID"));
        assert_eq!(
            metadata.musicbrainz_release_group_id.as_deref(),
            Some("Group-ID")
        );
        assert_eq!(metadata.musicbrainz_artist_id.as_deref(), Some("Artist-ID"));
        assert_eq!(
            metadata.musicbrainz_album_artist_id.as_deref(),
            Some("Album-Artist-ID")
        );
        assert_eq!(metadata.track_gain_db, Some(-3.25));
        assert_eq!(metadata.album_peak, Some(0.95));
    }

    #[test]
    fn malformed_numeric_values_do_not_become_numbers() {
        for value in ["0", "-1", "4294967296", "one", "1.5"] {
            assert_eq!(positive_number(value), None);
        }
        assert_eq!(number_pair(" 03 / 12 "), (Some(3), Some(12)));
        assert_eq!(number_pair("2/garbage"), (Some(2), None));
        assert_eq!(compilation("unknown"), None);
        assert_eq!(duration_ms(ffmpeg::ffi::AV_NOPTS_VALUE, 1, 48_000), None);
        assert_eq!(duration_ms(96_000, 1, 48_000), Some(2_000));
        assert_eq!(duration_ms(i64::MAX, i32::MAX, 1), None);
    }

    #[test]
    fn invalid_utf8_is_rejected_without_unchecked_str_creation() {
        let mut raw_dictionary = ptr::null_mut();
        let dictionary = unsafe {
            assert_eq!(
                ffmpeg::ffi::av_dict_set(
                    &mut raw_dictionary,
                    c"title".as_ptr(),
                    c"\xff".as_ptr(),
                    0
                ),
                0
            );
            ffmpeg::Dictionary::own(raw_dictionary)
        };
        let result = Tags::default().dictionary(&dictionary, &mut TagBudget::default());
        assert!(result.unwrap_err().to_string().contains("not UTF-8"));
    }

    #[test]
    fn excessive_metadata_fails_instead_of_truncating_display_values() {
        let mut dictionary = ffmpeg::Dictionary::new();
        dictionary.set("title", &"x".repeat(MAX_VALUE_BYTES + 1));
        let result = Tags::default().dictionary(&dictionary, &mut TagBudget::default());
        assert!(result.unwrap_err().to_string().contains("tag limits"));
    }

    #[test]
    fn replaygain_side_data_only_fills_missing_tags() {
        let mut tags = Tags::default();
        tags.tag("replaygain_track_gain", "-2 dB");
        let raw = ffmpeg::ffi::AVReplayGain {
            track_gain: -325_000,
            album_gain: -450_000,
            track_peak: 50_000,
            album_peak: 100_000,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&raw as *const ffmpeg::ffi::AVReplayGain).cast::<u8>(),
                std::mem::size_of_val(&raw),
            )
        };
        tags.gain.side_data(bytes).unwrap();
        let metadata = tags.finish(Path::new("/music/gain.flac")).unwrap();
        assert_eq!(metadata.track_gain_db, Some(-2.0));
        assert_eq!(metadata.album_gain_db, Some(-4.5));
        assert_eq!(metadata.track_peak, Some(0.5));
        assert_eq!(metadata.album_peak, Some(1.0));
    }
}
