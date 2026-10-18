//! SQLite is owned exclusively by the playback command thread. Queue mutations
//! commit before the matching infallible engine change; checkpoints are separate.
use crate::{
    library,
    protocol::{QueueItem, Settings},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

mod playlists;
pub(crate) use playlists::{PlaylistChange, PlaylistEdit, PlaylistFailure};
#[cfg(test)]
mod playlists_tests;
const APPLICATION_ID: i64 = 0x7479726e;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Checkpoint {
    pub current_id: Option<u64>,
    pub position_frames: u64,
    pub playing: bool,
}
pub(crate) struct Loaded {
    pub entries: Vec<(u64, PathBuf)>,
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
    },
    Remove {
        id: u64,
    },
    Clear,
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
        let mut version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let application: i64 =
            connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
        ensure!(
            application == APPLICATION_ID || (application == 0 && version == 0),
            "not a tyranny database"
        );
        if version == 0 {
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
        if version == 1 {
            let transaction = connection.transaction()?;
            library::migrate(&transaction)?;
            transaction.execute_batch(
                "ALTER TABLE queue ADD COLUMN track_id INTEGER REFERENCES library_tracks(id);",
            )?;
            transaction.pragma_update(None, "user_version", 2)?;
            transaction.commit()?;
            version = 2;
        }
        if version == 2 {
            let transaction = connection.transaction()?;
            playlists::migrate(&transaction)?;
            transaction.pragma_update(None, "user_version", 3)?;
            transaction.commit()?;
            version = 3;
        }
        if version == 3 {
            let transaction = connection.transaction()?;
            transaction.execute_batch("ALTER TABLE playlists ADD COLUMN query TEXT;")?;
            transaction.pragma_update(None, "user_version", 4)?;
            transaction.commit()?;
            version = 4;
        }
        ensure!(version == 4, "unsupported database version {version}");
        Ok(Self { connection })
    }

    pub fn load(&self) -> Result<Loaded> {
        let (revision, next_id, current_id, position_frames, playing, settings): (u64,u64,Option<u64>,u64,bool,String) = self.connection.query_row(
            "SELECT revision,next_id,current_id,position_frames,playing,settings FROM control WHERE singleton=1", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?;
        let mut query = self
            .connection
            .prepare("SELECT id,path,ordinal FROM queue ORDER BY ordinal")?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (id, path, ordinal) = row?;
            ensure!(
                id > 0 && id < next_id && ordinal >= 0,
                "invalid persisted queue identity/order"
            );
            let path = PathBuf::from(path);
            ensure!(path.is_absolute(), "persisted source path is not absolute");
            entries.push((id, path));
        }
        ensure!(
            current_id.is_none_or(|id| entries.iter().any(|entry| entry.0 == id)),
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
        let mut query = self
            .connection
            .prepare("SELECT id,path,track_id FROM queue ORDER BY ordinal LIMIT ?1 OFFSET ?2")?;
        Ok(query
            .query_map(params![limit, offset], |row| {
                Ok(QueueItem {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    track_id: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn edit(&mut self, edit: Edit<'_>, revision: u64, next_id: u64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match edit {
            Edit::Append { id, path, track } => {
                if let Some((track_id, library_revision)) = track {
                    let indexed = library::resolve_track(&transaction, track_id, library_revision)?;
                    ensure!(indexed == path, "indexed source changed before enqueue");
                }
                transaction.execute("INSERT INTO queue(id,ordinal,path,track_id) SELECT ?1,COALESCE(MAX(ordinal),-1)+1,?2,?3 FROM queue", params![id,path,track.map(|track| track.0)])?;
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
                        },
                        3,
                        3
                    )
                    .is_err()
            );
            let loaded = store.load()?;
            assert_eq!(loaded.entries, vec![(1, PathBuf::from("/music/a.flac"))]);
            assert_eq!((loaded.revision, loaded.next_id), (2, 2));
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }
}
