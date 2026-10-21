//! Scoring candidate releases, matching files to release tracks, and building
//! the values a lookup proposes. Pure: no I/O.
use super::{
    musicbrainz::{Credit, Release, credit_name},
    tags::FileTags,
};
use crate::protocol::{FieldChange, TagField};
use std::{collections::BTreeMap, path::PathBuf};

/// How far a file's duration may differ from a release track's length.
const DURATION_TOLERANCE_MS: u64 = 3000;

/// One file of the album, as the library knows it.
pub(super) struct AlbumTrack {
    pub track_id: u64,
    pub path: PathBuf,
    pub title: String,
    pub duration_ms: Option<u64>,
    pub disc_number: Option<u32>,
    pub track_number: Option<u32>,
    pub release_id: Option<String>,
    pub recording_id: Option<String>,
    pub release_track_id: Option<String>,
}

pub(super) struct AlbumFacts {
    pub title: String,
    pub artist: String,
    pub track_count: usize,
    pub duration_ms: Option<u64>,
}

/// What a candidate's source says about it.
#[derive(Clone, Copy, Default)]
pub(super) struct Evidence {
    /// MusicBrainz search score (0 to 100).
    pub search_score: Option<u32>,
    /// Number of fingerprinted tracks that voted for the release.
    pub votes: Option<u32>,
}

pub(super) fn score(album: &AlbumFacts, release: &Release, evidence: Evidence) -> u32 {
    let mut total = 0;
    if release.track_count() as usize == album.track_count {
        total += 1000;
    }
    if let (Some(a), Some(r)) = (album.duration_ms, release.duration_ms())
        && a.abs_diff(r) * 50 <= a
    {
        total += 300;
    }
    total += (200.0 * similarity(&album.title, &release.title)).round() as u32;
    total += evidence.search_score.unwrap_or(0);
    if album.track_count > 0 {
        total += 100 * evidence.votes.unwrap_or(0) / album.track_count as u32;
    }
    total
}

/// `1 - distance / longer length`, over trimmed, lowercased chars.
pub(super) fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (normal(a), normal(b));
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    1.0 - distance(&a, &b) as f64 / longest as f64
}

fn normal(text: &str) -> Vec<char> {
    text.trim().to_lowercase().chars().collect()
}

/// Levenshtein distance.
fn distance(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != cb))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// The release ID most files carry. Blank IDs are ignored. Ties go to the
/// smaller ID.
pub(super) fn most_common_release(tracks: &[AlbumTrack]) -> Option<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for id in tracks
        .iter()
        .filter_map(|t| t.release_id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        *counts.entry(id).or_default() += 1;
    }
    let mut best: Option<(&str, usize)> = None;
    for (id, count) in counts {
        if best.is_none_or(|(_, most)| count > most) {
            best = Some((id, count));
        }
    }
    best.map(|(id, _)| id.to_owned())
}

/// A track of a release: indexes into `media` and its `tracks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TrackRef {
    pub medium: usize,
    pub track: usize,
}

fn release_tracks(release: &Release) -> impl Iterator<Item = TrackRef> + '_ {
    release
        .media
        .iter()
        .enumerate()
        .flat_map(|(medium, m)| (0..m.tracks.len()).map(move |track| TrackRef { medium, track }))
}

/// The release track for each file, or `None`. Each release track is used at
/// most once.
pub(super) fn match_files(files: &[AlbumTrack], release: &Release) -> Vec<Option<TrackRef>> {
    let mut matched: Vec<Option<TrackRef>> = vec![None; files.len()];
    let mut used: Vec<TrackRef> = Vec::new();
    let refs: Vec<TrackRef> = release_tracks(release).collect();
    let track_at = |at: TrackRef| &release.media[at.medium].tracks[at.track];

    // Pass 1: release track ID or recording ID.
    pass(files, &mut matched, &mut used, |file, _| {
        refs.iter().copied().find(|&at| {
            let track = track_at(at);
            file.release_track_id.as_deref() == Some(track.id.as_str())
                || file.recording_id.as_deref() == Some(track.recording.id.as_str())
        })
    });

    // Pass 2: disc and track number. A missing disc number is disc 1.
    pass(files, &mut matched, &mut used, |file, used| {
        let number = file.track_number?;
        let disc = file.disc_number.unwrap_or(1);
        refs.iter().copied().find(|&at| {
            !used.contains(&at)
                && release.media[at.medium].position == disc
                && track_at(at).position == number
        })
    });

    // Pass 3: duration within tolerance, then closest title, then closest duration.
    pass(files, &mut matched, &mut used, |file, used| {
        let duration = file.duration_ms?;
        let title = normal(&file.title);
        refs.iter()
            .copied()
            .filter(|at| !used.contains(at))
            .filter_map(|at| {
                let track = track_at(at);
                let length = track.length.or(track.recording.length)?;
                let gap = duration.abs_diff(length);
                (gap <= DURATION_TOLERANCE_MS).then(|| {
                    let edit = distance(&title, &normal(&track.title));
                    ((edit, gap), at)
                })
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, at)| at)
    });
    matched
}

/// Gives each still-unmatched file the track `pick` finds for it. `pick` sees
/// the tracks used so far (it must skip them itself where it can tell them
/// apart; pass 1 is checked here).
fn pass(
    files: &[AlbumTrack],
    matched: &mut [Option<TrackRef>],
    used: &mut Vec<TrackRef>,
    pick: impl Fn(&AlbumTrack, &[TrackRef]) -> Option<TrackRef>,
) {
    for (file, slot) in files.iter().zip(matched.iter_mut()) {
        if slot.is_some() {
            continue;
        }
        if let Some(at) = pick(file, used).filter(|at| !used.contains(at)) {
            *slot = Some(at);
            used.push(at);
        }
    }
}

/// The value MusicBrainz gives each field of one track. Absent or empty
/// values are left out.
pub(super) fn proposed(release: &Release, at: TrackRef) -> BTreeMap<TagField, String> {
    let medium = &release.media[at.medium];
    let track = &medium.tracks[at.track];
    let credits: &[Credit] = if track.artist_credit.is_empty() {
        &track.recording.artist_credit
    } else {
        &track.artist_credit
    };
    let mut out = BTreeMap::new();
    let mut put = |field: TagField, value: String| {
        if !value.trim().is_empty() {
            out.insert(field, value);
        }
    };
    put(TagField::Title, track.title.clone());
    put(TagField::Artist, credit_name(credits));
    put(TagField::Album, release.title.clone());
    put(TagField::AlbumArtist, release.artist());
    put(TagField::TrackNumber, track.position.to_string());
    put(TagField::TrackTotal, medium.track_count.to_string());
    put(TagField::DiscNumber, medium.position.to_string());
    put(TagField::DiscTotal, release.media.len().to_string());
    put(TagField::Date, release.date.clone().unwrap_or_default());
    put(
        TagField::OriginalDate,
        release
            .release_group
            .as_ref()
            .and_then(|g| g.first_release_date.clone())
            .unwrap_or_default(),
    );
    put(TagField::MusicbrainzRecordingId, track.recording.id.clone());
    put(TagField::MusicbrainzTrackId, track.id.clone());
    put(TagField::MusicbrainzReleaseId, release.id.clone());
    put(
        TagField::MusicbrainzReleaseGroupId,
        release
            .release_group
            .as_ref()
            .map(|g| g.id.clone())
            .unwrap_or_default(),
    );
    put(
        TagField::MusicbrainzArtistId,
        credits
            .first()
            .map(|c| c.artist.id.clone())
            .unwrap_or_default(),
    );
    put(
        TagField::MusicbrainzAlbumArtistId,
        release
            .artist_credit
            .first()
            .map(|c| c.artist.id.clone())
            .unwrap_or_default(),
    );
    out
}

/// The proposed values that differ from the file's current ones, in `TagField`
/// order. Numbers compare as numbers; everything else as exact strings.
pub(super) fn changes(tags: &FileTags, proposed: &BTreeMap<TagField, String>) -> Vec<FieldChange> {
    proposed
        .iter()
        .filter_map(|(&field, new)| {
            let old = tags.current(field);
            let same = match old {
                None => false,
                Some(old) if field.is_number() => {
                    match (old.trim().parse::<u32>(), new.parse::<u32>()) {
                        (Ok(a), Ok(b)) => a == b,
                        _ => old == new,
                    }
                }
                Some(old) => old == new,
            };
            (!same).then(|| FieldChange {
                field,
                old: old.map(str::to_owned),
                new: new.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{
        tags::{TagKind, WritableFormat},
        test_support::fixture,
    };
    use super::*;

    fn release(name: &str) -> Release {
        serde_json::from_slice(&fixture(name)).unwrap()
    }
    fn album(title: &str, tracks: usize, duration_ms: Option<u64>) -> AlbumFacts {
        AlbumFacts {
            title: title.into(),
            artist: "Fixture Artist".into(),
            track_count: tracks,
            duration_ms,
        }
    }
    fn file(duration_ms: Option<u64>, disc: Option<u32>, number: Option<u32>) -> AlbumTrack {
        AlbumTrack {
            track_id: 1,
            path: PathBuf::from("x.flac"),
            title: String::new(),
            duration_ms,
            disc_number: disc,
            track_number: number,
            release_id: None,
            recording_id: None,
            release_track_id: None,
        }
    }
    fn at(medium: usize, track: usize) -> Option<TrackRef> {
        Some(TrackRef { medium, track })
    }
    fn tags(values: &[(TagField, &str)]) -> FileTags {
        FileTags {
            format: WritableFormat::Flac,
            primary_kind: TagKind::VorbisComments,
            id3v23: false,
            primary: values.iter().map(|(f, v)| (*f, v.to_string())).collect(),
            copies: vec![],
        }
    }

    #[test]
    fn score_prefers_track_count_then_duration_then_title_then_search_score() {
        let facts = album("Fixture Album", 3, Some(750));
        let a = Evidence {
            search_score: Some(100),
            votes: None,
        };
        let b = Evidence {
            search_score: Some(90),
            votes: None,
        };
        assert_eq!(score(&facts, &release("release-a.json"), a), 1600);
        assert_eq!(score(&facts, &release("release-b.json"), b), 290);
        // votes: 100 * 2 / 3 = 66
        let voted = Evidence {
            search_score: None,
            votes: Some(2),
        };
        assert_eq!(score(&facts, &release("release-a.json"), voted), 1566);
    }

    #[test]
    fn zero_track_release_duration_is_unknown() {
        let mut empty = release("release-a.json");
        empty.media.clear();
        // 0 tracks vs 0 album tracks: count matches, duration never does.
        let facts = album("Fixture Album", 0, Some(0));
        assert_eq!(score(&facts, &empty, Evidence::default()), 1200);
    }

    #[test]
    fn similarity_bounds() {
        assert_eq!(similarity("Same", " same "), 1.0);
        assert_eq!(similarity("", ""), 1.0);
        assert_eq!(similarity("abc", "xyz"), 0.0);
    }

    #[test]
    fn files_match_by_id_then_position_then_duration() {
        let release = release("release-a.json");
        let mut f0 = file(None, None, None);
        f0.release_track_id = Some("a1000000-0000-4000-8000-000000000003".into());
        let f1 = file(None, None, Some(1));
        let mut f2 = file(Some(251), None, None);
        f2.title = "two".into();
        let f3 = file(Some(10_000), None, None);
        assert_eq!(
            match_files(&[f0, f1, f2, f3], &release),
            [at(0, 2), at(0, 0), at(0, 1), None]
        );
    }

    #[test]
    fn recording_id_matches_too() {
        let release = release("release-a.json");
        let mut f = file(None, None, None);
        f.recording_id = Some("b1000000-0000-4000-8000-000000000002".into());
        assert_eq!(match_files(&[f], &release), [at(0, 1)]);
    }

    #[test]
    fn duration_pass_prefers_closest_title_then_closest_duration() {
        let mut release = release("release-a.json");
        release.media[0].tracks[0].length = Some(10_000);
        release.media[0].tracks[1].length = Some(10_500);
        release.media[0].tracks[2].length = Some(10_200);
        let mut by_title = file(Some(10_100), None, None);
        by_title.title = "third".into();
        assert_eq!(match_files(&[by_title], &release), [at(0, 2)]);
        // Same (empty-vs-title) distance: the smaller duration gap wins.
        release.media[0].tracks[0].title = "x".into();
        release.media[0].tracks[1].title = "x".into();
        release.media[0].tracks[2].title = "x".into();
        let mut by_gap = file(Some(10_190), None, None);
        by_gap.title = "x".into();
        assert_eq!(match_files(&[by_gap], &release), [at(0, 2)]);
    }

    #[test]
    fn each_release_track_matches_at_most_once() {
        let release = release("release-a.json");
        let files = [
            file(Some(250), None, Some(1)),
            file(Some(99_000), None, Some(1)),
        ];
        assert_eq!(match_files(&files, &release), [at(0, 0), None]);
    }

    #[test]
    fn missing_disc_number_is_disc_one_and_null_lengths_are_skipped() {
        let release = release("release-c.json");
        let files = [
            file(None, None, Some(2)),
            file(Some(250), Some(2), Some(2)),
            file(None, Some(2), Some(3)),
        ];
        assert_eq!(match_files(&files, &release), [at(0, 1), at(1, 1), None]);
        // Disc 2 track 1 has no length: duration matching skips it without panicking.
        assert_eq!(
            match_files(&[file(Some(20_000), None, None)], &release),
            [None]
        );
    }

    #[test]
    fn proposed_values_follow_the_spec_table() {
        let release = release("release-a.json");
        let got = proposed(
            &release,
            TrackRef {
                medium: 0,
                track: 1,
            },
        );
        let want: BTreeMap<TagField, String> = [
            (TagField::Title, "Second"),
            (TagField::Artist, "Fixture Artist feat. Guest"),
            (TagField::Album, "Fixture Album"),
            (TagField::AlbumArtist, "Fixture Artist"),
            (TagField::TrackNumber, "2"),
            (TagField::TrackTotal, "3"),
            (TagField::DiscNumber, "1"),
            (TagField::DiscTotal, "1"),
            (TagField::Date, "2001-05-14"),
            (TagField::OriginalDate, "1999-02-01"),
            (
                TagField::MusicbrainzRecordingId,
                "b1000000-0000-4000-8000-000000000002",
            ),
            (
                TagField::MusicbrainzTrackId,
                "a1000000-0000-4000-8000-000000000002",
            ),
            (
                TagField::MusicbrainzReleaseId,
                "11111111-1111-4111-8111-111111111111",
            ),
            (
                TagField::MusicbrainzReleaseGroupId,
                "99999999-9999-4999-8999-999999999999",
            ),
            (
                TagField::MusicbrainzArtistId,
                "c1000000-0000-4000-8000-000000000002",
            ),
            (
                TagField::MusicbrainzAlbumArtistId,
                "c1000000-0000-4000-8000-000000000002",
            ),
        ]
        .into_iter()
        .map(|(f, v)| (f, v.to_string()))
        .collect();
        assert_eq!(got.len(), 16);
        assert_eq!(got, want);
    }

    #[test]
    fn absent_or_empty_musicbrainz_values_are_not_proposed() {
        let b = release("release-b.json");
        let got = proposed(
            &b,
            TrackRef {
                medium: 0,
                track: 0,
            },
        );
        assert!(!got.contains_key(&TagField::OriginalDate));
        assert!(got.contains_key(&TagField::Date));
        let mut a = release("release-a.json");
        a.date = None;
        a.release_group = None;
        let got = proposed(
            &a,
            TrackRef {
                medium: 0,
                track: 0,
            },
        );
        assert!(!got.contains_key(&TagField::Date));
        assert!(!got.contains_key(&TagField::OriginalDate));
        assert!(!got.contains_key(&TagField::MusicbrainzReleaseGroupId));
    }

    #[test]
    fn track_artist_falls_back_to_recording_credit() {
        let mut a = release("release-a.json");
        a.media[0].tracks[1].artist_credit.clear();
        let got = proposed(
            &a,
            TrackRef {
                medium: 0,
                track: 1,
            },
        );
        assert_eq!(got[&TagField::Artist], "Fixture Artist feat. Guest");
    }

    #[test]
    fn only_changed_fields_are_proposed_and_numbers_compare_numerically() {
        let want: BTreeMap<TagField, String> = [
            (TagField::Album, "Fixture Album"),
            (TagField::TrackNumber, "3"),
            (TagField::Title, "Third"),
            (TagField::DiscNumber, "1"),
        ]
        .into_iter()
        .map(|(f, v)| (f, v.to_string()))
        .collect();
        let current = tags(&[
            (TagField::Album, "Old Album"),
            (TagField::TrackNumber, "03"),
            (TagField::Title, "Third"),
            (TagField::DiscNumber, "x"),
        ]);
        assert_eq!(
            changes(&current, &want),
            [
                FieldChange {
                    field: TagField::Album,
                    old: Some("Old Album".into()),
                    new: "Fixture Album".into(),
                },
                FieldChange {
                    field: TagField::DiscNumber,
                    old: Some("x".into()),
                    new: "1".into(),
                },
            ]
        );
        let none = tags(&[]);
        assert!(changes(&none, &want).iter().all(|c| c.old.is_none()));
        assert_eq!(changes(&none, &want).len(), 4);
    }

    #[test]
    fn most_common_release_ignores_blanks() {
        let with = |id: Option<&str>| AlbumTrack {
            release_id: id.map(str::to_owned),
            ..file(None, None, None)
        };
        let tracks = [
            with(Some("")),
            with(Some("  ")),
            with(None),
            with(Some("r2")),
            with(Some("r1")),
            with(Some("r2")),
        ];
        assert_eq!(most_common_release(&tracks).as_deref(), Some("r2"));
        assert_eq!(most_common_release(&tracks[..3]), None);
    }
}
