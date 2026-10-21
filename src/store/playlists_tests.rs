//! Observe durable behavior through Store, with legacy SQLite files as inputs.
use super::*;
use crate::{
    decoder::GainMode,
    library_types::{PageRequest, TrackMetadata},
    protocol::ErrorCode,
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("tyranny-playlist-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn database(&self) -> PathBuf {
        self.0.join("state.sqlite3")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn page(revision: Option<u64>) -> PageRequest {
    PageRequest {
        revision,
        offset: 0,
        limit: 100,
        query: String::new(),
    }
}
fn legacy_database(path: &Path, version: u32) -> Result<()> {
    let mut connection = Connection::open(path)?;
    // Schema 1 fixture: retained identity gaps, non-default settings and a
    // selected duplicate occurrence ensure migration cannot silently reset it.
    connection.execute_batch(
        "CREATE TABLE queue(id INTEGER PRIMARY KEY CHECK(id>0),ordinal INTEGER NOT NULL UNIQUE,path TEXT NOT NULL);
         CREATE TABLE control(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL,
         next_id INTEGER NOT NULL,current_id INTEGER REFERENCES queue(id) ON DELETE SET NULL,
         position_frames INTEGER NOT NULL,playing INTEGER NOT NULL,settings TEXT NOT NULL);
         INSERT INTO queue VALUES(3,0,'/music/retained.wav'),(9,1,'/music/retained.wav');"
    )?;
    // Settings as an older schema wrote them, without later fields.
    let settings = serde_json::json!({"gain_mode": GainMode::Album, "resume_on_start": true});
    connection.execute(
        "INSERT INTO control VALUES(1,7,12,9,48000,1,?1)",
        [settings.to_string()],
    )?;
    if version == 2 {
        let transaction = connection.transaction()?;
        library::migrate(&transaction)?;
        transaction.execute_batch(
            "ALTER TABLE queue ADD COLUMN track_id INTEGER REFERENCES library_tracks(id);",
        )?;
        seed_library(&transaction)?;
        transaction.execute("UPDATE queue SET track_id=1", [])?;
        transaction.commit()?;
    }
    connection.pragma_update(None, "application_id", APPLICATION_ID)?;
    connection.pragma_update(None, "user_version", version)?;
    Ok(())
}
fn seed_library(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "INSERT INTO library_roots(id,path,search,completed_scan) VALUES(1,'/music','/music',1);
         INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan) VALUES(1,1,'/music/retained.wav',1,1);
         INSERT INTO library_artists(id,identity,name,search) VALUES(1,'artist','Artist','artist');
         INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation)
         VALUES(1,'album',1,'Album','Artist','album','album',0);
         UPDATE library_state SET revision=9,root_count=1,track_count=1;"
    )?;
    let metadata = TrackMetadata {
        title: "Retained title".into(),
        artist: "Artist".into(),
        ..Default::default()
    };
    connection.execute(
        "INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number)
         VALUES(1,1,1,1,?1,'retained title','retained title',1,1,1)", [serde_json::to_string(&metadata)?],
    )?;
    Ok(())
}

#[test]
fn schema_one_and_two_migrations_preserve_queue_checkpoint_settings_and_library() -> Result<()> {
    for version in [1, 2] {
        let scratch = Scratch::new();
        let path = scratch.database();
        legacy_database(&path, version)?;
        let mut store = Store::open(&path)?;
        let loaded = store.load()?;
        assert_eq!(
            loaded
                .entries
                .iter()
                .map(|entry| (entry.id, entry.path.clone()))
                .collect::<Vec<_>>(),
            vec![
                (3, PathBuf::from("/music/retained.wav")),
                (9, PathBuf::from("/music/retained.wav"))
            ]
        );
        assert_eq!(
            (loaded.revision, loaded.next_id, loaded.playlists_revision),
            (7, 12, 1)
        );
        assert_eq!(loaded.checkpoint.current_id, Some(9));
        assert_eq!(loaded.checkpoint.position_frames, 48000);
        assert!(loaded.checkpoint.playing);
        assert_eq!(
            loaded.settings,
            Settings {
                gain_mode: GainMode::Album,
                resume_on_start: true,
                ..Settings::default()
            }
        );
        assert!(
            store
                .saved_playlists(page(Some(1)), None)?
                .entries
                .is_empty()
        );
        let created = store.edit_playlist(
            None,
            1,
            PlaylistEdit::Create {
                name: " Kept ".into(),
            },
        )?;
        let playlist = created.playlist.unwrap();
        assert_eq!(playlist.name, "Kept");
        if version == 2 {
            assert_eq!(store.track_source(1, 9)?, "/music/retained.wav");
            assert_eq!(store.queue_page(0, 10)?[0].track_id, Some(1));
            store.edit_playlist(
                Some(playlist.id),
                1,
                PlaylistEdit::Add {
                    library_revision: 9,
                    track_ids: vec![1, 1],
                    before: None,
                },
            )?;
            let (revision, entries) =
                store.playlist_entries(playlist.id, Some(2), Some(9), 0, 10, "")?;
            assert_eq!(revision, 9);
            assert_eq!(entries.entries[0].metadata.title, "Retained title");
            assert_ne!(entries.entries[0].id, entries.entries[1].id);
        }
        drop(store);
        let mut reopened = Store::open(&path)?;
        assert_eq!(reopened.load()?.checkpoint.current_id, Some(9));
        let catalog = reopened.saved_playlists(page(None), None)?;
        assert_eq!(catalog.entries[0].id, playlist.id);
        assert_eq!(
            catalog.entries[0].entry_count,
            Some(if version == 2 { 2 } else { 0 })
        );
    }
    Ok(())
}

#[test]
fn fresh_catalog_survives_restart_and_deleted_identities_never_recycle() -> Result<()> {
    let scratch = Scratch::new();
    let path = scratch.database();
    let mut store = Store::open(&path)?;
    let first = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::Create {
                name: "Été".into()
            },
        )?
        .playlist
        .unwrap();
    assert_eq!(first.revision, 1);
    let duplicate = store
        .edit_playlist(
            None,
            2,
            PlaylistEdit::Create {
                name: "ÉTÉ".into()
            },
        )
        .err()
        .unwrap();
    assert_eq!(
        duplicate.downcast_ref::<PlaylistFailure>().unwrap().code,
        ErrorCode::InvalidCommand
    );
    assert_eq!(store.playlists_revision()?, 2);
    store.edit_playlist(
        Some(first.id),
        1,
        PlaylistEdit::Rename {
            name: "ÉTÉ".into()
        },
    )?;
    let mut other = Store::open(&path)?;
    let stale = other
        .edit_playlist(Some(first.id), 1, PlaylistEdit::Delete)
        .err()
        .unwrap();
    assert_eq!(
        stale.downcast_ref::<PlaylistFailure>().unwrap().code,
        ErrorCode::StalePlaylistRevision
    );
    assert!(stale.to_string().contains("revision 1 is stale; current 2"));
    store.edit_playlist(Some(first.id), 2, PlaylistEdit::Delete)?;
    drop(store);
    let mut reopened = Store::open(&path)?;
    assert_eq!(reopened.load()?.playlists_revision, 4);
    let second = reopened
        .edit_playlist(
            None,
            4,
            PlaylistEdit::Create {
                name: "Été".into()
            },
        )?
        .playlist
        .unwrap();
    assert!(second.id > first.id);
    assert_eq!(reopened.load()?.revision, 1);
    Ok(())
}

#[test]
fn atomic_playlist_load_rechecks_all_revisions_and_rolls_back_late_failure() -> Result<()> {
    let scratch = Scratch::new();
    let path = scratch.database();
    legacy_database(&path, 2)?;
    let mut store = Store::open(&path)?;
    let playlist = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::Create {
                name: "Twice".into(),
            },
        )?
        .playlist
        .unwrap();
    store.edit_playlist(
        Some(playlist.id),
        1,
        PlaylistEdit::Add {
            library_revision: 9,
            track_ids: vec![1, 1],
            before: None,
        },
    )?;
    assert_eq!(store.playlist_sources(playlist.id, 2, 9)?.len(), 2);
    // First insert is valid, second source differs from the prepared playlist.
    // Neither queue rows nor the combined playback checkpoint may leak out.
    let broken = [
        (12, 1, Path::new("/music/retained.wav")),
        (13, 1, Path::new("/music/wrong.wav")),
    ];
    assert!(
        store
            .enqueue_playlist(
                playlist.id,
                2,
                9,
                7,
                broken.into_iter(),
                Some(Checkpoint {
                    current_id: Some(12),
                    position_frames: 0,
                    playing: true
                })
            )
            .is_err()
    );
    let unchanged = store.load()?;
    assert_eq!(
        (
            unchanged.revision,
            unchanged.next_id,
            unchanged.entries.len()
        ),
        (7, 12, 2)
    );
    assert_eq!(
        (
            unchanged.checkpoint.current_id,
            unchanged.checkpoint.position_frames,
            unchanged.checkpoint.playing
        ),
        (Some(9), 48000, true)
    );
    let entries = [
        (12, 1, Path::new("/music/retained.wav")),
        (13, 1, Path::new("/music/retained.wav")),
    ];
    for (playlist_revision, library_revision, queue_revision, code) in [
        (1, 9, 7, ErrorCode::StalePlaylistRevision),
        (2, 9, 6, ErrorCode::StaleRevision),
    ] {
        let error = store
            .enqueue_playlist(
                playlist.id,
                playlist_revision,
                library_revision,
                queue_revision,
                entries.into_iter(),
                None,
            )
            .err()
            .unwrap();
        assert_eq!(error.downcast_ref::<PlaylistFailure>().unwrap().code, code);
    }
    let stale_library = store
        .enqueue_playlist(playlist.id, 2, 8, 7, entries.into_iter(), None)
        .err()
        .unwrap();
    assert!(stale_library.is::<crate::library_types::StaleLibraryRevision>());
    store.enqueue_playlist(
        playlist.id,
        2,
        9,
        7,
        entries.into_iter(),
        Some(Checkpoint {
            current_id: Some(12),
            position_frames: 0,
            playing: true,
        }),
    )?;
    drop(store);
    let reopened = Store::open(&path)?;
    let loaded = reopened.load()?;
    assert_eq!(
        (loaded.revision, loaded.next_id, loaded.entries.len()),
        (8, 14, 4)
    );
    assert_eq!(loaded.checkpoint.current_id, Some(12));
    assert_eq!(loaded.checkpoint.position_frames, 0);
    assert!(loaded.checkpoint.playing);
    assert_eq!(
        reopened
            .queue_page(2, 10)?
            .iter()
            .map(|entry| entry.track_id)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(1)]
    );
    Ok(())
}

#[test]
fn unknown_newer_schema_is_rejected_without_resetting_existing_state() -> Result<()> {
    let scratch = Scratch::new();
    let path = scratch.database();
    legacy_database(&path, 1)?;
    let connection = Connection::open(&path)?;
    connection.pragma_update(None, "user_version", 9)?;
    assert!(
        Store::open(&path)
            .err()
            .unwrap()
            .to_string()
            .contains("unsupported database version 9")
    );
    connection.pragma_update(None, "user_version", 1)?;
    drop(connection);
    assert_eq!(Store::open(&path)?.load()?.checkpoint.current_id, Some(9));
    Ok(())
}

#[test]
fn moves_preserve_occurrence_order_across_gaps_and_integer_boundary() -> Result<()> {
    let scratch = Scratch::new();
    let path = scratch.database();
    legacy_database(&path, 2)?;
    let mut store = Store::open(&path)?;
    let mut playlist = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::Create {
                name: "Order".into(),
            },
        )?
        .playlist
        .unwrap();
    playlist = store
        .edit_playlist(
            Some(playlist.id),
            playlist.revision,
            PlaylistEdit::Add {
                library_revision: 9,
                track_ids: vec![1; 6],
                before: None,
            },
        )?
        .playlist
        .unwrap();
    let (_, page) = store.playlist_entries(playlist.id, None, Some(9), 0, 100, "")?;
    let removed = [page.entries[1].id, page.entries[3].id];
    playlist = store
        .edit_playlist(
            Some(playlist.id),
            playlist.revision,
            PlaylistEdit::Remove {
                entry_ids: removed.to_vec(),
            },
        )?
        .playlist
        .unwrap();
    let mut expected: Vec<_> = page
        .entries
        .into_iter()
        .map(|entry| entry.id)
        .filter(|id| !removed.contains(id))
        .collect();
    // Valid sparse order: exercise SQLite's signed-integer boundary without
    // requiring an impossibly long playlist or allowing REAL-valued ordinals.
    store.connection.execute(
        "UPDATE playlist_entries SET ordinal=?1 WHERE id=?2",
        params![i64::MAX, expected.last().unwrap()],
    )?;
    let identities = expected.clone();
    for &entry_id in &identities {
        for before in identities
            .iter()
            .copied()
            .map(Some)
            .chain(std::iter::once(None))
        {
            if before != Some(entry_id) {
                expected.retain(|id| *id != entry_id);
                let index = before.map_or(expected.len(), |id| {
                    expected.iter().position(|entry| *entry == id).unwrap()
                });
                expected.insert(index, entry_id);
            }
            playlist = store
                .edit_playlist(
                    Some(playlist.id),
                    playlist.revision,
                    PlaylistEdit::Move {
                        entry_ids: vec![entry_id],
                        before,
                    },
                )?
                .playlist
                .unwrap();
            let (_, actual) = store.playlist_entries(
                playlist.id,
                Some(playlist.revision),
                Some(9),
                0,
                100,
                "",
            )?;
            assert_eq!(
                actual
                    .entries
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(actual.total as usize, expected.len());
        }
    }
    Ok(())
}

#[test]
fn batch_reorders_and_inserts_are_atomic_bounded_and_revision_bound() -> Result<()> {
    let scratch = Scratch::new();
    let path = scratch.database();
    legacy_database(&path, 2)?;
    let mut store = Store::open(&path)?;
    let mut playlist = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::Create {
                name: "Batch".into(),
            },
        )?
        .playlist
        .unwrap();
    for count in [500, 5] {
        playlist = store
            .edit_playlist(
                Some(playlist.id),
                playlist.revision,
                PlaylistEdit::Add {
                    library_revision: 9,
                    track_ids: vec![1; count],
                    before: None,
                },
            )?
            .playlist
            .unwrap();
    }
    let playlist_id = playlist.id;
    let all_ids = |store: &mut Store, revision| -> Result<Vec<u64>> {
        let mut ids = Vec::new();
        for offset in [0, 500] {
            ids.extend(
                store
                    .playlist_entries(playlist_id, Some(revision), Some(9), offset, 500, "")?
                    .1
                    .entries
                    .into_iter()
                    .map(|entry| entry.id),
            );
        }
        Ok(ids)
    };
    let original = all_ids(&mut store, playlist.revision)?;
    // Reverse request order and a destination outside the first bounded page.
    playlist = store
        .edit_playlist(
            Some(playlist.id),
            playlist.revision,
            PlaylistEdit::Move {
                entry_ids: vec![original[3], original[1]],
                before: Some(original[503]),
            },
        )?
        .playlist
        .unwrap();
    let mut expected = original.clone();
    expected.retain(|id| *id != original[1] && *id != original[3]);
    let destination = expected.iter().position(|id| *id == original[503]).unwrap();
    expected.splice(destination..destination, [original[1], original[3]]);
    assert_eq!(all_ids(&mut store, playlist.revision)?, expected);
    let revision = playlist.revision;
    let catalog = store.playlists_revision()?;
    for before in [Some(original[1]), Some(original[503])] {
        let unchanged = store.edit_playlist(
            Some(playlist.id),
            revision,
            PlaylistEdit::Move {
                entry_ids: vec![original[3], original[1]],
                before,
            },
        )?;
        assert_eq!(unchanged.playlist.unwrap().revision, revision);
        assert_eq!(unchanged.revision, catalog);
    }
    for edit in [
        PlaylistEdit::Move {
            entry_ids: vec![original[1], u64::MAX],
            before: Some(original[1]),
        },
        PlaylistEdit::Move {
            entry_ids: vec![original[1], original[1]],
            before: None,
        },
        PlaylistEdit::Move {
            entry_ids: vec![original[1]],
            before: Some(u64::MAX),
        },
        PlaylistEdit::Move {
            entry_ids: vec![],
            before: None,
        },
        PlaylistEdit::Move {
            entry_ids: vec![original[1]; 501],
            before: None,
        },
        PlaylistEdit::Add {
            library_revision: 8,
            track_ids: vec![1],
            before: None,
        },
        PlaylistEdit::Add {
            library_revision: 9,
            track_ids: vec![1, 999],
            before: Some(original[503]),
        },
        PlaylistEdit::Add {
            library_revision: 9,
            track_ids: vec![1],
            before: Some(u64::MAX),
        },
    ] {
        assert!(
            store
                .edit_playlist(Some(playlist.id), revision, edit)
                .is_err()
        );
        assert_eq!(all_ids(&mut store, revision)?, expected);
        assert_eq!(store.playlists_revision()?, catalog);
    }
    assert!(
        store
            .edit_playlist(
                Some(playlist.id),
                revision - 1,
                PlaylistEdit::Move {
                    entry_ids: vec![original[0]],
                    before: None
                }
            )
            .is_err()
    );
    store.connection.execute_batch(
        "CREATE TRIGGER fail_playlist_insert BEFORE INSERT ON playlist_entries BEGIN SELECT RAISE(ABORT,'injected'); END;"
    )?;
    assert!(
        store
            .edit_playlist(
                Some(playlist.id),
                revision,
                PlaylistEdit::Add {
                    library_revision: 9,
                    track_ids: vec![1, 1],
                    before: Some(original[503])
                }
            )
            .is_err()
    );
    assert_eq!(all_ids(&mut store, revision)?, expected);
    store
        .connection
        .execute_batch("DROP TRIGGER fail_playlist_insert;")?;
    playlist = store
        .edit_playlist(
            Some(playlist.id),
            revision,
            PlaylistEdit::Add {
                library_revision: 9,
                track_ids: vec![1, 1],
                before: Some(original[503]),
            },
        )?
        .playlist
        .unwrap();
    let inserted = all_ids(&mut store, playlist.revision)?;
    assert_eq!(
        &inserted[destination..destination + 2],
        &[original[1], original[3]]
    );
    assert!(inserted[destination + 2] > *original.last().unwrap());
    assert_ne!(inserted[destination + 2], inserted[destination + 3]);
    assert_eq!(inserted[destination + 4], original[503]);

    let source = Path::new("/music/retained.wav");
    store.insert_tracks(
        7,
        9,
        [(12, 1, source), (13, 1, source), (14, 1, source)].into_iter(),
        Some(9),
    )?;
    let ids = |store: &Store| -> Result<Vec<u64>> {
        Ok(store
            .queue_page(0, 500)?
            .into_iter()
            .map(|entry| entry.id)
            .collect())
    };
    assert_eq!(ids(&store)?, [3, 12, 13, 14, 9]);
    store.move_queue(8, &[14, 12], Some(3))?;
    assert_eq!(ids(&store)?, [12, 14, 3, 13, 9]);
    for anchor in [Some(12), Some(3)] {
        store.move_queue(9, &[14, 12], anchor)?;
        assert_eq!(store.load()?.revision, 9);
    }
    assert!(store.move_queue(8, &[12], None).is_err());
    assert!(store.move_queue(9, &[12, 999], Some(12)).is_err());
    assert!(store.move_queue(9, &[12], Some(999)).is_err());
    assert!(
        store
            .insert_tracks(9, 8, [(15, 1, source)].into_iter(), Some(3))
            .is_err()
    );
    assert!(
        store
            .insert_tracks(8, 9, [(15, 1, source)].into_iter(), Some(3))
            .is_err()
    );
    assert!(
        store
            .insert_tracks(
                9,
                9,
                [(15, 1, source), (16, 999, source)].into_iter(),
                Some(3)
            )
            .is_err()
    );
    assert!(
        store
            .insert_tracks(9, 9, [(15, 1, source)].into_iter(), Some(999))
            .is_err()
    );
    store.connection.execute_batch(
        "CREATE TRIGGER fail_queue_move BEFORE UPDATE OF ordinal ON queue BEGIN SELECT RAISE(ABORT,'injected'); END;"
    )?;
    assert!(store.move_queue(9, &[12], None).is_err());
    assert!(
        store
            .insert_tracks(9, 9, [(15, 1, source)].into_iter(), Some(3))
            .is_err()
    );
    store
        .connection
        .execute_batch("DROP TRIGGER fail_queue_move; UPDATE library_assets SET indexed=0;")?;
    assert!(
        store
            .insert_tracks(9, 9, [(15, 1, source)].into_iter(), None)
            .is_err()
    );
    let loaded = store.load()?;
    assert_eq!((loaded.revision, loaded.next_id), (9, 15));
    assert_eq!(
        (
            loaded.checkpoint.current_id,
            loaded.checkpoint.position_frames,
            loaded.checkpoint.playing
        ),
        (Some(9), 48_000, true)
    );
    assert_eq!(ids(&store)?, [12, 14, 3, 13, 9]);
    drop(store);
    let mut restored = Store::open(&path)?;
    assert_eq!(ids(&restored)?, [12, 14, 3, 13, 9]);
    assert_eq!(restored.load()?.checkpoint.position_frames, 48_000);
    assert_eq!(
        restored
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u64>(0))?,
        8
    );
    restored
        .connection
        .execute_batch("UPDATE library_assets SET indexed=1;")?;
    let batch: Vec<_> = (15..515).map(|id| (id, 1, source)).collect();
    restored.insert_tracks(9, 9, batch.into_iter(), None)?;
    restored.insert_tracks(
        10,
        9,
        [(515, 1, source), (516, 1, source)].into_iter(),
        None,
    )?;
    restored.move_queue(11, &[516, 12], Some(515))?;
    let tail = restored.queue_page(500, 500)?;
    assert_eq!(
        tail.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [511, 512, 513, 514, 12, 516, 515]
    );
    restored.insert_tracks(
        12,
        9,
        [(517, 1, source), (518, 1, source)].into_iter(),
        Some(515),
    )?;
    assert_eq!(
        restored
            .queue_page(504, 500)?
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [12, 516, 517, 518, 515]
    );
    assert_eq!(restored.load()?.checkpoint.current_id, Some(9));
    Ok(())
}

#[test]
fn filtered_occurrences_page_duplicates_unavailable_rows_and_preserve_hidden_order() -> Result<()> {
    let scratch = Scratch::new();
    let path = scratch.database();
    legacy_database(&path, 2)?;
    let mut store = Store::open(&path)?;
    store.connection.execute_batch(
        "INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan)
         VALUES(2,1,'/music/other.wav',1,1);
         INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number)
         SELECT 2,2,artist_id,album_id,metadata,'other','other',1,1,2 FROM library_tracks WHERE id=1;"
    )?;
    let mut playlist = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::Create {
                name: "Filter fixture".into(),
            },
        )?
        .playlist
        .unwrap();
    playlist = store
        .edit_playlist(
            Some(playlist.id),
            playlist.revision,
            PlaylistEdit::Add {
                library_revision: 9,
                track_ids: vec![1, 2, 1, 2, 1, 2],
                before: None,
            },
        )?
        .playlist
        .unwrap();
    let (_, original) =
        store.playlist_entries(playlist.id, Some(playlist.revision), Some(9), 0, 100, "")?;
    let ids: Vec<_> = original.entries.iter().map(|entry| entry.id).collect();
    store
        .connection
        .execute("UPDATE library_assets SET indexed=0 WHERE id=1", [])?;
    let filter = "artist:[Artist,Missing] & !title:other";
    for (offset, expected) in [
        (0, vec![ids[0]]),
        (1, vec![ids[2]]),
        (2, vec![ids[4]]),
        (3, vec![]),
    ] {
        let (library, page) = store.playlist_entries(
            playlist.id,
            Some(playlist.revision),
            Some(9),
            offset,
            1,
            filter,
        )?;
        assert_eq!(
            (library, page.revision, page.total, page.offset),
            (9, playlist.revision, 3, offset)
        );
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            page.entries
                .iter()
                .all(|entry| !entry.available && entry.track_id == 1)
        );
    }
    assert_eq!(
        store.saved_playlists(page(None), None)?.entries[0].entry_count,
        Some(6)
    );
    for query in [
        "title:",
        "artist:[]",
        "artist:[Artist,]",
        "unknown:x",
        "(title:other",
    ] {
        let error = store
            .playlist_entries(playlist.id, None, None, 0, 2, query)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<PlaylistFailure>().unwrap().code,
            ErrorCode::InvalidFilter
        );
        assert!(error.to_string().contains("UTF-8 byte "));
    }
    for query in [
        "x".repeat(1025),
        format!("{}x", "!".repeat(33)),
        vec!["x"; 129].join(" "),
    ] {
        let error = store
            .playlist_entries(playlist.id, None, None, 0, 2, &query)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<PlaylistFailure>().unwrap().code,
            ErrorCode::InvalidFilter
        );
    }
    let stale = store
        .playlist_entries(playlist.id, Some(1), Some(9), 1, 1, filter)
        .unwrap_err();
    assert_eq!(
        stale.downcast_ref::<PlaylistFailure>().unwrap().code,
        ErrorCode::StalePlaylistRevision
    );
    let stale = store
        .playlist_entries(playlist.id, Some(playlist.revision), Some(8), 1, 1, filter)
        .unwrap_err();
    assert_eq!(
        stale
            .downcast_ref::<crate::library_types::StaleLibraryRevision>()
            .unwrap()
            .current,
        9
    );
    // A read never changes order/revision, even when its predicate is invalid.
    let (_, unchanged) =
        store.playlist_entries(playlist.id, Some(playlist.revision), Some(9), 0, 100, "")?;
    assert_eq!(
        unchanged
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        ids
    );
    // Visible occurrence IDs anchor in full order; hidden duplicates stay put.
    playlist = store
        .edit_playlist(
            Some(playlist.id),
            playlist.revision,
            PlaylistEdit::Move {
                entry_ids: vec![ids[0]],
                before: Some(ids[4]),
            },
        )?
        .playlist
        .unwrap();
    let (_, moved) =
        store.playlist_entries(playlist.id, Some(playlist.revision), Some(9), 0, 100, "")?;
    assert_eq!(
        moved
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [ids[1], ids[2], ids[3], ids[0], ids[4], ids[5]]
    );
    playlist = store
        .edit_playlist(
            Some(playlist.id),
            playlist.revision,
            PlaylistEdit::Remove {
                entry_ids: vec![ids[2]],
            },
        )?
        .playlist
        .unwrap();
    store
        .connection
        .execute("UPDATE library_assets SET indexed=1 WHERE id=1", [])?;
    let sources = store.playlist_sources(playlist.id, playlist.revision, 9)?;
    assert_eq!(
        sources.iter().map(|(track, _)| *track).collect::<Vec<_>>(),
        [2, 2, 1, 1, 2]
    );
    let (_, filtered) = store.playlist_entries(
        playlist.id,
        Some(playlist.revision),
        Some(9),
        0,
        100,
        filter,
    )?;
    assert_eq!(
        filtered
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [ids[0], ids[4]]
    );
    assert_eq!(filtered.total, 2);
    // Catalog search remains literal despite resembling structured syntax.
    let mut catalog = page(None);
    catalog.query = "artist:[]".into();
    assert!(store.saved_playlists(catalog, None)?.entries.is_empty());
    Ok(())
}

#[test]
fn smart_rules_are_live_read_only_and_catalog_kind_filter_is_bounded() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.database())?;
    seed_library(&store.connection)?;
    let regular = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::Create {
                name: "Z regular".into(),
            },
        )?
        .playlist
        .unwrap();
    let smart = store
        .edit_playlist(
            None,
            2,
            PlaylistEdit::CreateSmart {
                name: "A smart".into(),
                query: "title:retained & !title:missing".into(),
            },
        )?
        .playlist
        .unwrap();
    assert_eq!(smart.kind, crate::protocol::PlaylistKind::Smart);
    assert_eq!(smart.entry_count, None);
    let catalog =
        store.saved_playlists(page(None), Some(crate::protocol::PlaylistKind::Regular))?;
    assert_eq!(catalog.total, 1);
    assert_eq!(catalog.entries, vec![regular.clone()]);
    let (_, results) = store.playlist_entries(smart.id, Some(1), Some(9), 0, 1, "")?;
    assert_eq!(results.total, 1);
    assert_eq!((results.entries[0].id, results.entries[0].track_id), (1, 1));
    assert_eq!(
        store
            .playlist_entries(smart.id, Some(1), Some(9), 0, 1, "title:missing")?
            .1
            .total,
        0
    );
    for edit in [
        PlaylistEdit::Add {
            library_revision: 9,
            track_ids: vec![],
            before: None,
        },
        PlaylistEdit::Add {
            library_revision: 9,
            track_ids: vec![1],
            before: None,
        },
        PlaylistEdit::Remove { entry_ids: vec![] },
        PlaylistEdit::Remove { entry_ids: vec![1] },
        PlaylistEdit::Move {
            entry_ids: vec![1],
            before: Some(1),
        },
    ] {
        let error = store.edit_playlist(Some(smart.id), 1, edit).err().unwrap();
        assert_eq!(
            error.downcast_ref::<PlaylistFailure>().unwrap().code,
            ErrorCode::InvalidCommand
        );
    }
    let invalid = store
        .edit_playlist(
            Some(smart.id),
            1,
            PlaylistEdit::UpdateSmart {
                query: "artist:".into(),
            },
        )
        .err()
        .unwrap();
    assert_eq!(
        invalid.downcast_ref::<PlaylistFailure>().unwrap().code,
        ErrorCode::InvalidFilter
    );
    assert!(
        store
            .edit_playlist(
                Some(regular.id),
                1,
                PlaylistEdit::UpdateSmart { query: "".into() }
            )
            .is_err()
    );
    assert_eq!(store.playlists_revision()?, 3);
    let changed = store
        .edit_playlist(
            Some(smart.id),
            1,
            PlaylistEdit::UpdateSmart {
                query: "title:missing".into(),
            },
        )?
        .playlist
        .unwrap();
    assert_eq!(changed.revision, 2);
    assert!(
        store
            .playlist_entries(smart.id, Some(1), Some(9), 0, 1, "")
            .is_err()
    );
    assert_eq!(
        store
            .playlist_entries(smart.id, Some(2), Some(9), 0, 1, "")?
            .1
            .total,
        0
    );
    drop(store);
    let mut store = Store::open(&scratch.database())?;
    assert_eq!(
        store
            .saved_playlists(page(None), Some(crate::protocol::PlaylistKind::Smart))?
            .entries,
        vec![changed]
    );
    Ok(())
}

#[test]
fn schema_three_migration_preserves_regular_identities_order_revisions_and_queue() -> Result<()> {
    let scratch = Scratch::new();
    let path = scratch.database();
    legacy_database(&path, 2)?;
    {
        let mut connection = Connection::open(&path)?;
        let transaction = connection.transaction()?;
        playlists::migrate(&transaction)?;
        transaction.execute_batch(
            "INSERT INTO playlists(id,name,search,revision) VALUES(8,'Kept','kept',6);
             INSERT INTO playlist_entries(id,playlist_id,track_id,ordinal) VALUES(11,8,1,4),(19,8,1,2);
             UPDATE playlists_state SET revision=13;
             PRAGMA user_version=3;"
        )?;
        transaction.commit()?;
    }
    let mut store = Store::open(&path)?;
    let loaded = store.load()?;
    assert_eq!(
        (loaded.revision, loaded.next_id, loaded.playlists_revision),
        (7, 12, 13)
    );
    assert_eq!(
        loaded
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [3, 9]
    );
    assert_eq!(
        (
            loaded.checkpoint.current_id,
            loaded.checkpoint.position_frames,
            loaded.checkpoint.playing
        ),
        (Some(9), 48000, true)
    );
    assert_eq!(loaded.settings.gain_mode, GainMode::Album);
    let catalog = store.saved_playlists(page(Some(13)), None)?;
    let regular = &catalog.entries[0];
    assert_eq!(
        (regular.id, regular.revision, regular.entry_count),
        (8, 6, Some(2))
    );
    assert_eq!(regular.kind, crate::protocol::PlaylistKind::Regular);
    assert_eq!(regular.query, None);
    let (_, entries) = store.playlist_entries(8, Some(6), Some(9), 0, 10, "")?;
    assert_eq!(
        entries
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [19, 11]
    );
    store.edit_playlist(
        None,
        13,
        PlaylistEdit::CreateSmart {
            name: "Live".into(),
            query: "".into(),
        },
    )?;
    drop(store);
    let mut store = Store::open(&path)?;
    assert_eq!(
        store
            .playlist_entries(8, Some(6), Some(9), 0, 10, "")?
            .1
            .entries,
        entries.entries
    );
    assert_eq!(
        store
            .queue_page(0, 10)?
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [3, 9]
    );
    Ok(())
}

fn seed_many_tracks(connection: &Connection) -> Result<()> {
    seed_library(connection)?;
    for id in 2..=505 {
        connection.execute(
            "INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan) VALUES(?1,1,?2,1,1)",
            params![id, format!("/music/{id:04}.wav")],
        )?;
        connection.execute(
            "INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number)
             SELECT ?1,?1,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,?1 FROM library_tracks WHERE id=1",
            [id],
        )?;
    }
    Ok(())
}

#[test]
fn smart_full_snapshots_and_enqueue_exceed_pages_and_remain_isolated() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.database())?;
    seed_many_tracks(&store.connection)?;
    let smart = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::CreateSmart {
                name: "Live".into(),
                query: "title:retained".into(),
            },
        )?
        .playlist
        .unwrap();
    let (_, first) = store.playlist_entries(smart.id, Some(1), Some(9), 0, 500, "")?;
    assert_eq!((first.total, first.entries.len()), (505, 500));
    let (_, last) = store.playlist_entries(smart.id, Some(1), Some(9), 500, 100, "")?;
    assert_eq!(
        last.entries
            .iter()
            .map(|entry| entry.track_id)
            .collect::<Vec<_>>(),
        [501, 502, 503, 504, 505]
    );
    assert!(
        store
            .playlist_entries(smart.id, Some(1), None, 500, 100, "")
            .is_err()
    );
    assert!(
        store
            .playlist_entries(smart.id, Some(1), Some(9), 0, 501, "")
            .is_err()
    );
    assert_eq!(
        store
            .playlist_entries(smart.id, Some(1), Some(9), 0, 1, "!title:retained")?
            .1
            .total,
        0
    );
    let saved = store
        .snapshot_smart_playlist(smart.id, 1, 9, 2, "Frozen")?
        .playlist
        .unwrap();
    assert_eq!(saved.entry_count, Some(505));
    assert_eq!(saved.kind, crate::protocol::PlaylistKind::Regular);
    assert_eq!(saved.query, None);
    let second = store
        .snapshot_smart_playlist(smart.id, 1, 9, 3, "Frozen twice")?
        .playlist
        .unwrap();
    let frozen = store
        .playlist_entries(saved.id, Some(1), Some(9), 500, 100, "")?
        .1;
    let twice = store
        .playlist_entries(second.id, Some(1), Some(9), 500, 100, "")?
        .1;
    assert_ne!(frozen.entries[0].id, twice.entries[0].id);
    let sources = store.playlist_sources(smart.id, 1, 9)?;
    assert_eq!(sources.len(), 505);
    store.enqueue_playlist(
        smart.id,
        1,
        9,
        1,
        sources
            .iter()
            .enumerate()
            .map(|(index, (track, path))| (index as u64 + 1, *track, path.as_path())),
        None,
    )?;
    assert_eq!(store.load()?.entries.len(), 505);
    assert_eq!(
        store
            .queue_page(500, 100)?
            .iter()
            .map(|entry| entry.track_id)
            .collect::<Vec<_>>(),
        [Some(501), Some(502), Some(503), Some(504), Some(505)]
    );
    let queue = store.load()?.entries;
    store.edit_playlist(
        Some(smart.id),
        1,
        PlaylistEdit::UpdateSmart {
            query: "title:missing".into(),
        },
    )?;
    assert_eq!(
        store
            .playlist_entries(smart.id, Some(2), Some(9), 0, 1, "")?
            .1
            .total,
        0
    );
    let empty = store
        .snapshot_smart_playlist(smart.id, 2, 9, 5, "Empty")?
        .playlist
        .unwrap();
    assert_eq!(empty.entry_count, Some(0));
    // An indexing change affects live membership, not durable occurrences.
    store.connection.execute_batch(
        "UPDATE library_assets SET indexed=0 WHERE id=505; UPDATE library_state SET revision=10;",
    )?;
    store.edit_playlist(
        Some(smart.id),
        2,
        PlaylistEdit::UpdateSmart { query: "".into() },
    )?;
    assert_eq!(
        store
            .playlist_entries(smart.id, Some(3), Some(10), 0, 1, "")?
            .1
            .total,
        504
    );
    assert!(
        store
            .playlist_entries(smart.id, Some(3), Some(9), 500, 10, "")
            .is_err()
    );
    let (_, frozen_after) = store.playlist_entries(saved.id, Some(1), Some(10), 500, 100, "")?;
    assert_eq!(frozen_after.total, 505);
    assert_eq!(frozen_after.entries[4].id, frozen.entries[4].id);
    assert!(!frozen_after.entries[4].available);
    assert_eq!(store.load()?.entries, queue);
    assert_eq!(store.load()?.revision, 2);
    // Snapshot occurrences are editable, without affecting their live source.
    store.edit_playlist(
        Some(saved.id),
        1,
        PlaylistEdit::Remove {
            entry_ids: vec![frozen.entries[4].id],
        },
    )?;
    assert_eq!(
        store
            .playlist_entries(saved.id, Some(2), Some(10), 0, 1, "")?
            .1
            .total,
        504
    );
    drop(store);
    let mut store = Store::open(&scratch.database())?;
    assert_eq!(store.load()?.entries, queue);
    assert_eq!(
        store
            .playlist_entries(smart.id, Some(3), Some(10), 0, 1, "")?
            .1
            .total,
        504
    );
    Ok(())
}

#[test]
fn smart_snapshot_failures_and_enqueue_source_rechecks_rollback_atomically() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.database())?;
    seed_many_tracks(&store.connection)?;
    let smart = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::CreateSmart {
                name: "Live".into(),
                query: "".into(),
            },
        )?
        .playlist
        .unwrap();
    let regular = store
        .edit_playlist(
            None,
            2,
            PlaylistEdit::Create {
                name: "Regular".into(),
            },
        )?
        .playlist
        .unwrap();
    for (id, rule_revision, library, catalog, name) in [
        (smart.id, 2, 9, 3, "Stale rule"),
        (smart.id, 1, 8, 3, "Stale library"),
        (smart.id, 1, 9, 2, "Stale catalog"),
        (smart.id, 1, 9, 3, "regular"),
        (regular.id, 1, 9, 3, "Wrong kind"),
        (smart.id, 1, 9, 3, ""),
    ] {
        assert!(
            store
                .snapshot_smart_playlist(id, rule_revision, library, catalog, name)
                .is_err()
        );
    }
    store.connection.execute_batch(
        "CREATE TRIGGER fail_snapshot BEFORE INSERT ON playlist_entries WHEN NEW.ordinal=504
         BEGIN SELECT RAISE(ABORT,'injected late snapshot failure'); END;",
    )?;
    assert!(
        store
            .snapshot_smart_playlist(smart.id, 1, 9, 3, "Failed")
            .is_err()
    );
    assert_eq!(store.saved_playlists(page(Some(3)), None)?.total, 2);
    store
        .connection
        .execute_batch("DROP TRIGGER fail_snapshot;")?;
    let sources = store.playlist_sources(smart.id, 1, 9)?;
    let prepared = || {
        sources
            .iter()
            .enumerate()
            .map(|(index, (track, path))| (index as u64 + 1, *track, path.as_path()))
    };
    for (playlist, library, queue) in [(2, 9, 1), (1, 8, 1), (1, 9, 2)] {
        assert!(
            store
                .enqueue_playlist(smart.id, playlist, library, queue, prepared(), None)
                .is_err()
        );
    }
    // A full-length batch in the wrong order must not be accepted.
    assert!(
        store
            .enqueue_playlist(
                smart.id,
                1,
                9,
                1,
                sources
                    .iter()
                    .rev()
                    .enumerate()
                    .map(|(index, (track, path))| (index as u64 + 1, *track, path.as_path())),
                None
            )
            .is_err()
    );
    // Even unversioned damage between preparation and commit is caught.
    store
        .connection
        .execute_batch("UPDATE library_assets SET indexed=0 WHERE id=505;")?;
    assert!(
        store
            .enqueue_playlist(smart.id, 1, 9, 1, prepared(), None)
            .is_err()
    );
    store
        .connection
        .execute_batch("UPDATE library_assets SET indexed=1 WHERE id=505;")?;
    assert!(
        store
            .enqueue_playlist(
                smart.id,
                1,
                9,
                1,
                sources[..504]
                    .iter()
                    .enumerate()
                    .map(|(index, (track, path))| (index as u64 + 1, *track, path.as_path())),
                None
            )
            .is_err()
    );
    store.connection.execute_batch(
        "CREATE TRIGGER fail_smart_queue BEFORE INSERT ON queue WHEN NEW.id=505
         BEGIN SELECT RAISE(ABORT,'injected late enqueue failure'); END;",
    )?;
    assert!(
        store
            .enqueue_playlist(
                smart.id,
                1,
                9,
                1,
                prepared(),
                Some(Checkpoint {
                    current_id: Some(1),
                    position_frames: 0,
                    playing: true
                })
            )
            .is_err()
    );
    assert!(store.load()?.entries.is_empty());
    assert_eq!((store.load()?.revision, store.load()?.next_id), (1, 1));
    assert_eq!(store.load()?.checkpoint.current_id, None);
    store.connection.execute_batch(
        "DROP TRIGGER fail_smart_queue; UPDATE playlists SET query='artist:' WHERE id=1;",
    )?;
    // Catalog browsing never evaluates rules, including damaged persisted rules.
    assert_eq!(
        store
            .saved_playlists(page(None), Some(crate::protocol::PlaylistKind::Smart))?
            .total,
        1
    );
    assert!(
        store
            .playlist_entries(smart.id, Some(1), Some(9), 0, 1, "")
            .is_err()
    );
    assert!(
        store
            .snapshot_smart_playlist(smart.id, 1, 9, 3, "Malformed")
            .is_err()
    );
    assert!(store.playlist_sources(smart.id, 1, 9).is_err());
    assert_eq!(store.playlists_revision()?, 3);
    Ok(())
}

#[test]
fn smart_pages_hydrate_only_the_requested_rows_and_use_library_availability() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.database())?;
    seed_many_tracks(&store.connection)?;
    store.connection.execute_batch(
        "UPDATE library_tracks SET metadata='broken metadata' WHERE id=505;
         UPDATE library_assets SET last_seen_scan=0 WHERE id=503;
         UPDATE library_assets SET indexed=0 WHERE id=504;",
    )?;
    let smart = store
        .edit_playlist(
            None,
            1,
            PlaylistEdit::CreateSmart {
                name: "Bounded".into(),
                query: "artist:Artist & (album:Album | title:unknown)".into(),
            },
        )?
        .playlist
        .unwrap();
    let (_, first) = store.playlist_entries(
        smart.id,
        Some(1),
        Some(9),
        0,
        500,
        "title:retained | title:other",
    )?;
    assert_eq!((first.total, first.entries.len()), (503, 500));
    assert!(first.entries.iter().all(|entry| entry.available));
    assert!(
        store
            .playlist_entries(smart.id, Some(1), Some(9), 500, 100, "")
            .is_err()
    );
    // Whole-result paths copy identities and paths, never decode off-page metadata.
    assert_eq!(store.playlist_sources(smart.id, 1, 9)?.len(), 503);
    let frozen = store
        .snapshot_smart_playlist(smart.id, 1, 9, 2, "Narrow snapshot")?
        .playlist
        .unwrap();
    assert_eq!(frozen.entry_count, Some(503));
    assert_eq!(store.saved_playlists(page(None), None)?.total, 2);
    Ok(())
}
