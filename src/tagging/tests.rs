//! Lookups against scanned fixture albums and loopback providers. Tests never
//! touch the network.
use super::{
    ApplyRequest, Failure, LOOKUP_DEADLINE, LookupRequest, TagOutcome, TagRequest, TaggingWorkers,
    acoustid::AcoustId,
    engine::{Engine, Providers},
    journal::{FileStatus, Journal},
    musicbrainz::MusicBrainz,
    proposal::{PROPOSAL_TTL, Proposal, Proposals},
    tags::{CopyKey, FileTags, TagCopy, TagEdit, TagKind, edit_tags, read_tags},
    test_support::{HttpFixture, LibraryFixture, Reply, fixture, json},
};
use crate::cancel::Cancellation;
use crate::protocol::{
    ErrorCode, FieldChange, TagApplyResult, TagCandidate, TagField, TagFileOutcome, TagProposal,
};
use std::{
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const A: &str = "11111111-1111-4111-8111-111111111111";
const B: &str = "22222222-2222-4222-8222-222222222222";
const C: &str = "33333333-3333-4333-8333-333333333333";

fn release_target(id: &str) -> String {
    format!("/ws/2/release/{id}?inc=recordings+artist-credits+release-groups+media&fmt=json")
}
/// MusicBrainz: `search` answers searches; releases A, B and C are served.
fn musicbrainz(search: &'static str) -> HttpFixture {
    HttpFixture::new(move |target| releases_or(target, search))
}
fn releases_or(target: &str, search: &str) -> Reply {
    if target.starts_with("/ws/2/release?") {
        return json(200, fixture(search));
    }
    let name = [(A, "a"), (B, "b"), (C, "c")]
        .iter()
        .find(|(id, _)| target.contains(id))
        .map(|(_, n)| *n)
        .unwrap_or_else(|| panic!("unexpected target {target}"));
    json(200, fixture(&format!("release-{name}.json")))
}
fn providers(musicbrainz: &HttpFixture, acoustid: Option<&HttpFixture>) -> Providers {
    Providers {
        musicbrainz: MusicBrainz::for_test(&musicbrainz.origin(), Duration::ZERO)
            .map_err(|e| e.to_string()),
        acoustid: match acoustid {
            Some(fixture) => {
                AcoustId::for_test(&fixture.origin(), Duration::ZERO).map_err(|e| e.to_string())
            }
            None => Err("no AcoustID in this test".into()),
        },
    }
}
fn request(album: &LibraryFixture) -> LookupRequest {
    LookupRequest {
        album_id: album.album_id,
        library_revision: album.revision,
        release_id: None,
        fingerprint: false,
        acoustid_key: None,
    }
}
fn lookup(
    album: &LibraryFixture,
    providers: Providers,
    request: &LookupRequest,
) -> Result<TagProposal, Failure> {
    Engine::open(&album.database, providers).unwrap().lookup(
        request,
        &Cancellation::new(),
        deadline(),
    )
}
fn deadline() -> Instant {
    Instant::now() + LOOKUP_DEADLINE
}
fn targets(fixture: &HttpFixture) -> Vec<String> {
    fixture.seen().into_iter().map(|s| s.target).collect()
}
fn refused(result: Result<TagProposal, Failure>) -> (ErrorCode, String) {
    let failure = result.expect_err("lookup should fail");
    (failure.code, failure.message)
}
/// Three FLAC copies tagged with release A and track numbers 1 to 3.
fn stored_a() -> LibraryFixture {
    LibraryFixture::new(&[
        (
            "tone.flac",
            &[
                (TagField::MusicbrainzReleaseId, A),
                (TagField::TrackNumber, "1"),
            ],
        ),
        (
            "tone.flac",
            &[
                (TagField::MusicbrainzReleaseId, A),
                (TagField::TrackNumber, "2"),
            ],
        ),
        (
            "tone.flac",
            &[
                (TagField::MusicbrainzReleaseId, A),
                (TagField::TrackNumber, "3"),
            ],
        ),
    ])
}
/// Three FLAC copies with album "Fixture Album" and no stored IDs.
fn unidentified() -> LibraryFixture {
    let tags: &[(TagField, &str)] = &[(TagField::Album, "Fixture Album")];
    LibraryFixture::new(&[
        ("tone.flac", tags),
        ("tone.flac", tags),
        ("tone.flac", tags),
    ])
}

#[test]
fn lookup_by_stored_release_id_fetches_only_that_release() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let proposal = lookup(&album, providers(&server, None), &request(&album)).unwrap();
    assert_eq!(targets(&server), [release_target(A)]);
    assert_eq!(proposal.release_id, A);
    assert_eq!(proposal.candidates.len(), 1);
    assert_eq!(proposal.tracks.len(), 3);
    for (number, track) in (1..).zip(&proposal.tracks) {
        assert!(track.matched && track.writable, "{track:?}");
        assert_eq!(track.track_number, Some(number));
        assert_eq!(track.reason, None);
        assert!(
            track.changes.contains(&FieldChange {
                field: TagField::Album,
                old: Some("Old Album".into()),
                new: "Fixture Album".into(),
            }),
            "{track:?}"
        );
    }
}

#[test]
fn lookup_by_search_scores_and_chooses_best() {
    let album = unidentified();
    let server = musicbrainz("search.json");
    let proposal = lookup(&album, providers(&server, None), &request(&album)).unwrap();
    let seen = targets(&server);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen[0].starts_with("/ws/2/release?query="), "{seen:?}");
    assert_eq!(seen[1..], [release_target(A), release_target(B)]);
    assert_eq!(proposal.release_id, A);
    let scores: Vec<_> = proposal
        .candidates
        .iter()
        .map(|c| (c.release_id.as_str(), c.score))
        .collect();
    // A: 1000 tracks + 300 duration + 200 title + 100 search; B: 200 title + 90 search.
    assert_eq!(scores, [(A, 1600), (B, 290)]);
    assert_eq!(
        proposal.candidates[1],
        TagCandidate {
            release_id: B.into(),
            title: "Fixture Album".into(),
            artist: "Fixture Artist".into(),
            date: Some("2003".into()),
            country: Some("US".into()),
            track_count: 4,
            score: 290,
        }
    );
    assert!(proposal.tracks.iter().all(|t| t.matched && t.writable));
}

#[test]
fn empty_search_retries_without_track_count_then_reports_no_match() {
    let album = unidentified();
    let server = musicbrainz("search-empty.json");
    let result = lookup(&album, providers(&server, None), &request(&album));
    let seen = targets(&server);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen.iter().all(|t| t.starts_with("/ws/2/release?query=")));
    assert!(seen[0].contains("tracks%3A3"), "{}", seen[0]);
    assert!(!seen[1].contains("tracks%3A"), "{}", seen[1]);
    assert_eq!(
        refused(result),
        (
            ErrorCode::Lookup,
            "MusicBrainz has no matching release".into()
        )
    );
}

#[test]
fn a_search_candidate_that_fails_to_load_is_left_out() {
    let album = unidentified();
    let missing = HttpFixture::new(|target| {
        if target.contains(B) {
            return json(404, b"{}".to_vec());
        }
        releases_or(target, "search.json")
    });
    let proposal = lookup(&album, providers(&missing, None), &request(&album)).unwrap();
    assert_eq!(
        targets(&missing)[1..],
        [release_target(A), release_target(B)]
    );
    let ids: Vec<_> = proposal.candidates.iter().map(|c| &c.release_id).collect();
    assert_eq!(ids, [A]);
    assert_eq!(proposal.release_id, A);

    // Every candidate failing gives the first failure.
    let none = HttpFixture::new(|target| {
        if target.starts_with("/ws/2/release?") {
            return json(200, fixture("search.json"));
        }
        json(404, b"{}".to_vec())
    });
    assert_eq!(
        refused(lookup(&album, providers(&none, None), &request(&album))),
        (ErrorCode::Lookup, "MusicBrainz has no such record".into())
    );

    // A busy reply still stops the lookup.
    let busy = HttpFixture::new(|target| {
        if target.contains(B) {
            return json(503, b"{}".to_vec());
        }
        releases_or(target, "search.json")
    });
    assert_eq!(
        refused(lookup(&album, providers(&busy, None), &request(&album))),
        (
            ErrorCode::Lookup,
            "MusicBrainz is busy, try again shortly".into()
        )
    );
}

#[test]
fn explicit_release_id_is_the_only_candidate() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let request = LookupRequest {
        release_id: Some(B.into()),
        ..request(&album)
    };
    let proposal = lookup(&album, providers(&server, None), &request).unwrap();
    assert_eq!(targets(&server), [release_target(B)]);
    assert_eq!(proposal.release_id, B);
    let ids: Vec<_> = proposal.candidates.iter().map(|c| &c.release_id).collect();
    assert_eq!(ids, [B]);
}

#[test]
fn invalid_release_id_is_refused_before_http() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    for id in [
        "",
        "not-an-id",
        "C1000000-0000-4000-8000-000000000002",
        &format!("{A}?x=1"),
    ] {
        let request = LookupRequest {
            release_id: Some(id.into()),
            ..request(&album)
        };
        assert_eq!(
            refused(lookup(&album, providers(&server, None), &request)),
            (
                ErrorCode::InvalidCommand,
                "release_id must be a MusicBrainz ID".into()
            ),
            "{id}"
        );
    }
    assert!(server.seen().is_empty());
}

#[test]
fn lookup_by_fingerprint_ranks_acoustid_votes() {
    let album = LibraryFixture::new(&[
        ("tone10.wav", &[]),
        ("tone10.wav", &[]),
        ("tone10.wav", &[]),
    ]);
    let server = musicbrainz("search.json");
    let acoustid = HttpFixture::new(|_| json(200, fixture("acoustid.json")));
    let request = LookupRequest {
        fingerprint: true,
        acoustid_key: Some("KEY123".into()),
        ..request(&album)
    };
    let proposal = lookup(&album, providers(&server, Some(&acoustid)), &request).unwrap();
    let prints = targets(&acoustid);
    assert_eq!(prints.len(), 3);
    assert!(
        prints.iter().all(|t| t.contains("client=KEY123")),
        "{prints:?}"
    );
    // Every file votes B then A, so B is fetched first.
    assert_eq!(targets(&server), [release_target(B), release_target(A)]);
    let scores: Vec<_> = proposal
        .candidates
        .iter()
        .map(|c| (c.release_id.as_str(), c.score))
        .collect();
    // A: 1000 tracks + 92 "Old Album" vs "Fixture Album" + 100 votes; B: 92 + 100.
    assert_eq!(scores, [(A, 1192), (B, 192)]);
    assert_eq!(proposal.release_id, A);
}

#[test]
fn fingerprint_requires_a_valid_key() {
    let album = LibraryFixture::new(&[("tone.flac", &[])]);
    let server = musicbrainz("search.json");
    let acoustid = HttpFixture::new(|_| json(200, fixture("acoustid.json")));
    let bad_key = "acoustid_key must be 1 to 64 letters or digits";
    let cases = [
        (None, "fingerprint lookup needs an AcoustID key"),
        (Some(String::new()), bad_key),
        (Some("KEY 123".into()), bad_key),
        (Some("KEY-123".into()), bad_key),
        (Some("k".repeat(65)), bad_key),
    ];
    for (key, message) in cases {
        let request = LookupRequest {
            fingerprint: true,
            acoustid_key: key.clone(),
            ..request(&album)
        };
        assert_eq!(
            refused(lookup(
                &album,
                providers(&server, Some(&acoustid)),
                &request
            )),
            (ErrorCode::InvalidCommand, message.into()),
            "{key:?}"
        );
    }
    assert!(server.seen().is_empty() && acoustid.seen().is_empty());
}

#[test]
fn stale_revision_and_unknown_album_fail_before_network() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let stale = LookupRequest {
        library_revision: album.revision + 1,
        ..request(&album)
    };
    assert_eq!(
        refused(lookup(&album, providers(&server, None), &stale)),
        (
            ErrorCode::StaleLibraryRevision,
            format!("library revision is {}", album.revision)
        )
    );
    for album_id in [album.album_id + 999, u64::MAX] {
        let unknown = LookupRequest {
            album_id,
            ..request(&album)
        };
        assert_eq!(
            refused(lookup(&album, providers(&server, None), &unknown)),
            (ErrorCode::InvalidCommand, "unknown album".into())
        );
    }
    assert!(server.seen().is_empty());
}

#[test]
fn current_values_come_from_the_file_not_the_database() {
    let album = LibraryFixture::new(&[(
        "tone.flac",
        &[
            (TagField::MusicbrainzReleaseId, A),
            (TagField::TrackNumber, "1"),
        ],
    )]);
    let db = rusqlite::Connection::open(&album.database).unwrap();
    let metadata: String = db
        .query_row("SELECT metadata FROM library_tracks", [], |r| r.get(0))
        .unwrap();
    let metadata: crate::library_types::TrackMetadata = serde_json::from_str(&metadata).unwrap();
    let stem = album.files[0].file_stem().unwrap().to_str().unwrap();
    assert_eq!(metadata.title, stem);
    let server = musicbrainz("search.json");
    let proposal = lookup(&album, providers(&server, None), &request(&album)).unwrap();
    assert!(
        proposal.tracks[0].changes.contains(&FieldChange {
            field: TagField::Title,
            old: None,
            new: "First".into(),
        }),
        "{:?}",
        proposal.tracks[0]
    );
}

#[test]
fn unwritable_and_unmatched_files_say_why() {
    let stored: &[(TagField, &str)] = &[(TagField::MusicbrainzReleaseId, A)];
    let album = LibraryFixture::new(&[
        (
            "tone.flac",
            &[
                (TagField::MusicbrainzReleaseId, A),
                (TagField::TrackNumber, "1"),
            ],
        ),
        (
            "tone.flac",
            &[
                (TagField::MusicbrainzReleaseId, A),
                (TagField::TrackNumber, "2"),
            ],
        ),
        (
            "tone.flac",
            &[
                (TagField::MusicbrainzReleaseId, A),
                (TagField::TrackNumber, "3"),
            ],
        ),
        ("tone.mka", stored),
        ("tone10.wav", stored),
    ]);
    let server = musicbrainz("search.json");
    let proposal = lookup(&album, providers(&server, None), &request(&album)).unwrap();
    assert_eq!(proposal.tracks.len(), 5);
    let row = |suffix: &str| {
        proposal
            .tracks
            .iter()
            .find(|t| t.file_name.ends_with(suffix))
            .unwrap()
    };
    let mka = row("tone.mka");
    assert!(!mka.writable, "{mka:?}");
    assert_eq!(
        mka.reason.as_deref(),
        Some("Tyranny can't write tags to this format")
    );
    assert!(mka.changes.is_empty());
    let wav = row("tone10.wav");
    assert!(!wav.matched && !wav.writable, "{wav:?}");
    assert_eq!(
        wav.reason.as_deref(),
        Some("No matching track on this release")
    );
    let flacs: Vec<_> = proposal
        .tracks
        .iter()
        .filter(|t| t.file_name.ends_with("tone.flac"))
        .collect();
    assert_eq!(flacs.len(), 3);
    assert!(
        flacs
            .iter()
            .all(|t| t.matched && t.writable && t.reason.is_none())
    );
}

#[test]
fn a_file_replaced_by_a_symlink_is_refused_at_lookup() {
    let album = stored_a();
    let target = album.root.join("elsewhere.flac");
    std::fs::rename(&album.files[0], &target).unwrap();
    std::os::unix::fs::symlink(&target, &album.files[0]).unwrap();
    let server = musicbrainz("search.json");
    let proposal = lookup(&album, providers(&server, None), &request(&album)).unwrap();
    let row = &proposal.tracks[0];
    assert!(row.matched && !row.writable, "{row:?}");
    assert_eq!(
        row.reason.as_deref(),
        Some("Tyranny couldn't read this file's tags: not a regular file")
    );
    assert!(proposal.tracks[1..].iter().all(|t| t.writable));
}

#[test]
fn a_changed_field_in_a_read_only_tag_makes_the_file_unwritable() {
    use lofty::{
        config::WriteOptions,
        id3::v2::Id3v2Tag,
        tag::{Accessor, TagExt},
    };
    let album = stored_a();
    // lofty can read an ID3v2 tag before a FLAC stream, but not edit it.
    let mut id3 = Id3v2Tag::new();
    id3.set_title("Old Title".to_owned());
    let mut bytes = Vec::new();
    id3.dump_to(&mut bytes, WriteOptions::default()).unwrap();
    bytes.extend(std::fs::read(&album.files[0]).unwrap());
    std::fs::write(&album.files[0], bytes).unwrap();
    let server = musicbrainz("search.json");
    let proposal = lookup(&album, providers(&server, None), &request(&album)).unwrap();
    let row = &proposal.tracks[0];
    assert!(
        row.matched && !row.writable && row.removes_other_copies,
        "{row:?}"
    );
    assert_eq!(
        row.reason.as_deref(),
        Some("Has an old ID3 tag Tyranny can't edit")
    );
    assert!(proposal.tracks[1..].iter().all(|t| t.writable));
}

#[test]
fn musicbrainz_busy_stops_the_lookup() {
    let album = stored_a();
    let server = HttpFixture::new(|_| json(503, b"{}".to_vec()));
    assert_eq!(
        refused(lookup(&album, providers(&server, None), &request(&album))),
        (
            ErrorCode::Lookup,
            "MusicBrainz is busy, try again shortly".into()
        )
    );
    assert_eq!(server.seen().len(), 1);
}

#[test]
fn proposals_expire_after_15_minutes_and_evict_the_oldest() {
    let empty = || Proposal {
        release_id: A.into(),
        release_title: "Fixture Album".into(),
        files: Vec::new(),
    };
    let now = Instant::now();
    let mut proposals = Proposals::default();
    let id = proposals.insert(now, empty());
    assert!(
        proposals
            .take(now + PROPOSAL_TTL + Duration::from_secs(1), &id)
            .is_none()
    );
    let id = proposals.insert(now, empty());
    assert!(
        proposals
            .take(now + PROPOSAL_TTL - Duration::from_secs(1), &id)
            .is_some()
    );
    assert!(proposals.take(now, &id).is_none(), "take consumes");

    let ids: Vec<String> = (0..9)
        .map(|i| proposals.insert(now + Duration::from_millis(i), empty()))
        .collect();
    let later = now + Duration::from_secs(1);
    assert!(proposals.take(later, &ids[0]).is_none());
    for id in &ids[1..] {
        assert!(proposals.take(later, id).is_some());
    }
}

#[test]
fn abandoned_lookup_stops_the_worker() {
    let album = unidentified();
    let first = AtomicBool::new(true);
    let server = HttpFixture::new(move |target| {
        let mut reply = releases_or(target, "search.json");
        if target.starts_with("/ws/2/release?") && first.swap(false, Ordering::AcqRel) {
            reply.delay = Duration::from_secs(2);
        }
        reply
    });
    let workers = TaggingWorkers::with_providers(
        album.database.clone(),
        Some(providers(&server, None)),
        None,
    );
    let handle = workers.handle();
    let proposal = block_on(async {
        let first = {
            let handle = handle.clone();
            let request = request(&album);
            tokio::spawn(async move { handle.request(TagRequest::Lookup(request)).await })
        };
        // Drop the first lookup's future once its search is on the wire.
        let waited = Instant::now();
        while server.seen().is_empty() {
            assert!(waited.elapsed() < Duration::from_secs(5), "no search sent");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        first.abort();
        assert!(first.await.is_err_and(|error| error.is_cancelled()));
        let explicit = LookupRequest {
            release_id: Some(B.into()),
            ..request(&album)
        };
        handle.request(TagRequest::Lookup(explicit)).await
    });
    let Ok(TagOutcome::Proposal(proposal)) = proposal else {
        panic!("lookup failed");
    };
    assert_eq!(proposal.release_id, B);
    let seen = targets(&server);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].starts_with("/ws/2/release?query="), "{seen:?}");
    assert_eq!(seen[1], release_target(B));
    assert!(
        !seen
            .iter()
            .any(|t| t.contains(&format!("/ws/2/release/{A}")))
    );
    workers.shutdown();
}

#[test]
fn a_full_queue_is_busy() {
    let album = unidentified();
    let server = HttpFixture::new(|target| {
        let mut reply = releases_or(target, "search.json");
        reply.delay = Duration::from_secs(1);
        reply
    });
    let workers = TaggingWorkers::with_providers(
        album.database.clone(),
        Some(providers(&server, None)),
        None,
    );
    let handle = workers.handle();
    let failure = block_on(async {
        // One lookup runs and four wait; the sixth finds the queue full.
        let mut waiting = Vec::new();
        for _ in 0..5 {
            let handle = handle.clone();
            let request = request(&album);
            waiting.push(tokio::spawn(async move {
                handle.request(TagRequest::Lookup(request)).await
            }));
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let failure = handle.request(TagRequest::Lookup(request(&album))).await;
        for task in waiting {
            task.abort();
        }
        failure
    })
    .err()
    .expect("queue is full");
    assert_eq!(
        (failure.code, failure.message.as_str()),
        (
            ErrorCode::InvalidCommand,
            "tag lookups are busy, try again shortly"
        )
    );
    workers.shutdown();
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

// Apply, revert and the edit list.

fn engine(album: &LibraryFixture, server: &HttpFixture) -> Engine {
    Engine::open(&album.database, providers(server, None)).unwrap()
}
fn journal(album: &LibraryFixture) -> Journal {
    Journal::open(&album.database.with_extension("tagging.sqlite")).unwrap()
}
/// The tracks a proposal would write.
fn writes(proposal: &TagProposal) -> Vec<u64> {
    proposal
        .tracks
        .iter()
        .filter(|t| t.writable && !t.changes.is_empty())
        .map(|t| t.track_id)
        .collect()
}
fn apply_request(proposal: &TagProposal, omit_fields: &[TagField]) -> ApplyRequest {
    ApplyRequest {
        proposal_id: proposal.proposal_id.clone(),
        track_ids: writes(proposal),
        omit_fields: omit_fields.to_vec(),
    }
}
fn running() -> AtomicBool {
    AtomicBool::new(false)
}
fn outcomes(result: &TagApplyResult) -> Vec<(TagFileOutcome, Option<&str>)> {
    result
        .files
        .iter()
        .map(|f| (f.outcome, f.message.as_deref()))
        .collect()
}
fn bytes(album: &LibraryFixture) -> Vec<Vec<u8>> {
    album
        .files
        .iter()
        .map(|p| std::fs::read(p).unwrap())
        .collect()
}
fn refused_apply<T>(result: Result<T, Failure>) -> (ErrorCode, String) {
    let failure = result.err().expect("request should fail");
    (failure.code, failure.message)
}
fn expired() -> (ErrorCode, String) {
    (
        ErrorCode::InvalidCommand,
        "lookup expired, look the album up again".into(),
    )
}
const WRITTEN: (TagFileOutcome, Option<&str>) = (TagFileOutcome::Written, None);
const CHANGED: (TagFileOutcome, Option<&str>) =
    (TagFileOutcome::Skipped, Some("changed since the lookup"));
const MB_IDS: [TagField; 6] = [
    TagField::MusicbrainzRecordingId,
    TagField::MusicbrainzTrackId,
    TagField::MusicbrainzReleaseId,
    TagField::MusicbrainzReleaseGroupId,
    TagField::MusicbrainzArtistId,
    TagField::MusicbrainzAlbumArtistId,
];

#[test]
fn apply_writes_proposal_values_and_journals_before_and_after() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let before: Vec<FileTags> = album.files.iter().map(|p| read_tags(p).unwrap()).collect();
    let apply = apply_request(&proposal, &[]);
    assert_eq!(apply.track_ids.len(), 3);
    let (result, written) = engine.apply(&apply, &running()).unwrap();
    assert_eq!(written, album.files);
    assert_eq!(outcomes(&result), [WRITTEN; 3]);
    let edit_id = result.edit_id.expect("the edit is journaled");
    let record = journal(&album).load(edit_id).unwrap().unwrap();
    assert_eq!(
        (record.release_id.as_str(), record.release_title.as_str()),
        (A, "Fixture Album")
    );
    assert!(!record.reverted);
    assert_eq!(record.files.len(), 3);
    for (index, (track, (file, status))) in proposal.tracks.iter().zip(&record.files).enumerate() {
        assert_eq!(*status, FileStatus::Written);
        assert_eq!(result.files[index].track_id, track.track_id);
        assert_eq!(result.files[index].file_name, track.file_name);
        assert_eq!(
            (file.track_id, &file.path),
            (track.track_id, &album.files[index])
        );
        let now = read_tags(&album.files[index]).unwrap();
        assert_eq!(file.after.len(), track.changes.len());
        for change in &track.changes {
            assert_eq!(
                now.primary.get(&change.field),
                Some(&change.new),
                "{change:?}"
            );
            assert_eq!(file.after[&change.field].as_ref(), Some(&change.new));
            assert_eq!(
                file.before[&change.field].as_ref(),
                before[index].primary.get(&change.field)
            );
        }
    }
    assert_eq!(refused_apply(engine.apply(&apply, &running())), expired());
}

#[test]
fn omitted_fields_are_left_unchanged_and_the_rest_written() {
    let tagged = |number: &'static str| -> Vec<(TagField, &'static str)> {
        vec![
            (TagField::MusicbrainzReleaseId, A),
            (TagField::TrackNumber, number),
            (TagField::Title, "Old Title"),
        ]
    };
    let (one, two, three) = (tagged("1"), tagged("2"), tagged("3"));
    let album = LibraryFixture::new(&[
        ("tone.flac", &one),
        ("tone.flac", &two),
        ("tone.flac", &three),
    ]);
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    for track in &proposal.tracks {
        let fields: Vec<_> = track.changes.iter().map(|c| c.field).collect();
        assert!(
            fields.contains(&TagField::Title) && fields.contains(&TagField::Album),
            "{track:?}"
        );
    }
    let (result, written) = engine
        .apply(&apply_request(&proposal, &[TagField::Title]), &running())
        .unwrap();
    assert_eq!(outcomes(&result), [WRITTEN; 3]);
    assert_eq!(written, album.files);
    for path in &album.files {
        let tags = read_tags(path).unwrap();
        assert_eq!(tags.current(TagField::Title), Some("Old Title"));
        assert_eq!(tags.current(TagField::Album), Some("Fixture Album"));
    }
    let record = journal(&album)
        .load(result.edit_id.unwrap())
        .unwrap()
        .unwrap();
    for (file, _) in &record.files {
        assert!(!file.before.contains_key(&TagField::Title), "{file:?}");
        assert!(!file.after.contains_key(&TagField::Title), "{file:?}");
        assert!(file.after.contains_key(&TagField::Album), "{file:?}");
    }
}

#[test]
fn unknown_track_rejects_the_whole_apply() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let original = bytes(&album);
    let mut apply = apply_request(&proposal, &[]);
    apply.track_ids.push(999_999);
    assert_eq!(
        refused_apply(engine.apply(&apply, &running())),
        (
            ErrorCode::InvalidCommand,
            "track 999999 is not part of this lookup".into()
        )
    );
    assert_eq!(bytes(&album), original);
    // Decision 10: the proposal is consumed.
    assert_eq!(
        refused_apply(engine.apply(&apply_request(&proposal, &[]), &running())),
        expired()
    );
    assert!(journal(&album).recent(50).unwrap().is_empty());
}

#[test]
fn empty_and_repeated_track_lists_are_refused() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let mut apply = apply_request(&proposal, &[]);
    apply.track_ids.clear();
    assert_eq!(
        refused_apply(engine.apply(&apply, &running())),
        (ErrorCode::InvalidCommand, "choose at least one file".into())
    );
    let first = proposal.tracks[0].track_id;
    apply.track_ids = vec![first, first];
    assert_eq!(
        refused_apply(engine.apply(&apply, &running())),
        (
            ErrorCode::InvalidCommand,
            format!("track {first} is listed twice")
        )
    );
    let (result, _) = engine
        .apply(&apply_request(&proposal, &[]), &running())
        .unwrap();
    assert_eq!(outcomes(&result), [WRITTEN; 3]);
}

#[test]
fn apply_skips_a_file_changed_since_the_lookup() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let mut changed = std::fs::OpenOptions::new()
        .append(true)
        .open(&album.files[1])
        .unwrap();
    std::io::Write::write_all(&mut changed, b"x").unwrap();
    drop(changed);
    let kept = std::fs::read(&album.files[1]).unwrap();
    let (result, written) = engine
        .apply(&apply_request(&proposal, &[]), &running())
        .unwrap();
    assert_eq!(outcomes(&result), [WRITTEN, CHANGED, WRITTEN]);
    assert_eq!(written, [album.files[0].clone(), album.files[2].clone()]);
    assert_eq!(std::fs::read(&album.files[1]).unwrap(), kept);
    let record = journal(&album)
        .load(result.edit_id.unwrap())
        .unwrap()
        .unwrap();
    let journaled: Vec<_> = record.files.iter().map(|(f, s)| (f.track_id, *s)).collect();
    assert_eq!(
        journaled,
        [
            (proposal.tracks[0].track_id, FileStatus::Written),
            (proposal.tracks[2].track_id, FileStatus::Written),
        ]
    );
}

#[test]
fn deleted_file_between_lookup_and_apply_is_skipped() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    std::fs::remove_file(&album.files[0]).unwrap();
    let (result, written) = engine
        .apply(&apply_request(&proposal, &[]), &running())
        .unwrap();
    assert_eq!(outcomes(&result), [CHANGED, WRITTEN, WRITTEN]);
    assert_eq!(written, album.files[1..]);
    assert!(!album.files[0].exists());
}

/// Makes a directory writable again when dropped.
struct Writable(std::path::PathBuf);
impl Drop for Writable {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn unwritable_directory_fails_every_file_and_leaves_originals() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let original = bytes(&album);
    let folder = album.files[0].parent().unwrap().to_owned();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let _restore = Writable(folder.clone());
    let (result, written) = engine
        .apply(&apply_request(&proposal, &[]), &running())
        .unwrap();
    assert!(written.is_empty());
    assert_eq!(result.files.len(), 3);
    for file in &result.files {
        assert_eq!(file.outcome, TagFileOutcome::Failed, "{file:?}");
        assert!(file.message.as_deref().is_some_and(|m| !m.is_empty()));
    }
    assert_eq!(bytes(&album), original);
    let names: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 3, "no temporary file is left: {names:?}");
    let record = journal(&album)
        .load(result.edit_id.unwrap())
        .unwrap()
        .unwrap();
    assert!(record.files.iter().all(|(_, s)| *s == FileStatus::Failed));
}

#[test]
fn a_stopped_worker_writes_nothing() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let original = bytes(&album);
    let (result, written) = engine
        .apply(&apply_request(&proposal, &[]), &AtomicBool::new(true))
        .unwrap();
    let stopped = (
        TagFileOutcome::Skipped,
        Some("the server stopped before writing this file"),
    );
    assert_eq!(outcomes(&result), [stopped; 3]);
    assert_eq!((result.edit_id, written.len()), (None, 0));
    assert_eq!(bytes(&album), original);
}

#[test]
fn a_stop_between_files_finishes_the_started_file_only() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let original = bytes(&album);
    // Asked before the journal, then before each file: stop after file 1.
    let mut asked = 0;
    let (result, written) = engine
        .apply_until(&apply_request(&proposal, &[]), || {
            asked += 1;
            asked > 2
        })
        .unwrap();
    let stopped = (
        TagFileOutcome::Skipped,
        Some("the server stopped before writing this file"),
    );
    assert_eq!(outcomes(&result), [WRITTEN, stopped, stopped]);
    assert_eq!(written, album.files[..1]);
    assert_ne!(std::fs::read(&album.files[0]).unwrap(), original[0]);
    assert_eq!(bytes(&album)[1..], original[1..]);
    let record = journal(&album)
        .load(result.edit_id.unwrap())
        .unwrap()
        .unwrap();
    let statuses: Vec<_> = record.files.iter().map(|(_, s)| *s).collect();
    assert_eq!(
        statuses,
        [FileStatus::Written, FileStatus::Failed, FileStatus::Failed]
    );
}

#[test]
fn a_failed_revert_stays_open_until_a_second_revert_finishes_it() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let (applied, _) = engine
        .apply(&apply_request(&proposal, &[]), &running())
        .unwrap();
    let edit_id = applied.edit_id.unwrap();
    let applied_bytes = bytes(&album);
    let folder = album.files[0].parent().unwrap().to_owned();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let restore = Writable(folder);
    let (failed, written) = engine.revert(edit_id, &running()).unwrap();
    assert!(written.is_empty());
    assert!(
        failed
            .files
            .iter()
            .all(|f| f.outcome == TagFileOutcome::Failed),
        "{failed:?}"
    );
    assert_eq!(bytes(&album), applied_bytes);
    let record = journal(&album).load(edit_id).unwrap().unwrap();
    assert!(!record.reverted);
    assert!(record.files.iter().all(|(_, s)| *s == FileStatus::Written));

    drop(restore);
    let (reverted, written) = engine.revert(edit_id, &running()).unwrap();
    assert_eq!(outcomes(&reverted), [WRITTEN; 3]);
    assert_eq!(written, album.files);
    for path in &album.files {
        let tags = read_tags(path).unwrap();
        assert_eq!(tags.current(TagField::Album), Some("Old Album"));
        assert_eq!(tags.current(TagField::MusicbrainzReleaseId), Some(A));
        assert_eq!(tags.current(TagField::Title), None);
    }
    let record = journal(&album).load(edit_id).unwrap().unwrap();
    assert!(record.reverted);
    assert!(record.files.iter().all(|(_, s)| *s == FileStatus::Reverted));
}

#[test]
fn an_unavailable_journal_writes_nothing() {
    let album = stored_a();
    std::fs::create_dir(album.database.with_extension("tagging.sqlite")).unwrap();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let original = bytes(&album);
    let (code, message) = refused_apply(engine.apply(&apply_request(&proposal, &[]), &running()));
    assert_eq!(code, ErrorCode::Persistence);
    assert!(
        message.starts_with("tag journal unavailable: "),
        "{message}"
    );
    assert_eq!(bytes(&album), original);
    assert_eq!(refused_apply(engine.edits()).0, ErrorCode::Persistence);
}

#[test]
fn a_track_total_without_its_number_is_not_written_to_id3v2() {
    let album = LibraryFixture::new(&[("tone.mp3", &[(TagField::MusicbrainzReleaseId, A)])]);
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let fields: Vec<_> = proposal.tracks[0].changes.iter().map(|c| c.field).collect();
    assert!(
        fields.contains(&TagField::TrackNumber) && fields.contains(&TagField::TrackTotal),
        "{fields:?}"
    );
    let original = bytes(&album);
    let (result, written) = engine
        .apply(
            &apply_request(&proposal, &[TagField::TrackNumber]),
            &running(),
        )
        .unwrap();
    assert_eq!(
        outcomes(&result),
        [(
            TagFileOutcome::Skipped,
            Some("can't write the track total without the track number")
        )]
    );
    assert_eq!((result.edit_id, written.len()), (None, 0));
    assert_eq!(bytes(&album), original);
}

#[test]
fn revert_restores_before_values_and_removes_added_fields() {
    use lofty::{
        config::WriteOptions,
        id3::v1::Id3v1Tag,
        tag::{Accessor, TagExt},
    };
    let album = LibraryFixture::new(&[
        ("tone.mp3", &[(TagField::TrackNumber, "1")]),
        ("tone.mp3", &[(TagField::TrackNumber, "2")]),
        ("tone.mp3", &[(TagField::TrackNumber, "3")]),
    ]);
    for path in &album.files {
        let mut id3v1 = Id3v1Tag::new();
        id3v1.set_title("Old Title".to_owned());
        id3v1.save_to_path(path, WriteOptions::default()).unwrap();
    }
    let old_title = TagCopy {
        tag: TagKind::Id3v1,
        field: TagField::Title,
        key: CopyKey::Field,
        value: "Old Title".into(),
        read_only: false,
    };
    let before: Vec<FileTags> = album.files.iter().map(|p| read_tags(p).unwrap()).collect();
    for tags in &before {
        assert_eq!(tags.current(TagField::Title), Some("Old Title"));
        assert!(tags.copies.contains(&old_title), "{tags:?}");
        assert!(MB_IDS.iter().all(|f| tags.current(*f).is_none()));
    }
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    assert!(proposal.tracks.iter().all(|t| t.removes_other_copies));
    let (applied, _) = engine
        .apply(&apply_request(&proposal, &[]), &running())
        .unwrap();
    assert_eq!(outcomes(&applied), [WRITTEN; 3]);
    for path in &album.files {
        let tags = read_tags(path).unwrap();
        assert_eq!(
            tags.current(TagField::Title),
            Some(tags.primary[&TagField::Title].as_str())
        );
        assert!(!tags.copies.contains(&old_title));
        assert!(MB_IDS.iter().all(|f| tags.current(*f).is_some()));
    }
    let edit_id = applied.edit_id.unwrap();
    let (reverted, written) = engine.revert(edit_id, &running()).unwrap();
    assert_eq!(reverted.edit_id, Some(edit_id));
    assert_eq!(outcomes(&reverted), [WRITTEN; 3]);
    assert_eq!(written, album.files);
    for (path, before) in album.files.iter().zip(&before) {
        let tags = read_tags(path).unwrap();
        assert!(
            MB_IDS.iter().all(|f| tags.current(*f).is_none()),
            "{tags:?}"
        );
        assert_eq!(tags.current(TagField::Title), Some("Old Title"));
        assert!(tags.copies.contains(&old_title), "{tags:?}");
        assert_eq!(tags.primary, before.primary);
    }
    let record = journal(&album).load(edit_id).unwrap().unwrap();
    assert!(record.reverted);
    assert!(record.files.iter().all(|(_, s)| *s == FileStatus::Reverted));
}

#[test]
fn double_revert_is_refused() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let (applied, _) = engine
        .apply(&apply_request(&proposal, &[]), &running())
        .unwrap();
    let edit_id = applied.edit_id.unwrap();
    engine.revert(edit_id, &running()).unwrap();
    let reverted = bytes(&album);
    assert_eq!(
        refused_apply(engine.revert(edit_id, &running())),
        (
            ErrorCode::InvalidCommand,
            "this tag edit was already reverted".into()
        )
    );
    assert_eq!(
        refused_apply(engine.revert(edit_id + 1, &running())),
        (ErrorCode::InvalidCommand, "unknown tag edit".into())
    );
    assert_eq!(bytes(&album), reverted);
}

#[test]
fn revert_skips_files_changed_since_the_edit() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    let proposal = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let (applied, _) = engine
        .apply(&apply_request(&proposal, &[]), &running())
        .unwrap();
    let edit = TagEdit {
        set: [(TagField::Title, Some("Changed".to_owned()))].into(),
        restore: Vec::new(),
    };
    edit_tags(&album.files[1], &edit).unwrap();
    let edit_id = applied.edit_id.unwrap();
    let (reverted, written) = engine.revert(edit_id, &running()).unwrap();
    assert_eq!(
        outcomes(&reverted),
        [
            WRITTEN,
            (TagFileOutcome::Skipped, Some("changed since the edit")),
            WRITTEN
        ]
    );
    assert_eq!(written, [album.files[0].clone(), album.files[2].clone()]);
    let album_of = |index: usize| {
        read_tags(&album.files[index])
            .unwrap()
            .current(TagField::Album)
            .map(str::to_owned)
    };
    assert_eq!(album_of(0).as_deref(), Some("Old Album"));
    assert_eq!(album_of(1).as_deref(), Some("Fixture Album"));
    assert_eq!(album_of(2).as_deref(), Some("Old Album"));
    let changed = read_tags(&album.files[1]).unwrap();
    assert_eq!(changed.current(TagField::Title), Some("Changed"));
    let record = journal(&album).load(edit_id).unwrap().unwrap();
    assert!(record.reverted);
    let statuses: Vec<_> = record.files.iter().map(|(_, s)| *s).collect();
    assert_eq!(
        statuses,
        [
            FileStatus::Reverted,
            FileStatus::Written,
            FileStatus::Reverted
        ]
    );
}

#[test]
fn edits_lists_latest_first_with_counts() {
    let album = stored_a();
    let server = musicbrainz("search.json");
    let mut engine = engine(&album, &server);
    assert!(engine.edits().unwrap().is_empty());
    let first = engine
        .lookup(&request(&album), &Cancellation::new(), deadline())
        .unwrap();
    let (first, _) = engine
        .apply(&apply_request(&first, &[]), &running())
        .unwrap();
    let explicit = LookupRequest {
        release_id: Some(B.into()),
        ..request(&album)
    };
    let second = engine
        .lookup(&explicit, &Cancellation::new(), deadline())
        .unwrap();
    let mut apply = apply_request(&second, &[]);
    apply.track_ids.truncate(2);
    let (second, _) = engine.apply(&apply, &running()).unwrap();
    let second_id = second.edit_id.unwrap();
    engine.revert(second_id, &running()).unwrap();
    let listed: Vec<_> = engine
        .edits()
        .unwrap()
        .into_iter()
        .map(|e| (e.edit_id, e.release_title, e.file_count, e.reverted))
        .collect();
    assert_eq!(
        listed,
        [
            (second_id, "Fixture Album".to_owned(), 2, true),
            (first.edit_id.unwrap(), "Fixture Album".to_owned(), 3, false),
        ]
    );
}

#[test]
fn apply_is_not_cancelled_when_the_requester_goes_away() {
    let album = stored_a();
    // Release B is slow, so a lookup of it keeps the worker busy.
    let server = HttpFixture::new(|target| {
        let mut reply = releases_or(target, "search.json");
        if target.contains(B) {
            reply.delay = Duration::from_millis(500);
        }
        reply
    });
    let workers = TaggingWorkers::with_providers(
        album.database.clone(),
        Some(providers(&server, None)),
        None,
    );
    let handle = workers.handle();
    let proposal = block_on(async {
        let Ok(TagOutcome::Proposal(proposal)) =
            handle.request(TagRequest::Lookup(request(&album))).await
        else {
            panic!("lookup failed");
        };
        let busy = {
            let handle = handle.clone();
            let explicit = LookupRequest {
                release_id: Some(B.into()),
                ..request(&album)
            };
            tokio::spawn(async move { handle.request(TagRequest::Lookup(explicit)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        // One poll queues the apply behind the slow lookup; the future is
        // then dropped, so the apply's reply is closed before it runs.
        let apply = handle.request(TagRequest::Apply(apply_request(&proposal, &[])));
        let gone = tokio::time::timeout(Duration::ZERO, apply).await;
        assert!(gone.is_err(), "the apply is still waiting");
        assert!(matches!(busy.await, Ok(Ok(TagOutcome::Proposal(_)))));
        proposal
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let written = || {
        proposal
            .tracks
            .iter()
            .zip(&album.files)
            .all(|(track, path)| {
                read_tags(path).is_ok_and(|tags| {
                    track
                        .changes
                        .iter()
                        .all(|c| tags.primary.get(&c.field) == Some(&c.new))
                })
            })
    };
    while !written() {
        assert!(Instant::now() < deadline, "the apply never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    workers.shutdown();
}
