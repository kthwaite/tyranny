//! Builds the `WrappedReport` for one calendar year from the play rows.
#![allow(dead_code)] // Task 8 uses this
use super::{
    period::{Date, Zone, bounds, local},
    report::{
        Names, Tallies, Thresholds, lapsed_gap_days, listed, rank, ranked_track, span_days,
        station_list,
    },
    rows::{LibraryIndex, PlayRow, artist_key},
};
use crate::stats_types::{
    Comeback, Discovery, MonthTop, PeakTime, PeriodKind, SkippedTrack, WrappedArtist, WrappedReport,
};
use std::collections::{BTreeSet, HashMap};

const TOP_LIMIT: usize = 5;
/// Minimum counted plays this year for the discovery artist.
const DISCOVERY_MIN_PLAYS: u64 = 5;
/// Minimum listened time in the year for a peak time.
const PEAK_MIN_MS: u64 = 10 * 3_600_000;
/// Hours in one peak-time band.
const BAND_HOURS: usize = 3;
const MOST_SKIPPED_MIN_SKIPS: u64 = 5;
const COMEBACK_MIN_PLAYS: u64 = 10;
const STATION_MIN_MS: u64 = 3_600_000;
const MONTHS_MIN: usize = 3;

/// One month of listening.
#[derive(Default)]
struct Month {
    listened_ms: u64,
    counted: u64,
    /// Listened time by artist key.
    artists: HashMap<String, (u64, Names)>,
}

/// An artist's counted plays: first and last before the year, and first within it.
#[derive(Default)]
struct Span {
    first: Option<i64>,
    last_before: Option<i64>,
    first_in_year: Option<i64>,
}

/// The index of the first maximum, so ties go to the earliest.
fn first_max(values: &[u64]) -> usize {
    let mut best = 0;
    for (index, &value) in values.iter().enumerate() {
        if value > values[best] {
            best = index;
        }
    }
    best
}

pub(crate) fn wrapped(
    rows: &[PlayRow],
    library: &LibraryIndex,
    zone: &dyn Zone,
    year: i32,
    now: i64,
    thresholds: &Thresholds,
) -> WrappedReport {
    let anchor = Date {
        year,
        month: 1,
        day: 1,
    };
    let period = bounds(zone, PeriodKind::Year, anchor, None, now);
    let through = if (period.start..period.end).contains(&now) {
        now
    } else {
        period.end
    };

    let mut tallies = Tallies::default();
    let mut listened_ms = 0;
    let mut plays = 0;
    let mut weekdays = [0u64; 7];
    let mut bands = [0u64; 24 / BAND_HOURS];
    let mut stations: HashMap<&str, u64> = HashMap::new();
    let mut months: Vec<Month> = (0..12).map(|_| Month::default()).collect();
    let mut spans: HashMap<String, Span> = HashMap::new();
    let mut first_play: Option<i64> = None;

    for row in rows.iter().filter(|row| row.played_at < period.end) {
        let in_year = row.played_at >= period.start;
        if row.counted {
            first_play = Some(first_play.map_or(row.played_at, |first| first.min(row.played_at)));
            let artist = artist_key(&row.artist);
            if !artist.is_empty() {
                let span = spans.entry(artist).or_default();
                span.first = Some(span.first.map_or(row.played_at, |t| t.min(row.played_at)));
                if in_year {
                    span.first_in_year = Some(
                        span.first_in_year
                            .map_or(row.played_at, |t| t.min(row.played_at)),
                    );
                } else {
                    span.last_before = Some(
                        span.last_before
                            .map_or(row.played_at, |t| t.max(row.played_at)),
                    );
                }
            }
        }
        if !in_year {
            continue;
        }
        let at = local(zone, row.played_at);
        let month = &mut months[usize::from(at.date.month) - 1];
        if let Some(ms) = row.listened_ms {
            listened_ms += ms;
            weekdays[usize::from(at.weekday)] += ms;
            bands[usize::from(at.hour) / BAND_HOURS] += ms;
            month.listened_ms += ms;
            let key = artist_key(&row.artist);
            if !key.is_empty() {
                let slot = month.artists.entry(key).or_default();
                slot.0 += ms;
                slot.1.add(&row.artist, row.played_at);
            }
            if let (true, Some(station)) = (row.radio, &row.station) {
                *stations.entry(station).or_default() += ms;
            }
        }
        if row.counted {
            plays += 1;
            month.counted += 1;
        }
        tallies.add(row, thresholds);
    }
    let Tallies { artists, tracks } = tallies;

    let top_artists = rank(
        artists
            .iter()
            .filter(|(_, t)| listed(t.plays, t.listened_ms, true))
            .map(|(key, t)| {
                let share = if listened_ms == 0 {
                    0.0
                } else {
                    t.listened_ms as f64 / listened_ms as f64
                };
                (
                    key.clone(),
                    t.plays,
                    t.listened_ms,
                    WrappedArtist {
                        name: t.names.display(),
                        plays: t.plays,
                        listened_ms: t.listened_ms,
                        share,
                    },
                )
            })
            .collect(),
        true,
        TOP_LIMIT,
    );
    let top_tracks = rank(
        tracks
            .iter()
            .filter(|(_, t)| listed(t.plays, t.listened_ms, false))
            .map(|(key, t)| ranked_track(key, t, library))
            .collect(),
        false,
        TOP_LIMIT,
    );

    // The artist first heard this year with the most listened time.
    let discovery = artists
        .iter()
        .filter(|(key, t)| {
            t.plays >= DISCOVERY_MIN_PLAYS
                && spans
                    .get(*key)
                    .is_some_and(|span| span.first.is_some_and(|first| first >= period.start))
        })
        .max_by(|a, b| {
            a.1.listened_ms
                .cmp(&b.1.listened_ms)
                .then(a.1.plays.cmp(&b.1.plays))
                .then_with(|| b.0.cmp(a.0))
        })
        .map(|(key, t)| Discovery {
            name: t.names.display(),
            first_played: spans[key].first.unwrap_or(period.start),
            plays: t.plays,
            listened_ms: t.listened_ms,
        });

    let peak_time = (listened_ms >= PEAK_MIN_MS).then(|| PeakTime {
        weekday: first_max(&weekdays) as u8,
        band_start_hour: (first_max(&bands) * BAND_HOURS) as u8,
    });

    let most_skipped = tracks
        .iter()
        .filter(|(_, t)| t.skips >= MOST_SKIPPED_MIN_SKIPS)
        .max_by(|a, b| {
            a.1.skips
                .cmp(&b.1.skips)
                .then_with(|| (a.1.skips * b.1.started).cmp(&(b.1.skips * a.1.started)))
                .then_with(|| b.0.cmp(a.0))
        })
        .map(|(key, t)| SkippedTrack {
            title: t.title.display(),
            artist: t.artist.display(),
            started: t.started,
            skips: t.skips,
            track_id: library.tracks.get(key).copied(),
        });

    // A lapsed love that returned: no plays for G before its first play this year.
    let gap = lapsed_gap_days(span_days(first_play, through), thresholds);
    let comeback = artists
        .iter()
        .filter(|(_, t)| t.plays >= COMEBACK_MIN_PLAYS)
        .filter_map(|(key, t)| {
            let span = spans.get(key)?;
            let silent = (span.first_in_year? - span.last_before?).max(0) as u64 / 86_400;
            (silent >= gap).then_some((key, t, silent))
        })
        .max_by(|a, b| a.1.plays.cmp(&b.1.plays).then_with(|| b.0.cmp(a.0)))
        .map(|(_, t, silent)| Comeback {
            name: t.names.display(),
            plays: t.plays,
            gap_days: silent,
        });

    let top_station = station_list(stations)
        .into_iter()
        .next()
        .filter(|station| station.listened_ms >= STATION_MIN_MS);

    let months =
        (months.iter().filter(|month| month.counted > 0).count() >= MONTHS_MIN).then(|| {
            months
                .iter()
                .enumerate()
                .filter(|(_, month)| month.counted > 0)
                .map(|(index, month)| MonthTop {
                    month: index as u8 + 1,
                    listened_ms: month.listened_ms,
                    top_artist: month
                        .artists
                        .iter()
                        .max_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| b.0.cmp(a.0)))
                        .map(|(_, (_, names))| names.display()),
                })
                .collect()
        });

    WrappedReport {
        year,
        through,
        listened_ms,
        plays,
        artists: artists.values().filter(|t| t.plays > 0).count() as u64,
        tracks: tracks.values().filter(|t| t.plays > 0).count() as u64,
        top_artists,
        top_tracks,
        discovery,
        peak_time,
        most_skipped,
        comeback,
        top_station,
        months,
    }
}

/// Years with at least one counted play, newest first.
pub(crate) fn years(rows: &[PlayRow], zone: &dyn Zone) -> Vec<i32> {
    let years: BTreeSet<i32> = rows
        .iter()
        .filter(|row| row.counted)
        .map(|row| local(zone, row.played_at).date.year)
        .collect();
    years.into_iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::period::FixedZone;
    use crate::store::EndReason;

    const DAY: i64 = 86_400;
    const HOUR_MS: u64 = 3_600_000;
    /// 2026-01-01T00:00:00Z, a Thursday.
    const T0: i64 = 1_767_225_600;
    const NOW: i64 = T0 + 360 * DAY;

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
    fn skip(played_at: i64, artist: &str, title: &str) -> PlayRow {
        PlayRow {
            counted: false,
            end_reason: Some(EndReason::Skipped),
            ..listened(play(played_at, artist, title), 20_000)
        }
    }
    fn radio(played_at: i64, station: &str, ms: u64) -> PlayRow {
        PlayRow {
            end_reason: Some(EndReason::Stopped),
            radio: true,
            station: Some(station.into()),
            ..listened(play(played_at, "Some Band", "Some Song"), ms)
        }
    }
    fn run(rows: &[PlayRow], year: i32, now: i64) -> WrappedReport {
        wrapped(
            rows,
            &LibraryIndex::default(),
            &FixedZone(0),
            year,
            now,
            &Thresholds::default(),
        )
    }
    fn year_2026(rows: &[PlayRow]) -> WrappedReport {
        run(rows, 2026, NOW)
    }
    /// `count` plays of one artist's track, one per hour from `start`.
    fn plays_of(start: i64, count: i64, artist: &str, title: &str) -> Vec<PlayRow> {
        (0..count)
            .map(|i| play(start + i * 3600, artist, title))
            .collect()
    }

    #[test]
    fn empty_year_has_no_optional_points() {
        let report = year_2026(&[]);
        assert_eq!(
            (
                report.listened_ms,
                report.plays,
                report.artists,
                report.tracks
            ),
            (0, 0, 0, 0)
        );
        assert!(report.top_artists.is_empty() && report.top_tracks.is_empty());
        assert!(report.discovery.is_none() && report.peak_time.is_none());
        assert!(report.most_skipped.is_none() && report.comeback.is_none());
        assert!(report.top_station.is_none() && report.months.is_none());
    }

    #[test]
    fn totals_count_this_year_only_and_time_includes_radio() {
        let rows = vec![
            play(T0 - DAY, "Last Year", "Old"),
            play(T0 + DAY, "A", "One"),
            skip(T0 + 2 * DAY, "A", "Two"),
            radio(T0 + 3 * DAY, "Jazz FM", 600_000),
            PlayRow {
                listened_ms: None,
                ..play(T0 + 4 * DAY, "B", "Three")
            },
        ];
        let report = year_2026(&rows);
        assert_eq!(report.plays, 3);
        assert_eq!(report.listened_ms, 180_000 + 20_000 + 600_000);
        assert_eq!((report.artists, report.tracks), (3, 3));
    }

    #[test]
    fn top_artists_rank_by_time_with_share() {
        let rows = vec![
            listened(play(T0 + DAY, "Quiet", "Q"), 100_000),
            listened(play(T0 + DAY + 600, "Quiet", "Q"), 100_000),
            listened(play(T0 + DAY + 1200, "Loud", "L"), 600_000),
            // Skipped time still ranks by time.
            skip(T0 + DAY + 1800, "Quiet", "Q2"),
        ];
        let report = year_2026(&rows);
        let names: Vec<_> = report.top_artists.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Loud", "Quiet"]);
        let total = 100_000 + 100_000 + 600_000 + 20_000;
        assert_eq!(report.listened_ms, total);
        assert_eq!(report.top_artists[0].plays, 1);
        assert_eq!(report.top_artists[1].plays, 2);
        assert_eq!(report.top_artists[1].listened_ms, 220_000);
        assert!((report.top_artists[0].share - 600_000.0 / total as f64).abs() < 1e-9);
        let sum: f64 = report.top_artists.iter().map(|a| a.share).sum();
        assert!(sum <= 1.0 + 1e-9);
    }

    #[test]
    fn top_artists_stop_at_five() {
        let rows: Vec<_> = (0..7)
            .map(|i| play(T0 + DAY + i, &format!("Artist {i}"), "S"))
            .collect();
        assert_eq!(year_2026(&rows).top_artists.len(), 5);
    }

    #[test]
    fn top_tracks_rank_by_counted_plays() {
        let mut rows = plays_of(T0 + DAY, 3, "A", "Most");
        rows.extend(plays_of(T0 + 2 * DAY, 2, "B", "Next"));
        // A long uncounted listen does not outrank counted plays.
        rows.push(listened(skip(T0 + 3 * DAY, "C", "Skipped"), 4_000_000));
        let report = year_2026(&rows);
        let titles: Vec<_> = report.top_tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Most", "Next"]);
        assert_eq!(report.top_tracks[0].plays, 3);
    }

    #[test]
    fn discovery_is_first_heard_this_year() {
        // "Old" was first heard last year, so it never qualifies despite more time.
        let mut rows = plays_of(T0 - 10 * DAY, 1, "Old", "O");
        rows.extend(plays_of(T0 + DAY, 20, "Old", "O"));
        rows.extend(plays_of(T0 + 2 * DAY, 5, "New", "N"));
        rows.extend(plays_of(T0 + 3 * DAY, 4, "Almost", "A"));
        let found = year_2026(&rows).discovery.expect("discovery");
        assert_eq!(found.name, "New");
        assert_eq!(found.first_played, T0 + 2 * DAY);
        assert_eq!((found.plays, found.listened_ms), (5, 5 * 180_000));
    }

    #[test]
    fn discovery_needs_five_counted_plays() {
        let rows = plays_of(T0 + DAY, 4, "New", "N");
        assert!(year_2026(&rows).discovery.is_none());
    }

    #[test]
    fn peak_time_picks_weekday_and_three_hour_band() {
        // 2026-01-06 is a Tuesday (weekday 1). 19:00 local falls in the 18 band.
        let tuesday_evening = T0 + 5 * DAY + 19 * 3600;
        let rows = vec![
            listened(play(tuesday_evening, "A", "A"), 8 * HOUR_MS),
            listened(play(T0 + 6 * DAY + 3600, "B", "B"), 3 * HOUR_MS),
        ];
        let peak = year_2026(&rows).peak_time.expect("peak");
        assert_eq!((peak.weekday, peak.band_start_hour), (1, 18));
    }

    #[test]
    fn peak_time_needs_ten_hours() {
        let rows = vec![listened(play(T0 + DAY, "A", "A"), 10 * HOUR_MS - 1)];
        assert!(year_2026(&rows).peak_time.is_none());
        let rows = vec![listened(play(T0 + DAY, "A", "A"), 10 * HOUR_MS)];
        assert!(year_2026(&rows).peak_time.is_some());
    }

    #[test]
    fn most_skipped_is_the_track_with_most_skips_from_five() {
        let mut rows: Vec<_> = (0..5).map(|i| skip(T0 + DAY + i, "A", "Hated")).collect();
        rows.extend((0..4).map(|i| skip(T0 + 2 * DAY + i, "B", "Meh")));
        // A skim is not a skip, and radio never is.
        rows.push(PlayRow {
            listened_ms: Some(1_000),
            ..skip(T0 + 3 * DAY, "A", "Hated")
        });
        let found = year_2026(&rows).most_skipped.expect("most skipped");
        assert_eq!(
            (found.title.as_str(), found.skips, found.started),
            ("Hated", 5, 5)
        );
        rows.pop();
        rows.remove(0);
        assert!(year_2026(&rows).most_skipped.is_none());
    }

    #[test]
    fn top_station_needs_an_hour() {
        let rows = vec![
            radio(T0 + DAY, "Jazz FM", 40 * 60_000),
            radio(T0 + 2 * DAY, "Jazz FM", 30 * 60_000),
            radio(T0 + 3 * DAY, "Rock FM", HOUR_MS / 2),
        ];
        let station = year_2026(&rows).top_station.expect("station");
        assert_eq!(
            (station.name.as_str(), station.listened_ms),
            ("Jazz FM", 70 * 60_000)
        );
        assert!(year_2026(&rows[2..]).top_station.is_none());
    }

    #[test]
    fn comeback_needs_a_gap_before_this_year() {
        // 760 days of history to `now` make G = 190 days.
        let early = T0 - 400 * DAY;
        let mut rows = plays_of(early, 1, "Steady", "S");
        // "Gone" last played 200 days before the year, then returns with 10 plays.
        rows.extend(plays_of(T0 - 200 * DAY, 1, "Gone", "G"));
        rows.extend(plays_of(T0 + 10 * DAY, 10, "Gone", "G"));
        // "Recent" played 20 days before the year: no gap, however many plays.
        rows.extend(plays_of(T0 - 20 * DAY, 1, "Recent", "R"));
        rows.extend(plays_of(T0 + 11 * DAY, 30, "Recent", "R"));
        // "Fresh" has no history at all.
        rows.extend(plays_of(T0 + 12 * DAY, 40, "Fresh", "F"));
        rows.sort_by_key(|row| row.played_at);
        let back = year_2026(&rows).comeback.expect("comeback");
        assert_eq!(back.name, "Gone");
        assert_eq!(back.plays, 10);
        assert_eq!(back.gap_days, 210);
    }

    #[test]
    fn comeback_needs_ten_plays() {
        let mut rows = plays_of(T0 - 400 * DAY, 1, "Gone", "G");
        rows.extend(plays_of(T0 + 10 * DAY, 9, "Gone", "G"));
        assert!(year_2026(&rows).comeback.is_none());
    }

    #[test]
    fn months_need_three_months_with_plays() {
        let mut rows = plays_of(T0 + DAY, 1, "Jan", "J");
        rows.extend(plays_of(T0 + 40 * DAY, 2, "Feb", "F"));
        assert!(year_2026(&rows).months.is_none());
        rows.extend(plays_of(T0 + 70 * DAY, 1, "Mar", "M"));
        let months = year_2026(&rows).months.expect("months");
        let shape: Vec<_> = months
            .iter()
            .map(|m| (m.month, m.listened_ms, m.top_artist.clone()))
            .collect();
        assert_eq!(
            shape,
            [
                (1, 180_000, Some("Jan".to_string())),
                (2, 360_000, Some("Feb".to_string())),
                (3, 180_000, Some("Mar".to_string())),
            ]
        );
    }

    #[test]
    fn through_is_now_for_the_current_year_and_year_end_otherwise() {
        let year_end = T0 + 365 * DAY;
        assert_eq!(year_2026(&[]).through, NOW);
        assert_eq!(run(&[], 2025, NOW).through, T0);
        assert_eq!(run(&[], 2026, year_end + DAY).through, year_end);
    }

    #[test]
    fn years_lists_years_with_counted_plays_newest_first() {
        let rows = vec![
            play(T0 - 400 * DAY, "A", "A"),
            // An uncounted play does not make its year (2024 has a counted play, 2023 does not).
            skip(T0 - 3 * 365 * DAY, "A", "A"),
            play(T0 + DAY, "A", "A"),
            play(T0 + 2 * DAY, "A", "A"),
            // Local time decides the year: this is 2025 in UTC and 2026 at +1h.
            play(T0 - 1800, "A", "A"),
        ];
        assert_eq!(years(&rows, &FixedZone(0)), [2026, 2025, 2024]);
        assert_eq!(years(&rows, &FixedZone(3600)), [2026, 2024]);
        assert!(years(&[], &FixedZone(0)).is_empty());
    }
}
