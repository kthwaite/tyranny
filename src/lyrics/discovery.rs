use super::{Cancellation, Lyrics, LyricsSource, LyricsStatus, MAX_TEXT_BYTES};
use anyhow::{Context, Result, ensure};
use ffmpeg_next as ffmpeg;
use serde::{Deserialize, Serialize};
use std::{ffi::CStr, path::Path, ptr, time::Instant};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Identity {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
}
impl Identity {
    pub(super) fn eligible(&self) -> bool {
        !self.title.trim().is_empty()
            && !self.artist.trim().is_empty()
            && [&self.title, &self.artist, &self.album]
                .iter()
                .all(|s| s.len() <= 4096)
            && self.duration.is_finite()
            && (1.0..=3600.0).contains(&self.duration)
    }
}
#[derive(Serialize, Deserialize)]
pub(super) struct Local {
    pub lyrics: Option<Lyrics>,
    pub identity: Identity,
}

pub(super) fn probe(path: &Path, cancel: &Cancellation, deadline: Instant) -> Result<Local> {
    cancel.check(deadline)?;
    ensure!(
        path.is_absolute() && path.to_str().is_some_and(|s| !s.as_bytes().contains(&0)),
        "lyrics source must be an absolute UTF-8 file path"
    );
    ensure!(
        std::fs::metadata(path)?.is_file(),
        "lyrics source must be a regular file"
    );
    crate::decoder::initialize_ffmpeg()?;
    let interrupt = cancel.clone();
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
        move || interrupt.cancelled() || Instant::now() >= deadline,
        options,
    )?;
    cancel.check(deadline)?;
    let audio = input
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .context("source has no audio stream")?;
    let mut tags = Tags::default();
    tags.read(&input.metadata())?;
    // Audio-stream tags override container tags, matching native metadata probing.
    tags.read(&audio.metadata())?;
    let base = audio.time_base();
    let duration = if audio.duration() > 0 && base.0 > 0 && base.1 > 0 {
        audio.duration() as f64 * f64::from(base.0) / f64::from(base.1)
    } else {
        input.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
    };
    cancel.check(deadline)?;
    Ok(Local {
        lyrics: tags.lyrics.map(embedded),
        identity: Identity {
            title: tags.title,
            artist: tags.artist,
            album: tags.album,
            duration,
        },
    })
}
#[derive(Default)]
struct Tags {
    title: String,
    artist: String,
    album: String,
    lyrics: Option<String>,
    count: usize,
    bytes: usize,
}
impl Tags {
    fn read(&mut self, dictionary: &ffmpeg::DictionaryRef<'_>) -> Result<()> {
        // Avoid ffmpeg-next's unchecked UTF-8 dictionary iterator.
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
            self.count += 1;
            self.bytes = self
                .bytes
                .saturating_add(key.len())
                .saturating_add(value.len());
            ensure!(
                self.count <= 1024
                    && self.bytes <= 256 * 1024
                    && key.len() <= 4096
                    && value.len() <= MAX_TEXT_BYTES,
                "lyrics metadata exceeds tag limits"
            );
            let key = std::str::from_utf8(key).context("metadata key is not UTF-8")?;
            let value = std::str::from_utf8(value).context("metadata value is not UTF-8")?;
            self.visit(key, value);
        }
    }
    fn visit(&mut self, key: &str, value: &str) {
        if key.eq_ignore_ascii_case("title") {
            self.title = value.to_owned();
        } else if key.eq_ignore_ascii_case("artist") {
            self.artist = value.to_owned();
        } else if key.eq_ignore_ascii_case("album") {
            self.album = value.to_owned();
        } else if (key.eq_ignore_ascii_case("lyrics")
            || key.eq_ignore_ascii_case("uslt")
            || key
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("lyrics-")))
            && !value.trim().is_empty()
        {
            self.lyrics = Some(value.to_owned());
        }
    }
}
pub(super) fn has_timestamps(text: &str) -> bool {
    text.lines().any(|line| {
        let mut remainder = line.trim_start();
        while let Some((tag, rest)) = remainder.strip_prefix('[').and_then(|s| s.split_once(']')) {
            if timestamp(tag).is_some() {
                return true;
            }
            remainder = rest;
        }
        false
    })
}
// Keep this grammar aligned with TyrannyLikesSmokeCore.LyricsTimeline.timestamp.
fn timestamp(tag: &str) -> Option<f64> {
    fn integer(value: &str) -> Option<f64> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        value.parse::<f64>().ok().filter(|value| value.is_finite())
    }
    let mut parts = tag.split(':');
    let first = parts.next()?;
    let second = parts.next()?;
    let third = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (hours, minutes, final_part) = if let Some(third) = third {
        let minutes = integer(second)?;
        if minutes >= 60.0 {
            return None;
        }
        (integer(first)?, minutes, third)
    } else {
        (0.0, integer(first)?, second)
    };
    let mut final_parts = final_part.split('.');
    let seconds = integer(final_parts.next()?)?;
    if seconds >= 60.0 {
        return None;
    }
    let fraction = if let Some(digits) = final_parts.next() {
        if digits.len() > 9 {
            return None;
        }
        integer(digits)? / 10.0_f64.powi(digits.len() as i32)
    } else {
        0.0
    };
    if final_parts.next().is_some() {
        return None;
    }
    let result = hours * 3600.0 + minutes * 60.0 + seconds + fraction;
    result.is_finite().then_some(result)
}
fn embedded(text: String) -> Lyrics {
    let (plain_text, synced_lrc) = if has_timestamps(&text) {
        (None, Some(text))
    } else {
        (Some(text), None)
    };
    Lyrics {
        status: LyricsStatus::Ready,
        source: Some(LyricsSource::Embedded),
        plain_text,
        synced_lrc,
        attribution_url: None,
        message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_uslt_vorbis_plain_and_lrc_without_guessing_identity() {
        let mut tags = Tags::default();
        tags.visit("LYRICS", "plain lyrics");
        assert_eq!(
            embedded(tags.lyrics.take().unwrap()).plain_text.as_deref(),
            Some("plain lyrics")
        );
        tags.visit("lyrics-eng", "[00:01.20]Sung line");
        assert!(embedded(tags.lyrics.take().unwrap()).synced_lrc.is_some());
        tags.visit("USLT", "another language");
        assert!(tags.lyrics.is_some());
        assert!(!has_timestamps("[ar:artist]\n[00:99]bad"));
        assert!(
            !Identity {
                title: String::new(),
                artist: "Artist".into(),
                album: String::new(),
                duration: 12.0
            }
            .eligible()
        );
    }
    #[test]
    fn timestamp_grammar_matches_client_and_malformed_lrc_remains_plain() {
        for text in [
            "[00:01]Words",
            "[00:01.123456789]Words",
            "[01:02:03.001]Words",
            "[ar:Artist][00:01.5]Words",
            "[00:01][00:02]Words",
        ] {
            assert!(has_timestamps(text), "{text}");
            assert_eq!(embedded(text.into()).synced_lrc.as_deref(), Some(text));
        }
        for text in [
            "[00:1e0]Words",
            "[00:+1]Words",
            "[00:1.1234567890]Words",
            "[00:1.]Words",
            "[00:.5]Words",
            "[00:60]Words",
            "[01:60:01]Words",
            "[00:01:02:03]Words",
            "[00:1.2.3]Words",
        ] {
            assert!(!has_timestamps(text), "{text}");
            let lyrics = embedded(text.into());
            assert_eq!(lyrics.plain_text.as_deref(), Some(text));
            assert!(lyrics.synced_lrc.is_none());
        }
    }
    #[test]
    fn rejects_oversized_and_invalid_utf8_metadata() {
        let mut dictionary = ffmpeg::Dictionary::new();
        dictionary.set("lyrics", &"x".repeat(MAX_TEXT_BYTES + 1));
        assert!(Tags::default().read(&dictionary).is_err());
        let mut raw = ptr::null_mut();
        let dictionary = unsafe {
            assert_eq!(
                ffmpeg::ffi::av_dict_set(&mut raw, c"lyrics".as_ptr(), c"\xff".as_ptr(), 0),
                0
            );
            ffmpeg::Dictionary::own(raw)
        };
        assert!(Tags::default().read(&dictionary).is_err());
    }
}
