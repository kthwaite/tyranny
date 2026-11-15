//! Deterministic scanner-event interleavings and persistent removal transactions.
use super::*;
use crate::{
    listing::{Browser, Grouping, ListDescription, ListSource},
    protocol::TrackSelection,
    scanner::{Candidate, ProbeResult},
};

fn fixture(count: u64, version: i64) -> Connection {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    crate::store::migrate(&mut connection, version).unwrap();
    connection.execute_batch(
        "INSERT INTO library_roots(id,path,search,completed_scan) VALUES(1,'/music','/music',1);
         INSERT INTO library_artists(id,identity,name,search) VALUES(1,'artist','Artist','artist');
         INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation)
             VALUES(1,'album',1,'Album','Artist','album','album',0);
         UPDATE library_state SET revision=7,root_count=1,scan_serial=1;",
    ).unwrap();
    for id in 1..=count {
        connection.execute(
            "INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan,size,modified_ns,metadata_version)
             VALUES(?1,1,?2,1,1,10,0,?3)",
            params![id, format!("/music/{id}.wav"), db::METADATA_VERSION],
        ).unwrap();
        let metadata = TrackMetadata {
            title: format!("Song {id:06}"),
            artist: format!("Artist {id}"),
            album: "Album".into(),
            album_artist: "Artist".into(),
            duration_ms: Some(1000),
            ..TrackMetadata::default()
        };
        connection.execute(
            "INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,
             disc_number,disc_total,track_number,file_size)
             VALUES(?1,?1,1,1,?2,?3,?3,1,1,?1,10)",
            params![id, serde_json::to_string(&metadata).unwrap(), metadata.title.to_lowercase()],
        ).unwrap();
    }
    connection.execute("UPDATE library_state SET track_count=?1", [count]).unwrap();
    grouping::initialize(&connection).unwrap();
    connection
}

fn ids(revision: u64, track_ids: Vec<u64>) -> TrackSelection {
    TrackSelection::Ids { library_revision: revision, track_ids }
}

fn selection(source: ListSource, ranges: Vec<[u64; 2]>, excluded: Vec<u64>) -> TrackSelection {
    TrackSelection::List {
        list: ListDescription {
            source,
            browser: Browser::default(),
            query: String::new(),
            sort: Vec::new(),
            group: Grouping::None,
        },
        library_revision: 7,
        ranges,
        excluded,
        first_row_id: None,
    }
}

#[test]
fn removal_schema_migration_preserves_tracks_and_enforces_exclusion() {
    let mut connection = fixture(2, 10);
    crate::store::migrate(&mut connection, 11).unwrap();
    assert_eq!(db::state(&connection, None).unwrap().tracks, 2);
    assert_eq!(db::resolve_track(&connection, 1, 7).unwrap(), "/music/1.wav");
    assert_eq!(db::remove_tracks(&mut connection, &ids(7, vec![1])).unwrap(), (1, 8));
    assert!(connection.execute("UPDATE library_assets SET indexed=1 WHERE id=1", []).is_err());
    assert!(db::resolve_track(&connection, 1, 8).is_err());
    assert_eq!(db::resolve_track(&connection, 2, 8).unwrap(), "/music/2.wav");
    assert_eq!(db::state(&connection, None).unwrap().tracks, 1);
}

#[test]
fn removal_rolls_back_the_entire_selection_on_storage_failure() {
    let mut connection = fixture(2, 11);
    connection.execute_batch(
        "CREATE TRIGGER reject_second_removal BEFORE UPDATE OF excluded ON library_assets
         WHEN NEW.id=2 BEGIN SELECT RAISE(ABORT,'injected removal failure'); END;",
    ).unwrap();
    assert!(db::remove_tracks(&mut connection, &ids(7, vec![1, 2])).is_err());
    assert_eq!(db::state(&connection, None).unwrap().revision, 7);
    assert_eq!(db::state(&connection, None).unwrap().tracks, 2);
    assert!(db::resolve_track(&connection, 1, 7).is_ok());
    assert!(db::resolve_track(&connection, 2, 7).is_ok());
}

#[test]
fn removal_resolves_large_ranges_and_deduplicates_playlist_occurrences() {
    let mut connection = fixture(1200, 11);
    let selected = selection(ListSource::Library, vec![[0, u64::MAX]], vec![2, 700]);
    assert_eq!(db::remove_tracks(&mut connection, &selected).unwrap(), (1198, 8));
    let state = db::state(&connection, None).unwrap();
    assert_eq!((state.tracks, state.duration_ms, state.size_bytes), (2, 2000, 20));
    assert!(db::resolve_track(&connection, 2, 8).is_ok());
    assert!(db::resolve_track(&connection, 700, 8).is_ok());
    assert!(db::resolve_track(&connection, 1, 8).is_err());

    let mut connection = fixture(2, 11);
    connection.execute_batch(
        "INSERT INTO playlists(id,name,search,revision) VALUES(1,'Mix','mix',3);
         INSERT INTO playlist_entries(id,playlist_id,track_id,ordinal) VALUES
             (10,1,1,0),(11,1,2,1),(12,1,1,2);",
    ).unwrap();
    let stale = selection(ListSource::Playlist { id: 1, revision: 2 }, vec![[0, u64::MAX]], Vec::new());
    let failure = classify(db::remove_tracks(&mut connection, &stale).unwrap_err());
    assert_eq!(failure.kind, LibraryErrorKind::StalePlaylistRevision);
    assert_eq!(db::state(&connection, None).unwrap().tracks, 2);
    let selected = selection(ListSource::Playlist { id: 1, revision: 3 }, vec![[0, u64::MAX]], vec![11]);
    assert_eq!(db::remove_tracks(&mut connection, &selected).unwrap(), (1, 8));
    assert_eq!(db::state(&connection, None).unwrap().tracks, 1);
    assert!(db::resolve_track(&connection, 2, 8).is_ok());
    let mut mixed = selection(
        ListSource::Playlist { id: 1, revision: 3 }, vec![[0, u64::MAX]], Vec::new(),
    );
    if let TrackSelection::List { library_revision, .. } = &mut mixed {
        *library_revision = 8;
    }
    let failure = classify(db::remove_tracks(&mut connection, &mixed).unwrap_err());
    assert_eq!(failure.kind, LibraryErrorKind::Invalid);
    assert_eq!(db::state(&connection, None).unwrap().revision, 8);
    assert!(db::resolve_track(&connection, 2, 8).is_ok());
}

#[test]
fn removal_during_discovery_pending_work_or_in_flight_probe_never_resurrects() {
    // The scanner event functions are the coordinator's persisted work boundary.
    // Drive each interleaving explicitly rather than racing a native probe by time.
    for persist_status in [false, true] {
        for phase in 0..4 {
            let mut connection = fixture(1, 11);
            let transaction = connection.transaction().unwrap();
            let (serial, mut status) = if persist_status {
                db::begin_scan(&transaction, 1).unwrap()
            } else {
                (db::begin_paths(&transaction).unwrap(), ScanStatus {
                    root_id: Some(1),
                    phase: ScanPhase::Discovering,
                    ..ScanStatus::default()
                })
            };
            transaction.commit().unwrap();
            grouping::begin(&connection, 1).unwrap();
            let candidate = Candidate {
                path: "/music/1.wav".into(), size: 20, modified_ns: 1, created_ns: None,
            };
            if phase == 0 {
                db::remove_tracks(&mut connection, &ids(7, vec![1])).unwrap();
            }
            db::discover(&mut connection, serial, &mut status, &[candidate], persist_status).unwrap();
            status.total = Some(status.discovered);
            if phase == 1 {
                db::remove_tracks(&mut connection, &ids(7, vec![1])).unwrap();
            }
            let probes = db::next_batch(&mut connection, serial, &mut status, persist_status).unwrap();
            if phase >= 2 {
                assert_eq!(probes.len(), 1);
                db::remove_tracks(&mut connection, &ids(7, vec![1])).unwrap();
                let result = if phase == 2 {
                    Ok(TrackMetadata { title: "Late metadata".into(), ..TrackMetadata::default() })
                } else {
                    Err("Late probe failure".into())
                };
                db::probed(&mut connection, serial, &mut status,
                    vec![ProbeResult { candidate: probes.into_iter().next().unwrap(), result }],
                    persist_status).unwrap();
            } else {
                assert!(probes.is_empty());
            }
            let mut grouping = grouping::Reconciliation::default();
            while !grouping.seed(&mut connection).unwrap() {}
            for tick in 0..100 {
                if grouping.step(&mut connection, 1, serial, true).unwrap() { break; }
                assert!(tick < 99, "grouping did not complete after removal");
            }
            if persist_status {
                db::finish(&mut connection, serial, &mut status, true).unwrap();
                assert_eq!(status.phase, ScanPhase::Complete);
            } else {
                db::finish_paths(&mut connection, serial, &status).unwrap();
            }
            assert_eq!((status.discovered, status.processed, status.updated, status.failures), (1, 1, 0, 0));
            let state = db::state(&connection, None).unwrap();
            assert_eq!((state.revision, state.tracks, state.duration_ms, state.size_bytes), (8, 0, 0, 0));
            assert!(db::resolve_track(&connection, 1, 8).is_err());
        }
    }
}

#[test]
fn removal_after_group_analysis_does_not_rewrite_or_restore_excluded_members() {
    let mut connection = fixture(2, 11);
    grouping::begin(&connection, 1).unwrap();
    let mut grouping = grouping::Reconciliation::default();
    while !grouping.seed(&mut connection).unwrap() {}
    // Prepare members, then analyze their artist diversity before removing one.
    assert!(!grouping.step(&mut connection, 1, 1, true).unwrap());
    assert!(!grouping.step(&mut connection, 1, 1, true).unwrap());
    let metadata: String = connection.query_row("SELECT metadata FROM library_tracks WHERE id=1", [], |row| row.get(0)).unwrap();
    db::remove_tracks(&mut connection, &ids(7, vec![1])).unwrap();
    for tick in 0..100 {
        if grouping.step(&mut connection, 1, 1, true).unwrap() { break; }
        assert!(tick < 99, "grouping did not finish");
    }
    let state = db::state(&connection, None).unwrap();
    assert_eq!(state.tracks, 1);
    assert!(db::resolve_track(&connection, 1, state.revision).is_err());
    assert!(db::resolve_track(&connection, 2, state.revision).is_ok());
    let retained: String = connection.query_row("SELECT metadata FROM library_tracks WHERE id=1", [], |row| row.get(0)).unwrap();
    assert_eq!(retained, metadata);
}
