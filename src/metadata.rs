//! Native, read-only metadata probing; never launches a process or decodes a track.
//!
//! Stream tags override container tags. Display strings remain verbatim UTF-8
//! except trimmed album artists and normalized VA aliases;
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
    library_types::{TrackMetadata, is_lossless},
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TAGS: usize = 1_024;
const MAX_TAG_BYTES: usize = 256 * 1_024;
const MAX_VALUE_BYTES: usize = 64 * 1_024;
/// Genres kept per track; further genres are ignored.
const MAX_GENRES: usize = 32;
/// Individual genre values longer than this are skipped.
const MAX_GENRE_BYTES: usize = 256;

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
    // Match local playback's duration across all tracker subsongs.
    options.set("subsong", "all");
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
    let codec = codec_label(
        ffmpeg::codec::Id::from(parameters.codec_id),
        input.format().name(),
    );
    metadata.codec = Some(codec.to_owned());
    if is_lossless(codec) {
        metadata.bit_depth = [
            parameters.bits_per_raw_sample,
            parameters.bits_per_coded_sample,
        ]
        .into_iter()
        .find_map(|bits| u16::try_from(bits).ok().filter(|bits| *bits != 0));
    }
    metadata.bitrate_kbps = [parameters.bit_rate, input.bit_rate()]
        .into_iter()
        .find(|rate| *rate > 0)
        .and_then(|rate| u32::try_from((rate + 500) / 1_000).ok())
        .filter(|kbps| *kbps != 0);
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
    /// Index into `YEAR_KEYS` of the tag that supplied `metadata.year`.
    year_rank: Option<usize>,
    /// Lowercased genres already held, for case-insensitive dedupe.
    genre_keys: std::collections::HashSet<String>,
}

const YEAR_KEYS: [&str; 4] = ["date", "year", "originaldate", "originalyear"];

/// First run of exactly four ASCII digits in 1000..=2999.
pub(crate) fn year_from(value: &str) -> Option<i32> {
    let bytes = value.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_digit() {
            start += 1;
            continue;
        }
        let end = bytes[start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .map_or(bytes.len(), |length| start + length);
        if end - start == 4 {
            let year: i32 = value[start..end].parse().ok()?;
            if (1000..=2999).contains(&year) {
                return Some(year);
            }
        }
        start = end;
    }
    None
}

/// Splits a genre value on `;` and NUL, trimming and dropping empty parts.
pub(crate) fn split_genres(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([';', '\0'])
        .map(str::trim)
        .filter(|genre| !genre.is_empty())
}

pub(crate) fn codec_label(codec: ffmpeg::codec::Id, container: &str) -> &'static str {
    use ffmpeg::codec::Id;
    if container == "libopenmpt" {
        return "Tracker";
    }
    match codec {
        Id::FLAC => "FLAC",
        Id::MP3 => "MP3",
        Id::AAC => "AAC",
        Id::ALAC => "ALAC",
        Id::OPUS => "Opus",
        Id::VORBIS => "Vorbis",
        Id::WAVPACK => "WavPack",
        Id::APE => "APE",
        Id::MUSEPACK7 | Id::MUSEPACK8 => "Musepack",
        other if other.name().starts_with("pcm_") => {
            let mut formats = container.split(',');
            if formats.clone().any(|format| format == "wav") {
                "WAV"
            } else if formats.any(|format| format == "aiff") {
                "AIFF"
            } else {
                "PCM"
            }
        }
        _ => "Other",
    }
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
        if key_is(key, "albumartist") {
            metadata.album_artist_tag = Some(value.trim().to_owned());
            return;
        }
        if let Some(rank) = YEAR_KEYS.iter().position(|name| key_is(key, name)) {
            if self.year_rank.is_none_or(|current| rank <= current)
                && let Some(year) = year_from(value)
            {
                metadata.year = Some(year);
                self.year_rank = Some(rank);
            }
        } else if key_is(key, "genre") {
            for genre in split_genres(value) {
                if metadata.genres.len() >= MAX_GENRES {
                    break;
                }
                if genre.len() <= MAX_GENRE_BYTES && self.genre_keys.insert(genre.to_lowercase()) {
                    metadata.genres.push(genre.to_owned());
                }
            }
        } else if key_is(key, "track") || key_is(key, "tracknumber") {
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
                metadata.compilation_tag = Some(compilation);
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
        let various_artists = self
            .metadata
            .album_artist_tag
            .as_deref()
            .is_some_and(|artist| {
                artist.eq_ignore_ascii_case("Various Artists") || artist.eq_ignore_ascii_case("VA")
            });
        self.metadata.compilation = self.metadata.compilation_tag.unwrap_or(various_artists);
        self.metadata.album_artist = match self.metadata.album_artist_tag.as_deref() {
            Some(_) if various_artists => "Various Artists".to_owned(),
            Some(artist) => artist.to_owned(),
            None if self.metadata.compilation => "Various Artists".to_owned(),
            None => self.metadata.artist.clone(),
        };
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
        assert_eq!(metadata.album_artist_tag, None);
        assert_eq!(metadata.compilation_tag, None);
        assert!(!metadata.compilation);
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
        assert_eq!(metadata.album_artist_tag, None);
        assert_eq!(metadata.compilation_tag, Some(true));
        assert!(metadata.compilation);
    }

    #[test]
    fn explicit_totals_and_compilation_artist_win() {
        let mut dictionary = ffmpeg::Dictionary::new();
        dictionary.set("TOTALTRACKS", "15");
        dictionary.set("track", "03/12");
        dictionary.set("disc", "2/3");
        dictionary.set("album_artist", "  Release Artist  ");
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
        assert_eq!(metadata.album_artist_tag.as_deref(), Some("Release Artist"));
        assert_eq!(metadata.compilation_tag, Some(true));
        assert!(metadata.compilation);
    }

    #[test]
    fn various_artist_aliases_preserve_provenance_and_respect_explicit_flags() {
        for artist in ["Various Artists", "  vArIoUs ArTiStS\t", "VA", " va "] {
            for flag in [None, Some("false"), Some("true")] {
                let mut tags = Tags::default();
                tags.tag("artist", "  Performer  ");
                tags.tag("ALBUM ARTIST", artist);
                if let Some(flag) = flag {
                    tags.tag("TCMP", flag);
                }
                let metadata = tags.finish(Path::new("/music/track.flac")).unwrap();
                assert_eq!(metadata.artist, "  Performer  ");
                assert_eq!(metadata.album_artist, "Various Artists");
                assert_eq!(metadata.album_artist_tag.as_deref(), Some(artist.trim()));
                assert_eq!(metadata.compilation_tag, flag.and_then(compilation));
                assert_eq!(metadata.compilation, flag != Some("false"));
            }
        }
    }

    #[test]
    fn explicit_artist_equal_to_fallback_remains_distinguishable() {
        let mut tags = Tags::default();
        tags.tag("artist", "Performer");
        tags.tag("album_artist", "Performer");
        tags.tag("compilation", "0");
        let metadata = tags.finish(Path::new("/music/track.flac")).unwrap();
        assert_eq!(metadata.artist, "Performer");
        assert_eq!(metadata.album_artist, "Performer");
        assert_eq!(metadata.album_artist_tag.as_deref(), Some("Performer"));
        assert_eq!(metadata.compilation_tag, Some(false));
        assert!(!metadata.compilation);
    }

    #[test]
    fn stream_provenance_overrides_container_without_accepting_invalid_values() {
        let mut container = ffmpeg::Dictionary::new();
        container.set("album_artist", "VA");
        container.set("compilation", "yes");
        let mut stream = ffmpeg::Dictionary::new();
        stream.set("ALBUMARTIST", "  Release Artist  ");
        stream.set("TCMP", "no");
        let mut tags = Tags::default();
        let mut budget = TagBudget::default();
        tags.dictionary(&container, &mut budget).unwrap();
        tags.dictionary(&stream, &mut budget).unwrap();
        tags.tag("album_artist", " \t");
        tags.tag("compilation", "invalid");
        let metadata = tags.finish(Path::new("/music/track.flac")).unwrap();
        assert_eq!(metadata.album_artist_tag.as_deref(), Some("Release Artist"));
        assert_eq!(metadata.album_artist, "Release Artist");
        assert_eq!(metadata.compilation_tag, Some(false));
        assert!(!metadata.compilation);

        let mut tags = Tags::default();
        tags.tag("artist", "Performer");
        tags.tag("album_artist", " \t");
        tags.tag("compilation", "invalid");
        let metadata = tags.finish(Path::new("/music/track.flac")).unwrap();
        assert_eq!(metadata.album_artist_tag, None);
        assert_eq!(metadata.compilation_tag, None);
        assert_eq!(metadata.album_artist, "Performer");
        assert!(!metadata.compilation);
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

    #[test]
    fn year_tags_parse_dates_and_reject_garbage() {
        assert_eq!(year_from("1959-08-17"), Some(1959));
        assert_eq!(year_from("1971"), Some(1971));
        assert_eq!(year_from("c. 1985?"), Some(1985));
        assert_eq!(year_from("85"), None);
        assert_eq!(year_from("0000"), None);
        let mut tags = Tags::default();
        tags.tag("originaldate", "1950");
        tags.tag("date", "garbage");
        assert_eq!(tags.metadata.year, Some(1950));
        tags.tag("year", "1960");
        assert_eq!(tags.metadata.year, Some(1960));
    }

    #[test]
    fn genre_values_split_dedupe_and_keep_first_spelling() {
        let mut tags = Tags::default();
        tags.tag("genre", "Jazz; Modal");
        tags.tag("GENRE", "jazz");
        tags.tag("genre", "Bop\0 ");
        assert_eq!(tags.metadata.genres, ["Jazz", "Modal", "Bop"]);
    }

    #[test]
    fn codec_labels_follow_codec_and_container() {
        use ffmpeg::codec::Id;
        assert_eq!(codec_label(Id::FLAC, "flac"), "FLAC");
        assert_eq!(codec_label(Id::PCM_S16LE, "wav"), "WAV");
        assert_eq!(codec_label(Id::PCM_S24BE, "aiff"), "AIFF");
        assert_eq!(codec_label(Id::PCM_S16LE, "mov"), "PCM");
        assert_eq!(codec_label(Id::MP3, "mp3"), "MP3");
        assert_eq!(codec_label(Id::H264, "mp4"), "Other");
    }

    #[test]
    fn genre_count_and_length_are_capped() {
        let mut tags = Tags::default();
        let many: Vec<String> = (0..40).map(|n| format!("G{n}")).collect();
        tags.tag("genre", &many.join(";"));
        assert_eq!(tags.metadata.genres, many[..MAX_GENRES]);
        tags.tag("genre", "extra");
        assert_eq!(tags.metadata.genres.len(), MAX_GENRES);
        let mut tags = Tags::default();
        tags.tag(
            "genre",
            &format!("{};Kept", "x".repeat(MAX_GENRE_BYTES + 1)),
        );
        assert_eq!(tags.metadata.genres, ["Kept"]);
        tags.tag("genre", &"y".repeat(MAX_GENRE_BYTES));
        assert_eq!(tags.metadata.genres.len(), 2);
    }
}
