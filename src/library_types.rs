//! Durable library identities and bounded query/scan results. Paths identify
//! assets; track IDs identify playable items; queue occurrences remain separate.
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    pub disc_total: Option<u32>,
    pub compilation: bool,
    pub duration_ms: Option<u64>,
    pub sample_rate: u32,
    pub channels: u16,
    pub track_gain_db: Option<f64>,
    pub album_gain_db: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_peak: Option<f64>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_track_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub musicbrainz_album_artist_id: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub bit_depth: Option<u16>,
    #[serde(default)]
    pub bitrate_kbps: Option<u32>,
}

/// Whether a codec label (from `metadata::codec_label`) names a lossless format.
pub(crate) fn is_lossless(codec: &str) -> bool {
    matches!(
        codec,
        "FLAC" | "ALAC" | "WAV" | "AIFF" | "PCM" | "WavPack" | "APE"
    )
}

/// Short format description: `FLAC 24/96`, `WAV 16/44.1`, `MP3 320`, or the codec alone.
pub fn format_label(
    codec: Option<&str>,
    bit_depth: Option<u16>,
    sample_rate: u32,
    bitrate_kbps: Option<u32>,
) -> String {
    let Some(codec) = codec else {
        return String::new();
    };
    if is_lossless(codec) {
        if let Some(depth) = bit_depth.filter(|_| sample_rate != 0) {
            let tenths = (sample_rate + 50) / 100;
            let khz = if tenths.is_multiple_of(10) {
                (tenths / 10).to_string()
            } else {
                format!("{}.{}", tenths / 10, tenths % 10)
            };
            return format!("{codec} {depth}/{khz}");
        }
    } else if let Some(kbps) = bitrate_kbps {
        return format!("{codec} {kbps}");
    }
    codec.to_owned()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    #[default]
    Idle,
    Discovering,
    Indexing,
    Complete,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ScanStatus {
    pub root_id: Option<u64>,
    pub phase: ScanPhase,
    pub discovered: u64,
    pub processed: u64,
    pub total: Option<u64>,
    pub updated: u64,
    pub unchanged: u64,
    pub failures: u64,
    pub skipped: u64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LibraryState {
    pub revision: u64,
    pub roots: u64,
    pub tracks: u64,
    pub scan: ScanStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Root {
    pub id: u64,
    pub path: String,
    pub last_scan: Option<ScanStatus>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AlbumArtist {
    pub id: u64,
    pub name: String,
    pub album_count: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Album {
    pub id: u64,
    pub artist_id: u64,
    pub title: String,
    pub album_artist: String,
    pub musicbrainz_release_id: Option<String>,
    pub compilation: bool,
    pub disc_count: u64,
    pub track_count: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Track {
    pub id: u64,
    pub asset_id: u64,
    pub album_id: u64,
    pub path: String,
    pub metadata: TrackMetadata,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Page<T> {
    pub revision: u64,
    pub total: u64,
    pub offset: usize,
    pub entries: Vec<T>,
}
#[derive(Debug)]
pub struct PageRequest {
    pub revision: Option<u64>,
    pub offset: usize,
    pub limit: usize,
    pub query: String,
}

#[derive(Debug)]
pub enum LibraryCommand {
    AddRoot {
        path: String,
    },
    Rescan {
        root_id: u64,
    },
    Roots(PageRequest),
    Artists(PageRequest),
    Albums {
        artist_id: Option<u64>,
        page: PageRequest,
    },
    Tracks {
        album_id: Option<u64>,
        page: PageRequest,
    },
}
#[derive(Debug)]
pub enum LibraryReply {
    RootAdded(Root),
    ScanAccepted { root_id: u64 },
    Roots(Page<Root>),
    Artists(Page<AlbumArtist>),
    Albums(Page<Album>),
    Tracks(Page<Track>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryErrorKind {
    Invalid,
    InvalidFilter,
    Busy,
    StaleRevision,
    Storage,
}
#[derive(Debug)]
pub struct LibraryError {
    pub kind: LibraryErrorKind,
    pub message: String,
}
impl fmt::Display for LibraryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for LibraryError {}

#[derive(Debug)]
pub struct StaleLibraryRevision {
    pub current: u64,
}
impl fmt::Display for StaleLibraryRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "library revision is stale; current {}",
            self.current
        )
    }
}
impl std::error::Error for StaleLibraryRevision {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_labels_follow_codec_class() {
        assert_eq!(
            format_label(Some("FLAC"), Some(24), 96_000, None),
            "FLAC 24/96"
        );
        assert_eq!(
            format_label(Some("WAV"), Some(16), 44_100, None),
            "WAV 16/44.1"
        );
        assert_eq!(
            format_label(Some("MP3"), None, 44_100, Some(320)),
            "MP3 320"
        );
        assert_eq!(format_label(None, None, 0, None), "");
    }

    #[test]
    fn legacy_metadata_json_without_new_fields_deserializes() {
        let mut value = serde_json::to_value(TrackMetadata::default()).unwrap();
        let object = value.as_object_mut().unwrap();
        for key in ["year", "genres", "codec", "bit_depth", "bitrate_kbps"] {
            object.remove(key);
        }
        let metadata: TrackMetadata = serde_json::from_value(value).unwrap();
        assert_eq!(metadata, TrackMetadata::default());
    }
}
