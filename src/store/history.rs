//! Counted plays. Each one adds a `play_history` row and, for an indexed track,
//! updates its `track_stats` row in the same commit. The playback owner is
//! the only writer of both. A play is neither a library nor a queue edit, so
//! it changes neither revision; it changes the history revision.
use super::{Store, source};
use crate::{
    SAMPLE_RATE,
    library_types::Page,
    protocol::{HistoryEntry, QueueItem, QueueSource},
};
use anyhow::Result;
use rusqlite::{TransactionBehavior, params};
use std::collections::VecDeque;

/// A play counts after at most this long, however long the track is.
const MAX_THRESHOLD_SECS: u64 = 240;
/// The most counted plays kept while their history writes fail.
pub(crate) const MAX_PENDING_PLAYS: usize = 1_000;

/// The position at which a pass over a track `duration_frames` long counts as
/// a play: half the duration or 4 minutes, whichever comes first. `None` for
/// an unknown duration: such a pass counts only at its natural end.
pub(crate) fn count_threshold_frames(duration_frames: Option<u64>) -> Option<u64> {
    duration_frames.map(|duration| (duration / 2).min(MAX_THRESHOLD_SECS * u64::from(SAMPLE_RATE)))
}

/// One counted play of a queue entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlayRecord {
    pub entry_id: u64,
    /// `None` for an unindexed (direct-path) entry, which has no statistics.
    pub track_id: Option<u64>,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: Option<u64>,
    pub source: Option<QueueSource>,
    /// UTC seconds.
    pub played_at: i64,
}
impl PlayRecord {
    /// A play of queue entry `item` at `played_at`, with its queue page fields.
    pub fn of(item: QueueItem, played_at: i64) -> Self {
        Self {
            entry_id: item.id,
            track_id: item.track_id,
            path: item.path,
            title: item.title,
            artist: item.artist,
            album: item.album,
            duration_ms: item.duration_ms,
            source: item.source,
            played_at,
        }
    }
}

/// Counted plays not yet written, oldest first: the newest
/// `MAX_PENDING_PLAYS` of them.
#[derive(Default)]
pub(crate) struct PendingPlays(VecDeque<PlayRecord>);
impl PendingPlays {
    /// Adds `play` last. Returns the oldest play, dropped to make room.
    pub fn push(&mut self, play: PlayRecord) -> Option<PlayRecord> {
        let dropped = if self.0.len() >= MAX_PENDING_PLAYS {
            self.0.pop_front()
        } else {
            None
        };
        self.0.push_back(play);
        dropped
    }
    pub fn front(&self) -> Option<&PlayRecord> {
        self.0.front()
    }
    pub fn pop_front(&mut self) -> Option<PlayRecord> {
        self.0.pop_front()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Store {
    /// Adds `play` to the history and, for an indexed track, adds one to its
    /// play count and sets its last played time, in one commit.
    pub fn record_play(&mut self, play: &PlayRecord) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,
                duration_ms,source_kind,source_name) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                play.played_at,
                play.entry_id,
                play.track_id,
                play.path,
                play.title,
                play.artist,
                play.album,
                play.duration_ms,
                play.source.as_ref().map(|source| source.kind.as_str()),
                play.source.as_ref().map(|source| source.name.as_str()),
            ],
        )?;
        if let Some(track_id) = play.track_id {
            transaction.execute(
                "INSERT INTO track_stats(track_id,play_count,last_played) VALUES(?1,1,?2)
                 ON CONFLICT(track_id) DO UPDATE SET
                    play_count=play_count+1,last_played=excluded.last_played",
                params![track_id, play.played_at],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Counted plays, newest first.
    pub fn history_page(&self, offset: usize, limit: usize) -> Result<Page<HistoryEntry>> {
        let total = self
            .connection
            .query_row("SELECT COUNT(*) FROM play_history", [], |row| row.get(0))?;
        let mut query = self.connection.prepare(
            "SELECT id,played_at,track_id,title,artist,album,duration_ms,source_kind,source_name
             FROM play_history ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let entries = query
            .query_map(params![limit, offset], |row| {
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    played_at: row.get(1)?,
                    track_id: row.get(2)?,
                    title: row.get(3)?,
                    artist: row.get(4)?,
                    album: row.get(5)?,
                    duration_ms: row.get(6)?,
                    source: source(row.get(7)?, row.get(8)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Page {
            revision: self.history_revision()?,
            total,
            offset,
            entries,
        })
    }

    /// The newest history ID, or 0 before the first play.
    pub fn history_revision(&self) -> Result<u64> {
        Ok(self.connection.query_row(
            "SELECT COALESCE(MAX(id),0) FROM play_history",
            [],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SAMPLE_RATE,
        protocol::{QueueSource, SourceKind},
        store::Store,
    };
    use rusqlite::Connection;
    use std::{fs, path::PathBuf};

    struct Database(PathBuf);
    impl Drop for Database {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    impl Database {
        /// Track 3 is indexed; nothing has been played.
        fn open() -> (Self, Store) {
            let path = std::env::temp_dir()
                .join(format!("tyranny-history-{}.sqlite", uuid::Uuid::new_v4()));
            let store = Store::open(&path).unwrap();
            Connection::open(&path)
                .unwrap()
                .execute_batch(
                    "INSERT INTO library_roots(id,path,search) VALUES(1,'/music','/music');
                    INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan)
                        VALUES(1,1,'/music/a.flac',1,1);
                    INSERT INTO library_artists(id,identity,name,search) VALUES(1,'a','A','a');
                    INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation)
                        VALUES(1,'b',1,'B','A','b','b',0);
                    INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number)
                        VALUES(3,1,1,1,'{}','t','t',1,1,1);",
                )
                .unwrap();
            (Self(path), store)
        }
        fn query<T>(
            &self,
            sql: &str,
            row: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
        ) -> Vec<T> {
            let connection = Connection::open(&self.0).unwrap();
            let mut query = connection.prepare(sql).unwrap();
            query
                .query_map([], row)
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        }
        fn stats(&self) -> Vec<(u64, u64, Option<i64>)> {
            self.query(
                "SELECT track_id,play_count,last_played FROM track_stats ORDER BY track_id",
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
        }
        /// The library and queue revisions.
        fn revisions(&self) -> Vec<(u64, u64)> {
            self.query(
                "SELECT (SELECT revision FROM library_state),(SELECT revision FROM control)",
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        }
    }
    fn album() -> Option<QueueSource> {
        Some(QueueSource {
            kind: SourceKind::Album,
            name: "Album".into(),
        })
    }
    fn play(entry_id: u64, track_id: Option<u64>, title: &str, played_at: i64) -> PlayRecord {
        PlayRecord {
            entry_id,
            track_id,
            path: format!("/music/{title}.flac"),
            title: title.into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: Some(1_000),
            source: album(),
            played_at,
        }
    }
    fn titles(page: &Page<HistoryEntry>) -> Vec<&str> {
        page.entries
            .iter()
            .map(|entry| entry.title.as_str())
            .collect()
    }

    #[test]
    fn pending_plays_keep_the_newest_1000_in_order() {
        let mut pending = PendingPlays::default();
        for id in 1..=1_000 {
            assert_eq!(pending.push(play(id, None, "Waiting", id as i64)), None);
        }
        let dropped = pending.push(play(1_001, None, "Waiting", 1_001));
        assert_eq!(dropped.map(|play| play.entry_id), Some(1));
        assert_eq!(pending.len(), 1_000);
        assert_eq!(pending.front().map(|play| play.entry_id), Some(2));
        pending.pop_front();
        assert_eq!(pending.front().map(|play| play.entry_id), Some(3));
    }

    #[test]
    fn a_long_track_counts_at_four_minutes() {
        let minutes = |count: u64| count * 60 * u64::from(SAMPLE_RATE);
        assert_eq!(count_threshold_frames(Some(minutes(20))), Some(minutes(4)));
        assert_eq!(count_threshold_frames(Some(minutes(8))), Some(minutes(4)));
        // Shorter tracks count at half their duration.
        assert_eq!(count_threshold_frames(Some(minutes(6))), Some(minutes(3)));
        assert_eq!(count_threshold_frames(Some(48_001)), Some(24_000));
        // Unknown durations count only at their natural end.
        assert_eq!(count_threshold_frames(None), None);
    }

    #[test]
    fn unindexed_entries_record_history_without_stats() -> Result<()> {
        let (database, mut store) = Database::open();
        store.record_play(&play(1, None, "Loose", 100))?;
        assert!(database.stats().is_empty());
        store.record_play(&play(2, Some(3), "Indexed", 200))?;
        store.record_play(&play(3, Some(3), "Indexed", 300))?;
        assert_eq!(database.stats(), [(3, 2, Some(300))]);
        let page = store.history_page(0, 10)?;
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| (entry.track_id, entry.title.as_str()))
                .collect::<Vec<_>>(),
            [(Some(3), "Indexed"), (Some(3), "Indexed"), (None, "Loose")]
        );
        Ok(())
    }

    #[test]
    fn history_pages_newest_first_and_revision_tracks_inserts() -> Result<()> {
        let (database, mut store) = Database::open();
        let revisions = database.revisions();
        assert_eq!(store.history_revision()?, 0);
        let empty = store.history_page(0, 10)?;
        assert_eq!(
            (empty.revision, empty.total, empty.entries.len()),
            (0, 0, 0)
        );
        for (index, title) in ["First", "Second", "Third"].into_iter().enumerate() {
            let id = index as u64 + 1;
            store.record_play(&play(id, Some(3), title, 1_000 + id as i64))?;
            assert_eq!(store.history_revision()?, id);
        }
        let page = store.history_page(0, 2)?;
        assert_eq!((page.revision, page.total, page.offset), (3, 3, 0));
        assert_eq!(titles(&page), ["Third", "Second"]);
        let page = store.history_page(2, 2)?;
        assert_eq!((page.revision, page.total, page.offset), (3, 3, 2));
        assert_eq!(
            page.entries,
            [HistoryEntry {
                id: 1,
                played_at: 1_001,
                track_id: Some(3),
                title: "First".into(),
                artist: "Artist".into(),
                album: "Album".into(),
                duration_ms: Some(1_000),
                source: album(),
            }]
        );
        // Plays are not library or queue edits.
        assert_eq!(database.revisions(), revisions);
        Ok(())
    }
}
