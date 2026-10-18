//! SQLite is owned exclusively by the playback command thread. Queue mutations
//! commit before the matching infallible engine change; checkpoints are separate.
use crate::{
    library,
    library_types::format_label,
    play_order::RunInfo,
    protocol::{QueueItem, QueueSource, Settings, SourceKind},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

mod playlists;
pub(crate) use playlists::{PlaylistChange, PlaylistEdit, PlaylistFailure, list_source_rule};
#[cfg(test)]
mod playlists_tests;
const APPLICATION_ID: i64 = 0x7479726e;
const SCHEMA_VERSION: i64 = 5;

/// Runs the migration ladder up to `target`; tests use a lower target to build old schemas.
pub(crate) fn migrate(connection: &mut Connection, target: i64) -> Result<i64> {
    let mut version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let application: i64 = connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    ensure!(
        application == APPLICATION_ID || (application == 0 && version == 0),
        "not a tyranny database"
    );
    if version == 0 && target > 0 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE queue (
                id INTEGER PRIMARY KEY CHECK(id > 0),
                ordinal INTEGER NOT NULL UNIQUE,
                path TEXT NOT NULL
            );
            CREATE TABLE control (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                revision INTEGER NOT NULL CHECK(revision > 0),
                next_id INTEGER NOT NULL CHECK(next_id > 0),
                current_id INTEGER REFERENCES queue(id) ON DELETE SET NULL,
                position_frames INTEGER NOT NULL CHECK(position_frames >= 0),
                playing INTEGER NOT NULL CHECK(playing IN (0,1)),
                settings TEXT NOT NULL
            );",
        )?;
        transaction.execute(
            "INSERT INTO control VALUES (1, 1, 1, NULL, 0, 0, ?1)",
            [serde_json::to_string(&Settings::default())?],
        )?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", 1)?;
        transaction.commit()?;
        version = 1;
    }
    if version == 1 && target > 1 {
        let transaction = connection.transaction()?;
        library::migrate(&transaction)?;
        transaction.execute_batch(
            "ALTER TABLE queue ADD COLUMN track_id INTEGER REFERENCES library_tracks(id);",
        )?;
        transaction.pragma_update(None, "user_version", 2)?;
        transaction.commit()?;
        version = 2;
    }
    if version == 2 && target > 2 {
        let transaction = connection.transaction()?;
        playlists::migrate(&transaction)?;
        transaction.pragma_update(None, "user_version", 3)?;
        transaction.commit()?;
        version = 3;
    }
    if version == 3 && target > 3 {
        let transaction = connection.transaction()?;
        transaction.execute_batch("ALTER TABLE playlists ADD COLUMN query TEXT;")?;
        transaction.pragma_update(None, "user_version", 4)?;
        transaction.commit()?;
        version = 4;
    }
    if version == 4 && target > 4 {
        let transaction = connection.transaction()?;
        library::migrate_v5(&transaction)?;
        transaction.execute_batch(
            "ALTER TABLE queue ADD COLUMN source_kind TEXT;
                ALTER TABLE queue ADD COLUMN source_name TEXT;
                ALTER TABLE queue ADD COLUMN run_id INTEGER;
                ALTER TABLE queue ADD COLUMN played_seq INTEGER;",
        )?;
        transaction.pragma_update(None, "user_version", 5)?;
        transaction.commit()?;
        version = 5;
    }
    ensure!(version == target, "unsupported database version {version}");
    Ok(version)
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Checkpoint {
    pub current_id: Option<u64>,
    pub position_frames: u64,
    pub playing: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoadedEntry {
    pub id: u64,
    pub path: PathBuf,
    pub run: Option<RunInfo>,
    /// Set once the entry has played. Task 12 writes it.
    #[allow(dead_code)]
    pub played_seq: Option<u64>,
}
/// What the library knows about a queue entry's indexed track.
#[derive(Clone)]
pub(crate) struct EntryTrack {
    pub track_id: u64,
    pub album_id: u64,
    pub format: String,
}
pub(crate) struct Loaded {
    pub entries: Vec<LoadedEntry>,
    pub revision: u64,
    pub next_id: u64,
    pub playlists_revision: u64,
    pub checkpoint: Checkpoint,
    pub settings: Settings,
}
pub(crate) enum Edit<'a> {
    Append {
        id: u64,
        path: &'a str,
        track: Option<(u64, u64)>,
        source: Option<&'a QueueSource>,
        run_id: Option<u64>,
    },
    Remove {
        id: u64,
    },
    Clear,
}
/// File name without extension, for entries the library cannot describe.
fn file_stem(path: &str) -> String {
    Path::new(path).file_stem().map_or_else(
        || path.to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}
pub(crate) struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            ensure!(
                metadata.file_type().is_file(),
                "database must be a regular file, not a symlink"
            );
        }
        let mut connection = Connection::open(path).context("opening queue database")?;
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        connection.busy_timeout(Duration::from_millis(100))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        migrate(&mut connection, SCHEMA_VERSION)?;
        Ok(Self { connection })
    }

    pub fn load(&self) -> Result<Loaded> {
        let (revision, next_id, current_id, position_frames, playing, settings): (u64,u64,Option<u64>,u64,bool,String) = self.connection.query_row(
            "SELECT revision,next_id,current_id,position_frames,playing,settings FROM control WHERE singleton=1", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?;
        let mut query = self.connection.prepare(
            "SELECT id,path,ordinal,source_kind,run_id,played_seq FROM queue ORDER BY ordinal",
        )?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<u64>>(4)?,
                row.get::<_, Option<u64>>(5)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (id, path, ordinal, kind, run_id, played_seq) = row?;
            ensure!(
                id > 0 && id < next_id && ordinal >= 0,
                "invalid persisted queue identity/order"
            );
            let path = PathBuf::from(path);
            ensure!(path.is_absolute(), "persisted source path is not absolute");
            let run =
                kind.as_deref()
                    .and_then(SourceKind::parse)
                    .zip(run_id)
                    .map(|(kind, run_id)| RunInfo {
                        run_id,
                        scope: kind.repeat_scope(),
                    });
            entries.push(LoadedEntry {
                id,
                path,
                run,
                played_seq,
            });
        }
        ensure!(
            current_id.is_none_or(|id| entries.iter().any(|entry| entry.id == id)),
            "checkpoint refers to a missing queue occurrence"
        );
        Ok(Loaded {
            entries,
            revision,
            next_id,
            playlists_revision: self.playlists_revision()?,
            checkpoint: Checkpoint {
                current_id,
                position_frames,
                playing,
            },
            settings: serde_json::from_str(&settings).context("reading persisted settings")?,
        })
    }

    pub fn checkpoint(&self, checkpoint: Checkpoint, settings: &Settings) -> Result<()> {
        self.connection.execute("UPDATE control SET current_id=?1,position_frames=?2,playing=?3,settings=?4 WHERE singleton=1",
            params![checkpoint.current_id, checkpoint.position_frames, checkpoint.playing, serde_json::to_string(settings)?])?;
        Ok(())
    }

    pub fn track_source(&self, id: u64, revision: u64) -> Result<String> {
        library::resolve_track(&self.connection, id, revision)
    }

    pub fn queue_page(&self, offset: usize, limit: usize) -> Result<Vec<QueueItem>> {
        let mut query = self.connection.prepare(
            "SELECT q.id,q.path,q.track_id,q.source_kind,q.source_name,q.run_id,q.played_seq,
                    json_extract(t.metadata,'$.title'),json_extract(t.metadata,'$.artist'),
                    al.title,t.album_id,json_extract(t.metadata,'$.duration_ms')
             FROM queue q
             LEFT JOIN library_tracks t ON t.id=q.track_id
             LEFT JOIN library_albums al ON al.id=t.album_id
             ORDER BY q.ordinal LIMIT ?1 OFFSET ?2",
        )?;
        Ok(query
            .query_map(params![limit, offset], |row| {
                let path: String = row.get(1)?;
                let kind: Option<String> = row.get(3)?;
                let name: Option<String> = row.get(4)?;
                let title: Option<String> = row.get(7)?;
                Ok(QueueItem {
                    id: row.get(0)?,
                    track_id: row.get(2)?,
                    source: kind
                        .as_deref()
                        .and_then(SourceKind::parse)
                        .map(|kind| QueueSource {
                            kind,
                            name: name.unwrap_or_default(),
                        }),
                    run_id: row.get(5)?,
                    played: row.get::<_, Option<u64>>(6)?.is_some(),
                    title: title
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| file_stem(&path)),
                    artist: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    album: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    album_id: row.get(10)?,
                    duration_ms: row.get(11)?,
                    path,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The indexed track behind queue entry `entry_id`, or `None` when it has
    /// no track or the track is gone.
    pub fn entry_track(&self, entry_id: u64) -> Result<Option<EntryTrack>> {
        Ok(self
            .connection
            .query_row(
                "SELECT t.id,t.album_id,t.codec,t.bit_depth,
                        COALESCE(json_extract(t.metadata,'$.sample_rate'),0),t.bitrate_kbps
                 FROM queue q JOIN library_tracks t ON t.id=q.track_id WHERE q.id=?1",
                [entry_id],
                |row| {
                    let codec: Option<String> = row.get(2)?;
                    Ok(EntryTrack {
                        track_id: row.get(0)?,
                        album_id: row.get(1)?,
                        format: format_label(
                            codec.as_deref(),
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ),
                    })
                },
            )
            .optional()?)
    }

    pub fn edit(&mut self, edit: Edit<'_>, revision: u64, next_id: u64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match edit {
            Edit::Append {
                id,
                path,
                track,
                source,
                run_id,
            } => {
                if let Some((track_id, library_revision)) = track {
                    let indexed = library::resolve_track(&transaction, track_id, library_revision)?;
                    ensure!(indexed == path, "indexed source changed before enqueue");
                }
                transaction.execute(
                    "INSERT INTO queue(id,ordinal,path,track_id,source_kind,source_name,run_id)
                     SELECT ?1,COALESCE(MAX(ordinal),-1)+1,?2,?3,?4,?5,?6 FROM queue",
                    params![
                        id,
                        path,
                        track.map(|track| track.0),
                        source.map(|source| source.kind.as_str()),
                        source.map(|source| source.name.as_str()),
                        run_id
                    ],
                )?;
            }
            Edit::Remove { id } => {
                transaction.execute("UPDATE control SET current_id=NULL,position_frames=0,playing=0 WHERE current_id=?1", [id])?;
                ensure!(
                    transaction.execute("DELETE FROM queue WHERE id=?1", [id])? == 1,
                    "persisted queue occurrence not found"
                );
            }
            Edit::Clear => {
                transaction.execute("UPDATE control SET current_id=NULL,position_frames=0,playing=0 WHERE singleton=1", [])?;
                transaction.execute("DELETE FROM queue", [])?;
            }
        }
        transaction.execute(
            "UPDATE control SET revision=?1,next_id=?2 WHERE singleton=1",
            params![revision, next_id],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        play_order::{RepeatScope, RunInfo},
        protocol::{QueueSource, SourceKind},
    };
    #[test]
    fn failed_edit_rolls_back_queue_and_revision() -> Result<()> {
        let path =
            std::env::temp_dir().join(format!("tyranny-store-{}.sqlite", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut store = Store::open(&path)?;
            store.edit(
                Edit::Append {
                    id: 1,
                    path: "/music/a.flac",
                    track: None,
                    source: None,
                    run_id: None,
                },
                2,
                2,
            )?;
            assert!(
                store
                    .edit(
                        Edit::Append {
                            id: 1,
                            path: "/music/b.flac",
                            track: None,
                            source: None,
                            run_id: None,
                        },
                        3,
                        3
                    )
                    .is_err()
            );
            let loaded = store.load()?;
            assert_eq!(
                loaded
                    .entries
                    .iter()
                    .map(|entry| (entry.id, entry.path.clone()))
                    .collect::<Vec<_>>(),
                vec![(1, PathBuf::from("/music/a.flac"))]
            );
            assert_eq!((loaded.revision, loaded.next_id), (2, 2));
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("tyranny-store-{}.sqlite", uuid::Uuid::new_v4()))
    }
    fn user_version(path: &Path) -> i64 {
        let connection = Connection::open(path).unwrap();
        connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    }
    fn pending_upgrade(path: &Path) -> i64 {
        let connection = Connection::open(path).unwrap();
        connection
            .query_row(
                "SELECT pending_upgrade FROM library_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
    fn column_default(path: &Path, table: &str, column: &str) -> i64 {
        let connection = Connection::open(path).unwrap();
        let default: String = connection
            .query_row(
                &format!("SELECT dflt_value FROM pragma_table_info('{table}') WHERE name=?1"),
                [column],
                |row| row.get(0),
            )
            .unwrap();
        default.parse().unwrap()
    }
    fn table_exists(path: &Path, table: &str) -> bool {
        let connection = Connection::open(path).unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }
    fn schema_4_file(populated: bool) -> PathBuf {
        let path = temp_path();
        let mut connection = Connection::open(&path).unwrap();
        assert_eq!(migrate(&mut connection, 4).unwrap(), 4);
        if populated {
            connection
                .execute_batch(
                    "INSERT INTO library_roots(id,path,search) VALUES(1,'/music','/music');
                    INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan)
                        VALUES(1,1,'/music/a.flac',1,1);
                    INSERT INTO library_artists(id,identity,name,search) VALUES(1,'a','A','a');
                    INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation)
                        VALUES(1,'b',1,'B','A','b','b',0);
                    INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number)
                        VALUES(1,1,1,1,'{}','t','t',1,1,1);
                    INSERT INTO queue(id,ordinal,path,track_id) VALUES(1,0,'/music/a.flac',1);
                    UPDATE control SET next_id=2,current_id=1;
                    INSERT INTO playlists(id,name,search,revision) VALUES(1,'P','p',1);
                    INSERT INTO playlist_entries(playlist_id,track_id,ordinal) VALUES(1,1,0);
                    INSERT INTO playlists(id,name,search,revision,query) VALUES(2,'S','s',1,'artist:a');",
                )
                .unwrap();
        }
        path
    }
    fn source(kind: SourceKind, name: &str) -> QueueSource {
        QueueSource {
            kind,
            name: name.to_owned(),
        }
    }
    #[test]
    fn queue_rows_persist_source_run_and_played_through_restart() -> Result<()> {
        let path = temp_path();
        let result = (|| -> Result<()> {
            let mut store = Store::open(&path)?;
            let album = source(SourceKind::Album, "Kind of Blue");
            let songs = source(SourceKind::Songs, "");
            for (id, name, source, run) in [
                (1, "a", Some(&album), Some(1)),
                (2, "b", Some(&album), Some(1)),
                (3, "c", Some(&songs), Some(3)),
                (4, "d", None, None),
            ] {
                store.edit(
                    Edit::Append {
                        id,
                        path: &format!("/music/{name}.flac"),
                        track: None,
                        source,
                        run_id: run,
                    },
                    id + 1,
                    id + 1,
                )?;
            }
            Connection::open(&path)?.execute("UPDATE queue SET played_seq=7 WHERE id=2", [])?;
            drop(store);
            let loaded = Store::open(&path)?.load()?;
            let album_run = Some(RunInfo {
                run_id: 1,
                scope: RepeatScope::Run,
            });
            let songs_run = Some(RunInfo {
                run_id: 3,
                scope: RepeatScope::Queue,
            });
            assert_eq!(
                loaded
                    .entries
                    .iter()
                    .map(|entry| (entry.id, entry.run, entry.played_seq))
                    .collect::<Vec<_>>(),
                [
                    (1, album_run, None),
                    (2, album_run, Some(7)),
                    (3, songs_run, None),
                    (4, None, None)
                ]
            );
            let page = Store::open(&path)?.queue_page(0, 10)?;
            assert_eq!(
                page.iter()
                    .map(|item| (item.source.clone(), item.run_id, item.played))
                    .collect::<Vec<_>>(),
                [
                    (Some(album.clone()), Some(1), false),
                    (Some(album), Some(1), true),
                    (Some(songs), Some(3), false),
                    (None, None, false)
                ]
            );
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }
    #[test]
    fn queue_page_returns_track_display_fields_and_stem_fallback() -> Result<()> {
        let path = temp_path();
        let result = (|| -> Result<()> {
            let store = Store::open(&path)?;
            let connection = Connection::open(&path)?;
            // Foreign keys off lets row 3 point at a track that no longer exists.
            connection.execute_batch(
                "PRAGMA foreign_keys=OFF;
                INSERT INTO library_roots(id,path,search) VALUES(1,'/music','/music');
                INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan)
                    VALUES(1,1,'/music/a.flac',1,1);
                INSERT INTO library_artists(id,identity,name,search) VALUES(1,'a','A','a');
                INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation)
                    VALUES(7,'b',1,'Album B','A','b','b',0);
                INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number)
                    VALUES(3,1,1,7,'{\"title\":\"Title A\",\"artist\":\"Artist A\",\"duration_ms\":1234}','t','t',1,1,1);
                INSERT INTO queue(id,ordinal,path,track_id) VALUES(1,0,'/music/a.flac',3);
                INSERT INTO queue(id,ordinal,path,track_id) VALUES(2,1,'/elsewhere/Loose Song.flac',NULL);
                INSERT INTO queue(id,ordinal,path,track_id) VALUES(3,2,'/music/gone.flac',99);
                UPDATE control SET next_id=4;",
            )?;
            let page = store.queue_page(0, 10)?;
            let shown = |index: usize| {
                let item = &page[index];
                (
                    item.title.as_str(),
                    item.artist.as_str(),
                    item.album.as_str(),
                    item.album_id,
                    item.duration_ms,
                )
            };
            assert_eq!(page[0].track_id, Some(3));
            assert_eq!(
                shown(0),
                ("Title A", "Artist A", "Album B", Some(7), Some(1234))
            );
            assert_eq!(shown(1), ("Loose Song", "", "", None, None));
            assert_eq!(page[2].track_id, Some(99));
            assert_eq!(shown(2), ("gone", "", "", None, None));
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }
    #[test]
    fn schema_4_database_migrates_to_5_preserving_state() -> Result<()> {
        let path = schema_4_file(true);
        assert_eq!(user_version(&path), 4);
        let result = (|| -> Result<()> {
            let store = Store::open(&path)?;
            let loaded = store.load()?;
            assert_eq!(user_version(&path), 5);
            assert_eq!(loaded.entries.len(), 1);
            assert_eq!(loaded.checkpoint.current_id, Some(1));
            assert_eq!(pending_upgrade(&path), 1);
            assert_eq!(column_default(&path, "library_tracks", "date_added"), 0);
            assert!(table_exists(&path, "play_history") && table_exists(&path, "track_stats"));
            let connection = Connection::open(&path)?;
            let counts: (i64, i64, i64) = connection.query_row(
                "SELECT (SELECT COUNT(*) FROM playlist_entries),
                        (SELECT COUNT(*) FROM playlists WHERE query IS NOT NULL),
                        (SELECT COUNT(*) FROM queue WHERE source_kind IS NULL AND run_id IS NULL AND played_seq IS NULL)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(counts, (1, 1, 1));
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }
    #[test]
    fn empty_schema_4_database_migrates_without_pending_upgrade() -> Result<()> {
        let path = schema_4_file(false);
        let result = (|| -> Result<()> {
            Store::open(&path)?;
            assert_eq!(user_version(&path), 5);
            assert_eq!(pending_upgrade(&path), 0);
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }
}
