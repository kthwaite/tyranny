//! Private SQLite journal of tag edits, kept so an edit can be reverted.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::tags::TagCopy;
use crate::protocol::{TagEditSummary, TagField};

const APPLICATION_ID: i64 = 0x7479_7467;
pub(super) const KEPT_EDITS: u64 = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct JournalFile {
    pub track_id: u64,
    pub path: PathBuf,
    pub before: BTreeMap<TagField, Option<String>>,
    pub after: BTreeMap<TagField, Option<String>>,
    pub removed: Vec<TagCopy>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileStatus {
    Pending,
    Written,
    Failed,
    Reverted,
}
impl FileStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Written => "written",
            Self::Failed => "failed",
            Self::Reverted => "reverted",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "pending" => Self::Pending,
            "written" => Self::Written,
            "failed" => Self::Failed,
            "reverted" => Self::Reverted,
            other => return Err(anyhow!("unknown journal file status {other:?}")),
        })
    }
}

pub(super) struct EditRecord {
    pub reverted: bool,
    pub files: Vec<(JournalFile, FileStatus)>,
}

pub(super) struct Journal {
    connection: Connection,
}

impl Journal {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "tag journal must be a regular file"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        let application: i64 = connection.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            (application == 0 && version == 0) || (application == APPLICATION_ID && version == 1),
            "unsupported private tag journal"
        );
        connection.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS edits(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL,
                release_id TEXT NOT NULL,
                release_title TEXT NOT NULL,
                reverted INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS edit_files(
                edit_id INTEGER NOT NULL REFERENCES edits(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                track_id INTEGER NOT NULL,
                body TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('pending','written','failed','reverted')),
                PRIMARY KEY(edit_id, position));",
        )?;
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
        connection.pragma_update(None, "user_version", 1)?;
        Ok(Self { connection })
    }

    /// Records an edit with every file `pending`, and returns its id.
    pub(super) fn begin(
        &mut self,
        release_id: &str,
        release_title: &str,
        files: &[JournalFile],
    ) -> Result<u64> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT INTO edits(created_at, release_id, release_title) VALUES(?1, ?2, ?3)",
            params![created_at, release_id, release_title],
        )?;
        let edit_id = tx.last_insert_rowid();
        {
            let mut insert = tx.prepare(
                "INSERT INTO edit_files(edit_id, position, track_id, body, status)
                 VALUES(?1, ?2, ?3, ?4, 'pending')",
            )?;
            for (position, file) in files.iter().enumerate() {
                insert.execute(params![
                    edit_id,
                    position as i64,
                    file.track_id as i64,
                    serde_json::to_string(file)?
                ])?;
            }
        }
        tx.execute(
            "DELETE FROM edits WHERE id <= ?1",
            [edit_id.saturating_sub(KEPT_EDITS as i64)],
        )?;
        tx.commit()?;
        Ok(edit_id as u64)
    }

    pub(super) fn set_status(
        &mut self,
        edit_id: u64,
        track_id: u64,
        status: FileStatus,
    ) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE edit_files SET status=?1 WHERE edit_id=?2 AND track_id=?3",
            params![status.as_str(), edit_id as i64, track_id as i64],
        )?;
        ensure!(changed > 0, "no journaled file for track {track_id}");
        Ok(())
    }

    /// Marks a file `written` and records the copies its write removed, which
    /// revert puts back.
    pub(super) fn set_written(
        &mut self,
        edit_id: u64,
        track_id: u64,
        removed: &[TagCopy],
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        let body: String = tx
            .query_row(
                "SELECT body FROM edit_files WHERE edit_id=?1 AND track_id=?2",
                params![edit_id as i64, track_id as i64],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("no journaled file for track {track_id}"))?;
        let mut file: JournalFile = serde_json::from_str(&body)?;
        file.removed = removed.to_vec();
        tx.execute(
            "UPDATE edit_files SET body=?1, status=?2 WHERE edit_id=?3 AND track_id=?4",
            params![
                serde_json::to_string(&file)?,
                FileStatus::Written.as_str(),
                edit_id as i64,
                track_id as i64
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn load(&self, edit_id: u64) -> Result<Option<EditRecord>> {
        let Some(reverted) = self
            .connection
            .query_row(
                "SELECT reverted FROM edits WHERE id=?1",
                [edit_id as i64],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut statement = self
            .connection
            .prepare("SELECT body, status FROM edit_files WHERE edit_id=?1 ORDER BY position")?;
        let rows = statement.query_map([edit_id as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut files = Vec::new();
        for row in rows {
            let (body, status) = row?;
            files.push((serde_json::from_str(&body)?, FileStatus::parse(&status)?));
        }
        Ok(Some(EditRecord { reverted, files }))
    }

    /// The release an edit was journaled with, as `(release_id, release_title)`.
    #[cfg(test)]
    pub(super) fn release(&self, edit_id: u64) -> Result<Option<(String, String)>> {
        Ok(self
            .connection
            .query_row(
                "SELECT release_id, release_title FROM edits WHERE id=?1",
                [edit_id as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub(super) fn mark_reverted(&mut self, edit_id: u64) -> Result<()> {
        let changed = self
            .connection
            .execute("UPDATE edits SET reverted=1 WHERE id=?1", [edit_id as i64])?;
        ensure!(changed > 0, "no journaled edit {edit_id}");
        Ok(())
    }

    /// Newest first. `file_count` counts files that did not fail.
    pub(super) fn recent(&self, limit: usize) -> Result<Vec<TagEditSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT e.id, e.created_at, e.release_title, e.reverted,
                    (SELECT COUNT(*) FROM edit_files f WHERE f.edit_id=e.id AND f.status<>'failed')
             FROM edits e ORDER BY e.id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |r| {
            Ok(TagEditSummary {
                edit_id: r.get::<_, i64>(0)? as u64,
                created_at: r.get(1)?,
                release_title: r.get(2)?,
                reverted: r.get::<_, i64>(3)? != 0,
                file_count: r.get::<_, i64>(4)? as u32,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TagEditSummary;
    use crate::tagging::tags::{CopyKey, TagKind};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tyranny-journal-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn file(track_id: u64) -> JournalFile {
        JournalFile {
            track_id,
            path: PathBuf::from(format!("/music/{track_id}.flac")),
            before: BTreeMap::from([
                (TagField::Title, Some("Old".to_string())),
                (TagField::Album, None),
            ]),
            after: BTreeMap::from([
                (TagField::Title, Some("New".to_string())),
                (TagField::Album, Some("Album".to_string())),
            ]),
            removed: vec![TagCopy {
                tag: TagKind::Id3v1,
                field: TagField::Title,
                key: CopyKey::Field,
                value: "Old".to_string(),
                read_only: false,
            }],
        }
    }

    fn open(dir: &Path) -> Journal {
        Journal::open(&dir.join("state.tagging.sqlite")).unwrap()
    }

    #[test]
    fn begin_load_round_trips_files_and_statuses() {
        let dir = temp_dir();
        let mut journal = open(&dir);
        let files = [file(7), file(9)];
        let id = journal.begin("rel-1", "Release", &files).unwrap();
        let record = journal.load(id).unwrap().unwrap();
        assert_eq!(
            journal.release(id).unwrap(),
            Some(("rel-1".into(), "Release".into()))
        );
        assert!(!record.reverted);
        assert_eq!(
            record.files,
            vec![
                (files[0].clone(), FileStatus::Pending),
                (files[1].clone(), FileStatus::Pending)
            ]
        );
        journal.set_status(id, 9, FileStatus::Written).unwrap();
        journal.set_status(id, 7, FileStatus::Failed).unwrap();
        let record = journal.load(id).unwrap().unwrap();
        assert_eq!(record.files[0].1, FileStatus::Failed);
        assert_eq!(record.files[1].1, FileStatus::Written);
        journal.set_status(id, 9, FileStatus::Reverted).unwrap();
        assert_eq!(
            journal.load(id).unwrap().unwrap().files[1].1,
            FileStatus::Reverted
        );
        assert!(journal.load(id + 1).unwrap().is_none());
    }

    #[test]
    fn set_written_records_what_the_write_removed() {
        let dir = temp_dir();
        let mut journal = open(&dir);
        let files = [file(7), file(9)];
        let id = journal.begin("r", "T", &files).unwrap();
        let removed = vec![TagCopy {
            tag: TagKind::Ape,
            field: TagField::TrackTotal,
            key: CopyKey::Field,
            value: "12".to_string(),
            read_only: false,
        }];
        journal.set_written(id, 9, &removed).unwrap();
        let record = journal.load(id).unwrap().unwrap();
        assert_eq!(record.files[0], (files[0].clone(), FileStatus::Pending));
        let mut written = files[1].clone();
        written.removed = removed;
        assert_eq!(record.files[1], (written, FileStatus::Written));
        assert!(journal.set_written(id, 8, &[]).is_err());
    }

    #[test]
    fn only_the_latest_200_edits_are_kept() {
        let dir = temp_dir();
        let mut journal = open(&dir);
        for _ in 0..205 {
            journal.begin("r", "T", &[file(1)]).unwrap();
        }
        for id in 1..=5 {
            assert!(journal.load(id).unwrap().is_none());
        }
        assert!(journal.load(6).unwrap().is_some());
        let recent = journal.recent(50).unwrap();
        assert_eq!(recent.len(), 50);
        assert_eq!(recent[0].edit_id, 205);
        assert_eq!(recent[49].edit_id, 156);
        let count: i64 = journal
            .connection
            .query_row("SELECT COUNT(*) FROM edit_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 200);
    }

    #[test]
    fn file_count_excludes_failed_files() {
        let dir = temp_dir();
        let mut journal = open(&dir);
        let id = journal
            .begin("r", "T", &[file(1), file(2), file(3)])
            .unwrap();
        journal.set_status(id, 2, FileStatus::Failed).unwrap();
        journal.set_status(id, 3, FileStatus::Written).unwrap();
        let summary: Vec<TagEditSummary> = journal.recent(10).unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].file_count, 2);
        assert_eq!(summary[0].release_title, "T");
        assert!(summary[0].created_at > 0);
    }

    #[test]
    fn mark_reverted_sets_the_flag() {
        let dir = temp_dir();
        let mut journal = open(&dir);
        let id = journal.begin("r", "T", &[file(1)]).unwrap();
        assert!(!journal.recent(1).unwrap()[0].reverted);
        journal.mark_reverted(id).unwrap();
        assert!(journal.load(id).unwrap().unwrap().reverted);
        assert!(journal.recent(1).unwrap()[0].reverted);
    }

    #[test]
    fn journal_is_private_and_refuses_symlinks_and_foreign_databases() {
        let dir = temp_dir();
        let path = dir.join("state.tagging.sqlite");
        let journal = Journal::open(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let application: i64 = journal
            .connection
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(application, 0x7479_7467);
        let version: i64 = journal
            .connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        drop(journal);

        let link = dir.join("link.sqlite");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(Journal::open(&link).is_err());

        let foreign = dir.join("foreign.sqlite");
        let connection = rusqlite::Connection::open(&foreign).unwrap();
        connection
            .pragma_update(None, "application_id", 0x1234_5678)
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        drop(connection);
        assert!(Journal::open(&foreign).is_err());

        let fresh = dir.join("fresh.sqlite");
        let journal = Journal::open(&fresh).unwrap();
        let application: i64 = journal
            .connection
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(application, 0x7479_7467);
    }

    #[test]
    fn reopen_keeps_edits() {
        let dir = temp_dir();
        let id = {
            let mut journal = open(&dir);
            journal.begin("r", "T", &[file(1)]).unwrap()
        };
        let mut journal = open(&dir);
        assert_eq!(journal.load(id).unwrap().unwrap().files.len(), 1);
        assert_eq!(journal.begin("r", "T", &[file(1)]).unwrap(), id + 1);
    }
}
