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

/// An artist in the Wrapped top list. `share` is the artist's part of the year's listened time.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct WrappedArtist {
    pub name: String,
    pub plays: u64,
    pub listened_ms: u64,
    pub share: f64,
}

/// The artist first heard this year with the most listened time. `first_played` is UTC seconds.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Discovery {
    pub name: String,
    pub first_played: i64,
    pub plays: u64,
    pub listened_ms: u64,
}

/// The busiest weekday (0 = Monday) and the busiest 3-hour band, which starts at 0, 3, ... 21.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PeakTime {
    pub weekday: u8,
    pub band_start_hour: u8,
}

/// A lapsed love that returned this year after `gap_days` of silence.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Comeback {
    pub name: String,
    pub plays: u64,
    pub gap_days: u64,
}

/// One month of the year. `month` is 1 to 12.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MonthTop {
    pub month: u8,
    pub listened_ms: u64,
    pub top_artist: Option<String>,
}

/// Every candidate data point for one calendar year. `through` is UTC seconds.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct WrappedReport {
    pub year: i32,
    pub through: i64,
    pub listened_ms: u64,
    pub plays: u64,
    pub artists: u64,
    pub tracks: u64,
    pub top_artists: Vec<WrappedArtist>,
    pub top_tracks: Vec<RankedTrack>,
    pub discovery: Option<Discovery>,
    pub peak_time: Option<PeakTime>,
    pub most_skipped: Option<SkippedTrack>,
    pub comeback: Option<Comeback>,
    pub top_station: Option<StationTime>,
    pub months: Option<Vec<MonthTop>>,
}
