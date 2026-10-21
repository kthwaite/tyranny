//! Wire types for listening statistics.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKind {
    Day,
    Week,
    Month,
    Year,
    All,
}

/// A period kind plus a local anchor date (`YYYY-MM-DD`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PeriodRequest {
    pub kind: PeriodKind,
    pub anchor: String,
}

/// An artist in a ranking. `artist_id` is set when the name matches exactly one library artist.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RankedArtist {
    pub name: String,
    pub plays: u64,
    pub listened_ms: u64,
    pub artist_id: Option<u64>,
}

/// A track in a ranking. `track_id` is set when the title and artist match exactly one library track.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RankedTrack {
    pub title: String,
    pub artist: String,
    pub plays: u64,
    pub listened_ms: u64,
    pub track_id: Option<u64>,
}

/// Listened time in one sub-bucket of the period. `start` is UTC seconds.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Bucket {
    pub start: i64,
    pub listened_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SkippedTrack {
    pub title: String,
    pub artist: String,
    pub started: u64,
    pub skips: u64,
    pub track_id: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct StationTime {
    pub name: String,
    pub listened_ms: u64,
}

/// An artist with many plays and none for a long while. `last_played` is UTC seconds.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LapsedLove {
    pub name: String,
    pub plays: u64,
    pub last_played: i64,
    pub artist_id: Option<u64>,
}

/// An artist whose plays sit in a few tracks. `library_tracks` is the artist's library track count.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ShallowDive {
    pub name: String,
    pub plays: u64,
    pub top_share: f64,
    pub top_tracks: Vec<String>,
    pub library_tracks: Option<u64>,
    pub artist_id: Option<u64>,
}

/// Everything the Stats page shows for one period.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct StatsReport {
    /// UTC seconds, `[period_start, period_end)`.
    pub period_start: i64,
    pub period_end: i64,
    pub plays: u64,
    pub listened_ms: u64,
    pub untimed_plays: u64,
    pub artists: u64,
    pub tracks: u64,
    pub skip_rate: Option<f64>,
    pub top_artists_by_plays: Vec<RankedArtist>,
    pub top_artists_by_time: Vec<RankedArtist>,
    pub top_tracks_by_plays: Vec<RankedTrack>,
    pub top_tracks_by_time: Vec<RankedTrack>,
    pub over_time: Vec<Bucket>,
    /// 7 rows, Monday first, of 24 local hours.
    pub heatmap: Vec<[u64; 24]>,
    pub new_artists: Vec<RankedArtist>,
    pub most_skipped: Vec<SkippedTrack>,
    pub stations: Vec<StationTime>,
    pub lapsed_loves: Vec<LapsedLove>,
    pub shallow_dives: Vec<ShallowDive>,
    pub history_span_days: u64,
}
