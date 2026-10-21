//! Lookups against scanned fixture albums and loopback providers. Tests never
//! touch the network.
use super::{
    Failure, LookupRequest, TagOutcome, TagRequest, TaggingWorkers,
    acoustid::AcoustId,
    engine::{Engine, Providers},
    musicbrainz::MusicBrainz,
    proposal::{PROPOSAL_TTL, Proposal, Proposals},
    test_support::{HttpFixture, LibraryFixture, Reply, fixture, json},
};
use crate::cancel::Cancellation;
use crate::protocol::{ErrorCode, FieldChange, TagCandidate, TagField, TagProposal};
use std::{
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
    Engine::open(&album.database, providers)
        .unwrap()
        .lookup(request, &Cancellation::new())
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
    let workers =
        TaggingWorkers::with_providers(album.database.clone(), providers(&server, None), None);
    let handle = workers.handle();
    let proposal = block_on(async {
        let abandoned = tokio::time::timeout(
            Duration::from_millis(100),
            handle.request(TagRequest::Lookup(request(&album))),
        )
        .await;
        assert!(abandoned.is_err(), "the first lookup is still waiting");
        let explicit = LookupRequest {
            release_id: Some(B.into()),
            ..request(&album)
        };
        handle.request(TagRequest::Lookup(explicit)).await
    });
    let TagOutcome::Proposal(proposal) = proposal.unwrap();
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
    let workers =
        TaggingWorkers::with_providers(album.database.clone(), providers(&server, None), None);
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
