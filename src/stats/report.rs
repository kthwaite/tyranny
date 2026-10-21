//! Aggregates play rows into a `StatsReport` for one period.
#![allow(dead_code)] // Task 7 uses this
use super::{
    period::{Bounds, Zone, buckets, local},
    rows::{LibraryIndex, PlayRow, artist_key},
};
use crate::stats_types::{
    Bucket, LapsedLove, PeriodKind, RankedArtist, RankedTrack, ShallowDive, SkippedTrack,
    StationTime, StatsReport,
};
use crate::store::EndReason;
use std::collections::HashMap;

/// Every tunable number in one place.
#[derive(Clone, Debug)]
pub(crate) struct Thresholds {
    pub list_limit: usize,
    pub most_skipped_limit: usize,
    pub most_skipped_min_started: u64,
    /// A play with less listened time than this is a skim.
    pub skim_ms: u64,
    pub lapsed_min_span_days: u64,
    pub lapsed_gap_min_days: u64,
    pub lapsed_gap_max_days: u64,
    pub lapsed_plays_floor: u64,
    pub shallow_plays_floor: u64,
    /// Plays that a full `scale_span_days` of history asks for.
    pub scale_plays: u64,
    pub scale_span_days: u64,
    pub shallow_top_n: usize,
    pub shallow_share: f64,
}
impl Default for Thresholds {
    fn default() -> Self {
        Self {
            list_limit: 25,
            most_skipped_limit: 10,
            most_skipped_min_started: 3,
            skim_ms: 5_000,
            lapsed_min_span_days: 90,
            lapsed_gap_min_days: 30,
            lapsed_gap_max_days: 1_095,
            lapsed_plays_floor: 10,
            shallow_plays_floor: 20,
            scale_plays: 150,
            scale_span_days: 1_826,
            shallow_top_n: 3,
            shallow_share: 0.6,
        }
    }
}

/// How often each spelling of one key was played, and when it last was.
#[derive(Default)]
struct Names(HashMap<String, (u64, i64)>);
impl Names {
    fn add(&mut self, spelling: &str, played_at: i64) {
        let slot = self
            .0
            .entry(spelling.trim().to_string())
            .or_insert((0, i64::MIN));
        slot.0 += 1;
        slot.1 = slot.1.max(played_at);
    }
    /// The most frequent spelling. Ties go to the latest play.
    fn display(&self) -> String {
        self.0
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(spelling, _)| spelling.clone())
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct ArtistTally {
    plays: u64,
    listened_ms: u64,
    last_played: i64,
    names: Names,
    /// Counted plays by track key, for plays with a title.
    tracks: HashMap<String, (u64, Names)>,
}

#[derive(Default)]
struct TrackTally {
    plays: u64,
    listened_ms: u64,
    started: u64,
    skips: u64,
    artist: Names,
    title: Names,
}

/// Ranks `items` by plays or by time, each breaking ties by the other and then by key.
fn rank<T>(mut items: Vec<(String, u64, u64, T)>, by_time: bool, limit: usize) -> Vec<T> {
    items.sort_by(|a, b| {
        let (a_main, a_other, b_main, b_other) = if by_time {
            (a.2, a.1, b.2, b.1)
        } else {
            (a.1, a.2, b.1, b.2)
        };
        b_main
            .cmp(&a_main)
            .then(b_other.cmp(&a_other))
            .then_with(|| a.0.cmp(&b.0))
    });
    items.into_iter().take(limit).map(|item| item.3).collect()
}

pub(crate) fn report(
    rows: &[PlayRow],
    library: &LibraryIndex,
    zone: &dyn Zone,
    period: Bounds,
    kind: PeriodKind,
    now: i64,
    thresholds: &Thresholds,
) -> StatsReport {
    let in_period = |row: &&PlayRow| row.played_at >= period.start && row.played_at < period.end;
    let limit = thresholds.list_limit;
    let buckets = buckets(zone, kind, period);
    let mut over_time: Vec<Bucket> = buckets
        .iter()
        .map(|&start| Bucket {
            start,
            listened_ms: 0,
        })
        .collect();
    let mut heatmap = vec![[0u64; 24]; 7];
    let (mut plays, mut listened_ms, mut untimed_plays) = (0, 0, 0);
    let (mut started, mut skips) = (0u64, 0u64);
    let mut artists: HashMap<String, ArtistTally> = HashMap::new();
    let mut tracks: HashMap<(String, String), TrackTally> = HashMap::new();
    let mut stations: HashMap<&str, u64> = HashMap::new();

    for row in rows.iter().filter(in_period) {
        let artist = artist_key(&row.artist);
        let title = artist_key(&row.title);
        if let Some(ms) = row.listened_ms {
            listened_ms += ms;
            let bucket = buckets.partition_point(|&start| start <= row.played_at);
            if bucket > 0 {
                over_time[bucket - 1].listened_ms += ms;
            }
            let at = local(zone, row.played_at);
            heatmap[usize::from(at.weekday)][usize::from(at.hour)] += ms;
            if let (true, Some(station)) = (row.radio, &row.station) {
                *stations.entry(station).or_default() += ms;
            }
        }
        let ms = row.listened_ms.unwrap_or(0);
        if row.counted {
            plays += 1;
            untimed_plays += u64::from(row.listened_ms.is_none());
        }
        // Plays count only counted rows, but listened time sums every timed row.
        if !artist.is_empty() {
            let tally = artists.entry(artist.clone()).or_default();
            tally.plays += u64::from(row.counted);
            tally.listened_ms += ms;
            tally.names.add(&row.artist, row.played_at);
        }
        // A play is started unless it is radio, from before schema 8, or a skim.
        let play_started = !row.radio && row.listened_ms.is_some_and(|ms| ms >= thresholds.skim_ms);
        let skipped = play_started && !row.counted && row.end_reason == Some(EndReason::Skipped);
        started += u64::from(play_started);
        skips += u64::from(skipped);
        if !title.is_empty() {
            let tally = tracks.entry((artist, title)).or_default();
            tally.plays += u64::from(row.counted);
            tally.listened_ms += ms;
            tally.started += u64::from(play_started);
            tally.skips += u64::from(skipped);
            tally.artist.add(&row.artist, row.played_at);
            tally.title.add(&row.title, row.played_at);
        }
    }

    let ranked_artist = |key: &String, tally: &ArtistTally| {
        (
            key.clone(),
            tally.plays,
            tally.listened_ms,
            RankedArtist {
                name: tally.names.display(),
                plays: tally.plays,
                listened_ms: tally.listened_ms,
                artist_id: library.artists.get(key).copied(),
            },
        )
    };
    // By plays, only entries with a counted play. By time, any entry with listened time.
    let listed =
        |plays: u64, listened_ms: u64, by_time: bool| plays > 0 || (by_time && listened_ms > 0);
    let artist_list = |by_time| {
        let items = artists
            .iter()
            .filter(|(_, t)| listed(t.plays, t.listened_ms, by_time))
            .map(|(k, t)| ranked_artist(k, t))
            .collect();
        rank(items, by_time, limit)
    };
    let counted_tracks: Vec<_> = tracks.iter().filter(|(_, tally)| tally.plays > 0).collect();
    let ranked_track = |key: &(String, String), tally: &TrackTally| {
        (
            format!("{}\n{}", key.0, key.1),
            tally.plays,
            tally.listened_ms,
            RankedTrack {
                title: tally.title.display(),
                artist: tally.artist.display(),
                plays: tally.plays,
                listened_ms: tally.listened_ms,
                track_id: library.tracks.get(key).copied(),
            },
        )
    };
    let track_list = |by_time| {
        let items = tracks
            .iter()
            .filter(|(_, t)| listed(t.plays, t.listened_ms, by_time))
            .map(|(k, t)| ranked_track(k, t))
            .collect();
        rank(items, by_time, limit)
    };

    // History before the period decides which artists are new.
    let mut first_heard: HashMap<String, i64> = HashMap::new();
    // All history, whatever the period, feeds lapsed loves and shallow dives.
    let mut history: HashMap<String, ArtistTally> = HashMap::new();
    let mut first_play: Option<i64> = None;
    for row in rows.iter().filter(|row| row.counted) {
        first_play = Some(first_play.map_or(row.played_at, |first| first.min(row.played_at)));
        let artist = artist_key(&row.artist);
        if artist.is_empty() {
            continue;
        }
        let first = first_heard.entry(artist.clone()).or_insert(row.played_at);
        *first = (*first).min(row.played_at);
        let tally = history.entry(artist).or_default();
        tally.plays += 1;
        tally.last_played = tally.last_played.max(row.played_at);
        tally.names.add(&row.artist, row.played_at);
        let title = artist_key(&row.title);
        if !title.is_empty() {
            let (count, names) = tally.tracks.entry(title).or_default();
            *count += 1;
            names.add(&row.title, row.played_at);
        }
    }
    let new_artists = rank(
        artists
            .iter()
            .filter(|(key, tally)| {
                tally.plays > 0
                    && first_heard
                        .get(*key)
                        .is_some_and(|&first| first >= period.start)
            })
            .map(|(k, t)| ranked_artist(k, t))
            .collect(),
        false,
        limit,
    );

    let mut most_skipped: Vec<_> = tracks
        .iter()
        .filter(|(_, t)| t.started >= thresholds.most_skipped_min_started && t.skips > 0)
        .collect();
    most_skipped.sort_by(|a, b| {
        (b.1.skips * a.1.started)
            .cmp(&(a.1.skips * b.1.started))
            .then(b.1.skips.cmp(&a.1.skips))
            .then_with(|| a.0.cmp(b.0))
    });
    let most_skipped = most_skipped
        .into_iter()
        .take(thresholds.most_skipped_limit)
        .map(|(key, tally)| SkippedTrack {
            title: tally.title.display(),
            artist: tally.artist.display(),
            started: tally.started,
            skips: tally.skips,
            track_id: library.tracks.get(key).copied(),
        })
        .collect();

    let mut stations: Vec<_> = stations
        .into_iter()
        .map(|(name, listened_ms)| StationTime {
            name: name.to_string(),
            listened_ms,
        })
        .collect();
    stations.sort_by(|a, b| {
        b.listened_ms
            .cmp(&a.listened_ms)
            .then_with(|| a.name.cmp(&b.name))
    });

    let span_days = first_play.map_or(0, |first| (now - first).max(0) as u64 / 86_400);
    let scaled = thresholds.scale_plays * span_days.min(thresholds.scale_span_days)
        / thresholds.scale_span_days;
    let artist_id = |key: &String| library.artists.get(key).copied();

    let mut lapsed: Vec<_> = Vec::new();
    if span_days >= thresholds.lapsed_min_span_days {
        let gap_days = (span_days / 4).clamp(
            thresholds.lapsed_gap_min_days,
            thresholds.lapsed_gap_max_days,
        );
        let cutoff = now - gap_days as i64 * 86_400;
        let min_plays = thresholds.lapsed_plays_floor.max(scaled);
        lapsed = history
            .iter()
            .filter(|(_, t)| t.plays >= min_plays && t.last_played < cutoff)
            .collect();
        lapsed.sort_by(|a, b| {
            b.1.plays
                .cmp(&a.1.plays)
                .then(a.1.last_played.cmp(&b.1.last_played))
                .then_with(|| a.0.cmp(b.0))
        });
    }
    let lapsed_loves = lapsed
        .into_iter()
        .take(limit)
        .map(|(key, tally)| LapsedLove {
            name: tally.names.display(),
            plays: tally.plays,
            last_played: tally.last_played,
            artist_id: artist_id(key),
        })
        .collect();

    let min_plays = thresholds.shallow_plays_floor.max(scaled);
    let mut shallow: Vec<_> = history
        .iter()
        .filter(|(_, t)| t.plays >= min_plays)
        .filter_map(|(key, tally)| {
            let mut top: Vec<_> = tally.tracks.iter().collect();
            top.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
            top.truncate(thresholds.shallow_top_n);
            let share =
                top.iter().map(|(_, (count, _))| count).sum::<u64>() as f64 / tally.plays as f64;
            (share >= thresholds.shallow_share).then(|| {
                let id = artist_id(key);
                (
                    key,
                    ShallowDive {
                        name: tally.names.display(),
                        plays: tally.plays,
                        top_share: share,
                        top_tracks: top.iter().map(|(_, (_, names))| names.display()).collect(),
                        library_tracks: id.and_then(|id| library.artist_tracks.get(&id).copied()),
                        artist_id: id,
                    },
                )
            })
        })
        .collect();
    shallow.sort_by(|a, b| {
        b.1.top_share
            .total_cmp(&a.1.top_share)
            .then(b.1.plays.cmp(&a.1.plays))
            .then_with(|| a.0.cmp(b.0))
    });

    StatsReport {
        period_start: period.start,
        period_end: period.end,
        plays,
        listened_ms,
        untimed_plays,
        artists: artists.values().filter(|tally| tally.plays > 0).count() as u64,
        tracks: counted_tracks.len() as u64,
        skip_rate: (started > 0).then(|| skips as f64 / started as f64),
        top_artists_by_plays: artist_list(false),
        top_artists_by_time: artist_list(true),
        top_tracks_by_plays: track_list(false),
        top_tracks_by_time: track_list(true),
        over_time,
        heatmap,
        new_artists,
        most_skipped,
        stations,
        lapsed_loves,
        shallow_dives: shallow
            .into_iter()
            .take(limit)
            .map(|(_, dive)| dive)
            .collect(),
        history_span_days: span_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::period::FixedZone;

    const DAY: i64 = 86_400;
    /// 2026-01-01T00:00:00Z.
    const T0: i64 = 1_767_225_600;

    /// A counted, finished, 3-minute library play of `title` by `artist`.
    fn play(played_at: i64, artist: &str, title: &str) -> PlayRow {
        PlayRow {
            played_at,
            listened_ms: Some(180_000),
            counted: true,
            end_reason: Some(EndReason::Finished),
            track_id: None,
            artist: artist.into(),
            title: title.into(),
            radio: false,
            station: None,
        }
    }
    fn listened(mut row: PlayRow, ms: u64) -> PlayRow {
        row.listened_ms = Some(ms);
        row
    }
    /// An uncounted play that ended skipped after `ms`.
    fn skip(played_at: i64, artist: &str, title: &str, ms: u64) -> PlayRow {
        PlayRow {
            counted: false,
            end_reason: Some(EndReason::Skipped),
            ..listened(play(played_at, artist, title), ms)
        }
    }
    fn radio(played_at: i64, artist: &str, title: &str, station: &str, counted: bool) -> PlayRow {
        PlayRow {
            counted,
            end_reason: Some(EndReason::Stopped),
            radio: true,
            station: Some(station.into()),
            ..play(played_at, artist, title)
        }
    }
    fn index() -> LibraryIndex {
        LibraryIndex::default()
    }
    fn run(
        rows: &[PlayRow],
        library: &LibraryIndex,
        kind: PeriodKind,
        period: Bounds,
        now: i64,
    ) -> StatsReport {
        report(
            rows,
            library,
            &FixedZone(0),
            period,
            kind,
            now,
            &Thresholds::default(),
        )
    }
    /// Bounds of the whole of January 2026, with `now` at its end.
    fn january() -> Bounds {
        Bounds {
            start: T0,
            end: T0 + 31 * DAY,
        }
    }
    fn month(rows: &[PlayRow]) -> StatsReport {
        run(rows, &index(), PeriodKind::Month, january(), T0 + 31 * DAY)
    }

    #[test]
    fn empty_history_gives_an_empty_report() {
        let report = month(&[]);
        assert_eq!(
            (report.plays, report.listened_ms, report.untimed_plays),
            (0, 0, 0)
        );
        assert_eq!(
            (report.artists, report.tracks, report.history_span_days),
            (0, 0, 0)
        );
        assert_eq!(report.skip_rate, None);
        assert!(report.top_artists_by_plays.is_empty() && report.top_artists_by_time.is_empty());
        assert!(report.top_tracks_by_plays.is_empty() && report.top_tracks_by_time.is_empty());
        assert!(report.new_artists.is_empty() && report.most_skipped.is_empty());
        assert!(report.stations.is_empty() && report.lapsed_loves.is_empty());
        assert!(report.shallow_dives.is_empty());
        assert_eq!(report.heatmap, vec![[0u64; 24]; 7]);
        assert_eq!(report.over_time.len(), 31);
        assert!(
            report
                .over_time
                .iter()
                .all(|bucket| bucket.listened_ms == 0)
        );
        assert_eq!(
            (report.period_start, report.period_end),
            (T0, T0 + 31 * DAY)
        );
    }

    #[test]
    fn tiles_count_counted_plays_and_sum_known_time() {
        let untimed = PlayRow {
            listened_ms: None,
            end_reason: None,
            ..play(T0 + 30, "A", "z")
        };
        let rows = [
            play(T0, "A", "x"),
            listened(play(T0 + 10, "A", "y"), 60_000),
            untimed,
            skip(T0 + 20, "A", "w", 20_000),
            play(T0 - 10, "A", "before the period"),
            play(T0 + 31 * DAY, "A", "after the period"),
        ];
        let report = month(&rows);
        assert_eq!(report.plays, 3);
        assert_eq!(report.untimed_plays, 1);
        assert_eq!(report.listened_ms, 180_000 + 60_000 + 20_000);
        assert_eq!(report.artists, 1);
        assert_eq!(report.tracks, 3);
    }

    #[test]
    fn skips_are_uncounted_skipped_plays() {
        let late_skip = PlayRow {
            end_reason: Some(EndReason::Skipped),
            ..play(T0, "A", "late")
        };
        let rows = [
            late_skip,
            skip(T0 + 1, "A", "a", 30_000),
            play(T0 + 2, "A", "b"),
            play(T0 + 3, "A", "c"),
        ];
        // 4 started, 1 skip. The late skip is a counted play.
        assert_eq!(month(&rows).skip_rate, Some(0.25));
    }

    #[test]
    fn skims_are_left_out_of_skip_stats() {
        let mut rows: Vec<PlayRow> = (0..20).map(|i| skip(T0 + i, "A", "scrub", 300)).collect();
        rows.push(play(T0 + 100, "A", "full"));
        let report = month(&rows);
        assert_eq!(report.skip_rate, Some(0.0));
        assert!(report.most_skipped.is_empty());
        // The skims' time still counts.
        assert_eq!(report.listened_ms, 180_000 + 20 * 300);
    }

    #[test]
    fn radio_never_enters_skip_stats_and_junk_never_enters_artists() {
        let rows = [
            radio(T0, "Burial", "Archangel", "FM", true),
            radio(T0 + 1, "", "", "FM", false),
            PlayRow {
                end_reason: Some(EndReason::Skipped),
                ..radio(T0 + 2, "X", "Y", "FM", false)
            },
        ];
        let report = month(&rows);
        assert_eq!(report.skip_rate, None);
        assert_eq!(report.plays, 1);
        assert_eq!(report.artists, 1);
        assert_eq!(report.top_artists_by_plays.len(), 1);
        assert_eq!(
            report.stations,
            vec![StationTime {
                name: "FM".into(),
                listened_ms: 540_000
            }]
        );
    }

    #[test]
    fn radio_and_library_merge_by_case_insensitive_name() {
        let mut library = index();
        library.artists.insert("burial".into(), 7);
        library
            .tracks
            .insert(("burial".into(), "archangel".into()), 70);
        let rows = [
            play(T0, "Burial", "Archangel"),
            play(T0 + 1, "Burial", "Archangel"),
            radio(T0 + 2, " burial ", "archangel", "FM", true),
        ];
        let report = run(&rows, &library, PeriodKind::Month, january(), T0 + 31 * DAY);
        assert_eq!(report.artists, 1);
        assert_eq!(report.tracks, 1);
        assert_eq!(
            report.top_artists_by_plays,
            vec![RankedArtist {
                name: "Burial".into(),
                plays: 3,
                listened_ms: 540_000,
                artist_id: Some(7)
            }]
        );
        assert_eq!(
            report.top_tracks_by_plays,
            vec![RankedTrack {
                title: "Archangel".into(),
                artist: "Burial".into(),
                plays: 3,
                listened_ms: 540_000,
                track_id: Some(70),
            }]
        );
    }

    #[test]
    fn display_name_is_the_most_frequent_spelling_latest_on_ties() {
        let rows = [
            play(T0, "burial", "a"),
            play(T0 + 1, "Burial", "a"),
            play(T0 + 2, "BURIAL", "a"),
            play(T0 + 3, "Burial", "a"),
        ];
        assert_eq!(month(&rows).top_artists_by_plays[0].name, "Burial");
        let tie = [play(T0, "Burial", "a"), play(T0 + 1, "burial", "a")];
        assert_eq!(month(&tie).top_artists_by_plays[0].name, "burial");
    }

    #[test]
    fn top_artists_by_plays_and_by_time_differ() {
        let rows = [
            play(T0, "Short", "a"),
            play(T0 + 1, "Short", "a"),
            play(T0 + 2, "Short", "a"),
            listened(play(T0 + 3, "Long", "b"), 900_000),
            listened(play(T0 + 4, "Long", "b"), 900_000),
        ];
        let report = month(&rows);
        let names = |list: &[RankedArtist]| list.iter().map(|a| a.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&report.top_artists_by_plays), ["Short", "Long"]);
        assert_eq!(names(&report.top_artists_by_time), ["Long", "Short"]);
        assert_eq!(report.top_artists_by_time[0].listened_ms, 1_800_000);
        assert_eq!(report.top_tracks_by_time[0].title, "b");
        assert_eq!(report.top_tracks_by_plays[0].title, "a");
    }

    #[test]
    fn by_time_rankings_count_skipped_time_but_plays_stay_counted_only() {
        let open = PlayRow {
            end_reason: None,
            counted: false,
            ..listened(play(T0 + 3, "Heard", "a"), 30_000)
        };
        let untimed = PlayRow {
            listened_ms: None,
            end_reason: None,
            ..play(T0 + 4, "Heard", "a")
        };
        let rows = [
            play(T0, "Heard", "a"),
            skip(T0 + 1, "Skipped", "b", 600_000),
            skip(T0 + 2, "Skipped", "b", 600_000),
            open,
            untimed,
        ];
        let report = month(&rows);
        let by_time: Vec<_> = report
            .top_artists_by_time
            .iter()
            .map(|a| (a.name.as_str(), a.plays, a.listened_ms))
            .collect();
        assert_eq!(by_time, [("Skipped", 0, 1_200_000), ("Heard", 2, 210_000)]);
        assert_eq!(report.top_tracks_by_time[0].title, "b");
        assert_eq!(report.top_tracks_by_time[0].plays, 0);
        // By plays, an entry with no counted play is absent.
        let by_plays: Vec<_> = report
            .top_artists_by_plays
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(by_plays, ["Heard"]);
        assert_eq!(report.top_artists_by_plays[0].listened_ms, 210_000);
        assert_eq!(report.top_tracks_by_plays.len(), 1);
        assert_eq!((report.artists, report.tracks, report.plays), (1, 1, 2));
        assert!(report.new_artists.iter().all(|a| a.name == "Heard"));
    }

    #[test]
    fn lists_are_limited_and_ties_break_by_the_other_measure_then_name() {
        let rows: Vec<PlayRow> = (0..30)
            .map(|i| play(T0 + i, &format!("artist {i:02}"), "t"))
            .collect();
        let report = month(&rows);
        assert_eq!(report.top_artists_by_plays.len(), 25);
        assert_eq!(report.top_artists_by_plays[0].name, "artist 00");
        let rows = [
            play(T0, "B", "t"),
            listened(play(T0 + 1, "A", "t"), 1),
            listened(play(T0 + 2, "C", "t"), 1),
        ];
        let names: Vec<_> = month(&rows)
            .top_artists_by_plays
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, ["B", "A", "C"]);
    }

    #[test]
    fn heatmap_buckets_listened_time_by_local_weekday_and_hour() {
        // 2026-01-01 is a Thursday. At UTC+1, 23:30Z on the 1st is 00:30 Friday.
        let rows = [
            listened(play(T0 + 10 * 3600, "A", "a"), 1_000),
            listened(play(T0 + 23 * 3600 + 1800, "A", "a"), 2_000),
        ];
        let report = run(&rows, &index(), PeriodKind::Month, january(), T0 + 31 * DAY);
        assert_eq!(report.heatmap[3][10], 1_000);
        assert_eq!(report.heatmap[3][23], 2_000);
        let report = report_in(&rows, 3600);
        assert_eq!(report.heatmap[3][11], 1_000);
        assert_eq!(report.heatmap[4][0], 2_000);
    }
    fn report_in(rows: &[PlayRow], offset: i32) -> StatsReport {
        report(
            rows,
            &index(),
            &FixedZone(offset),
            january(),
            PeriodKind::Month,
            T0 + 31 * DAY,
            &Thresholds::default(),
        )
    }

    #[test]
    fn new_artists_are_first_heard_in_the_period() {
        let rows = [
            play(T0 - DAY, "Old", "a"),
            play(T0 + 1, "Old", "a"),
            play(T0 + 2, "Fresh", "a"),
            play(T0 + 3, "Fresh", "a"),
            play(T0 + 4, "Newer", "a"),
            skip(T0 + 5, "Skipped Only", "a", 20_000),
        ];
        let names: Vec<_> = month(&rows)
            .new_artists
            .into_iter()
            .map(|a| (a.name, a.plays))
            .collect();
        assert_eq!(names, [("Fresh".to_string(), 2), ("Newer".to_string(), 1)]);
    }

    #[test]
    fn most_skipped_needs_three_starts() {
        let rows = [
            // Two starts only.
            skip(T0, "A", "two", 20_000),
            skip(T0 + 1, "A", "two", 20_000),
            // Three starts, two skips.
            skip(T0 + 2, "A", "often", 20_000),
            skip(T0 + 3, "A", "often", 20_000),
            play(T0 + 4, "A", "often"),
            // Three starts, one skip, and an open play counted as started.
            skip(T0 + 5, "A", "some", 20_000),
            play(T0 + 6, "A", "some"),
            PlayRow {
                end_reason: None,
                counted: false,
                ..listened(play(T0 + 7, "A", "some"), 10_000)
            },
            // Never skipped.
            play(T0 + 8, "A", "never"),
            play(T0 + 9, "A", "never"),
            play(T0 + 10, "A", "never"),
        ];
        let skipped: Vec<_> = month(&rows)
            .most_skipped
            .into_iter()
            .map(|t| (t.title, t.started, t.skips))
            .collect();
        assert_eq!(
            skipped,
            [("often".to_string(), 3, 2), ("some".to_string(), 3, 1)]
        );
    }

    #[test]
    fn lapsed_loves_wait_for_ninety_days_of_history() {
        let fan = |count: i64, from: i64| (0..count).map(move |i| play(from + i, "Fan", "t"));
        // Span 30 days: nothing, however long ago the plays were.
        let now = T0 + 30 * DAY;
        let rows: Vec<_> = fan(40, T0).collect();
        let short = run(&rows, &index(), PeriodKind::Month, january(), now);
        assert!(short.lapsed_loves.is_empty());
        assert_eq!(short.history_span_days, 30);
        // Span 400 days: gap 100 days, at least 32 plays.
        let now = T0 + 400 * DAY;
        let mut library = index();
        library.artists.insert("fan".into(), 5);
        let bounds = Bounds {
            start: T0,
            end: now + 1,
        };
        let lapsed =
            |rows: &[PlayRow]| run(rows, &library, PeriodKind::All, bounds, now).lapsed_loves;
        let mut rows: Vec<_> = fan(32, T0).collect();
        assert_eq!(
            lapsed(&rows),
            vec![LapsedLove {
                name: "Fan".into(),
                plays: 32,
                last_played: T0 + 31,
                artist_id: Some(5)
            }]
        );
        // 31 plays are too few.
        rows.pop();
        rows.push(play(T0 + 400 * DAY, "Anchor", "t"));
        assert!(lapsed(&rows).is_empty());
        // A play within the last 100 days ends the lapse.
        let mut rows: Vec<_> = fan(32, T0).collect();
        rows.push(play(now - 99 * DAY, "Fan", "t"));
        assert!(lapsed(&rows).is_empty());
    }

    #[test]
    fn shallow_dives_need_sixty_percent_in_top_three() {
        let now = T0 + 100 * DAY;
        let mut library = index();
        library.artists.insert("narrow".into(), 1);
        library.artist_tracks.insert(1, 42);
        let mut rows = Vec::new();
        // 20 plays, 12 in the top three: exactly 60%.
        for (i, title) in [
            "a", "a", "a", "a", "b", "b", "b", "b", "c", "c", "c", "c", "d", "e", "f", "g", "h",
            "i", "j", "k",
        ]
        .iter()
        .enumerate()
        {
            rows.push(play(T0 + i as i64, "Narrow", title));
        }
        // 20 plays spread over 20 titles.
        for i in 0..20 {
            rows.push(play(T0 + 100 + i, "Wide", &format!("t{i}")));
        }
        // 19 plays, all one track: under the 20-play floor.
        for i in 0..19 {
            rows.push(play(T0 + 200 + i, "Few", "t"));
        }
        let bounds = Bounds {
            start: T0,
            end: now + 1,
        };
        let report = run(&rows, &library, PeriodKind::All, bounds, now);
        assert_eq!(report.shallow_dives.len(), 1);
        let dive = &report.shallow_dives[0];
        assert_eq!((dive.name.as_str(), dive.plays), ("Narrow", 20));
        assert_eq!(dive.top_share, 0.6);
        assert_eq!(dive.top_tracks, ["a", "b", "c"]);
        assert_eq!((dive.library_tracks, dive.artist_id), (Some(42), Some(1)));
    }

    #[test]
    fn over_time_buckets_follow_the_period_kind() {
        // Day: hourly.
        let day = Bounds {
            start: T0,
            end: T0 + DAY,
        };
        let rows = [
            listened(play(T0 + 2 * 3600 + 5, "A", "a"), 7_000),
            listened(play(T0 + 5 * 3600, "A", "a"), 3_000),
        ];
        let report = run(&rows, &index(), PeriodKind::Day, day, T0 + DAY);
        assert_eq!(report.over_time.len(), 24);
        assert_eq!(
            report.over_time[2],
            Bucket {
                start: T0 + 2 * 3600,
                listened_ms: 7_000
            }
        );
        assert_eq!(report.over_time[5].listened_ms, 3_000);
        // Year: months.
        let year = Bounds {
            start: T0,
            end: T0 + 365 * DAY,
        };
        let rows = [listened(play(T0 + 40 * DAY, "A", "a"), 9_000)];
        let report = run(&rows, &index(), PeriodKind::Year, year, T0 + 365 * DAY);
        assert_eq!(report.over_time.len(), 12);
        assert_eq!(report.over_time[1].listened_ms, 9_000);
        // All: years, with the first bucket starting before the period does.
        let first = T0 + 100 * DAY;
        let all = Bounds {
            start: first,
            end: T0 + 800 * DAY,
        };
        let rows = [
            listened(play(first, "A", "a"), 1_000),
            listened(play(T0 + 400 * DAY, "A", "a"), 2_000),
        ];
        let report = run(&rows, &index(), PeriodKind::All, all, T0 + 800 * DAY - 1);
        assert_eq!(
            report
                .over_time
                .iter()
                .map(|b| b.listened_ms)
                .collect::<Vec<_>>(),
            [1_000, 2_000, 0]
        );
    }
}
