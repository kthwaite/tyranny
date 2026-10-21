//! Plays. Each one is a `play_history` row, opened when it starts and closed
//! when it ends, with its listened time and end reason. A play counts once,
//! and only then does it appear in the history and update its `track_stats`
//! row, in one commit. The playback owner is the only writer of both. A play
//! is neither a library nor a queue edit, so it changes neither revision; a
//! counted play changes the history revision, and every write the stats one.
use super::{Store, source};
use crate::{
    SAMPLE_RATE,
    library_types::Page,
    protocol::{HistoryEntry, QueueItem, QueueSource},
};
use anyhow::Result;
use rusqlite::{TransactionBehavior, params};
use std::collections::{HashMap, VecDeque};

/// A play counts after at most this long, however long the track is.
const MAX_THRESHOLD_SECS: u64 = 240;
/// The most play events kept while their history writes fail.
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
    /// The stream title as the station sent it, for radio.
    pub raw_title: Option<String>,
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
            raw_title: None,
        }
    }
}

/// Why a play ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// The track played to its end.
    Finished,
    /// The listener moved on, or the entry was removed.
    Skipped,
    /// Playback stopped: the queue was cleared, or the daemon went away.
    Stopped,
}
impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Skipped => "skipped",
            Self::Stopped => "stopped",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "finished" => Some(Self::Finished),
            "skipped" => Some(Self::Skipped),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// A play row that has not ended yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenPlayRow {
    pub row: i64,
    pub entry_id: u64,
    pub counted: bool,
    pub listened_ms: u64,
    /// Radio only: the station's now-playing text the play opened with.
    pub raw_title: Option<String>,
}

/// One change to a play row, in the order it happened. `play` names the play
/// within the owner; the row ID is known only once `Open` is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HistoryEvent {
    Open {
        play: u64,
        record: PlayRecord,
    },
    Count {
        play: u64,
    },
    Checkpoint {
        play: u64,
        listened_ms: u64,
    },
    Close {
        play: u64,
        listened_ms: u64,
        reason: EndReason,
        ended_at: i64,
    },
}
impl HistoryEvent {
    pub fn play(&self) -> u64 {
        match self {
            Self::Open { play, .. }
            | Self::Count { play }
            | Self::Checkpoint { play, .. }
            | Self::Close { play, .. } => *play,
        }
    }
}

/// What one `PendingHistory::write` did.
#[derive(Debug, Default)]
pub(crate) struct HistoryWrite {
    /// Some event was written.
    pub wrote: bool,
    /// Some play counted, so the history revision moved.
    pub counted: bool,
    /// The failure that stopped the write; its event and the later ones wait.
    pub error: Option<anyhow::Error>,
}

/// Play events not yet written, oldest first: the newest `MAX_PENDING_PLAYS`
/// of them.
#[derive(Default)]
pub(crate) struct PendingHistory {
    events: VecDeque<HistoryEvent>,
    /// The row of each play whose `Open` was written and which has not closed.
    rows: HashMap<u64, i64>,
}
impl PendingHistory {
    /// Adds `event` last. Returns the oldest event, dropped to make room. A
    /// play whose close is dropped is forgotten; its row stays open until
    /// startup closes it.
    pub fn push(&mut self, event: HistoryEvent) -> Option<HistoryEvent> {
        let dropped = if self.events.len() >= MAX_PENDING_PLAYS {
            self.events.pop_front()
        } else {
            None
        };
        if let Some(HistoryEvent::Close { play, .. }) = &dropped {
            self.rows.remove(play);
        }
        self.events.push_back(event);
        dropped
    }
    /// Tells the queue that `play` already has `row`, as for an open row
    /// restored at startup.
    pub fn adopt(&mut self, play: u64, row: i64) {
        self.rows.insert(play, row);
    }
    /// Writes the events oldest first, one small commit each. The first
    /// failure stops the write. An event of a play with no row is discarded:
    /// its `Open` was dropped.
    pub fn write(&mut self, store: &mut Store) -> HistoryWrite {
        let mut result = HistoryWrite::default();
        while let Some(event) = self.events.front() {
            let row = self.rows.get(&event.play()).copied();
            let outcome = match (event, row) {
                (HistoryEvent::Open { play, record }, _) => store.open_play(record).map(|row| {
                    self.rows.insert(*play, row);
                }),
                (_, None) => {
                    self.events.pop_front();
                    continue;
                }
                (HistoryEvent::Count { .. }, Some(row)) => store.count_play(row).map(|()| {
                    result.counted = true;
                }),
                (HistoryEvent::Checkpoint { listened_ms, .. }, Some(row)) => {
                    store.checkpoint_play(row, *listened_ms)
                }
                (
                    HistoryEvent::Close {
                        play,
                        listened_ms,
                        reason,
                        ended_at,
                    },
                    Some(row),
                ) => store
                    .close_play(row, *listened_ms, *reason, *ended_at)
                    .map(|()| {
                        self.rows.remove(play);
                    }),
            };
            if let Err(error) = outcome {
                result.error = Some(error);
                break;
            }
            self.events.pop_front();
            result.wrote = true;
        }
        result
    }
    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl Store {
    /// Adds an open row for `play`: not counted, with no listened time. It is
    /// absent from the history and the track statistics until it counts.
    pub fn open_play(&mut self, play: &PlayRecord) -> Result<i64> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,
                duration_ms,source_kind,source_name,raw_title,counted,listened_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,0)",
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
                play.raw_title,
            ],
        )?;
        let row = transaction.last_insert_rowid();
        bump_writes(&transaction)?;
        transaction.commit()?;
        Ok(row)
    }

    /// Marks `row` counted and, for an indexed track, adds one to its play
    /// count and sets its last played time, in one commit. Does nothing to a
    /// row that has already counted.
    pub fn count_play(&mut self, row: i64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let counted = transaction.execute(
            "UPDATE play_history SET counted=1 WHERE id=?1 AND counted=0",
            [row],
        )?;
        if counted == 1 {
            transaction.execute(
                "INSERT INTO track_stats(track_id,play_count,last_played)
                 SELECT track_id,1,played_at FROM play_history
                 WHERE id=?1 AND track_id IS NOT NULL
                 ON CONFLICT(track_id) DO UPDATE SET
                    play_count=play_count+1,last_played=excluded.last_played",
                [row],
            )?;
        }
        bump_writes(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// Saves the listened time of the open row `row`.
    pub fn checkpoint_play(&mut self, row: i64, listened_ms: u64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE play_history SET listened_ms=?2 WHERE id=?1",
            params![row, listened_ms],
        )?;
        bump_writes(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// Ends `row` with its final listened time and reason.
    pub fn close_play(
        &mut self,
        row: i64,
        listened_ms: u64,
        reason: EndReason,
        ended_at: i64,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE play_history SET listened_ms=?2,end_reason=?3,ended_at=?4 WHERE id=?1",
            params![row, listened_ms, reason.as_str(), ended_at],
        )?;
        bump_writes(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// The rows not yet ended, oldest first. A row from before schema 8 has no
    /// listened time and is not open.
    pub fn open_play_rows(&self) -> Result<Vec<OpenPlayRow>> {
        let mut query = self.connection.prepare(
            "SELECT id,entry_id,counted,listened_ms,raw_title FROM play_history
             WHERE ended_at IS NULL AND listened_ms IS NOT NULL ORDER BY id",
        )?;
        let rows = query
            .query_map([], |row| {
                Ok(OpenPlayRow {
                    row: row.get(0)?,
                    entry_id: row.get(1)?,
                    counted: row.get(2)?,
                    listened_ms: row.get(3)?,
                    raw_title: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Closes every open row but `keep` as stopped, ended when its listening
    /// would have stopped. Returns how many it closed.
    pub fn close_open_plays_except(&mut self, keep: Option<i64>) -> Result<usize> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let closed = transaction.execute(
            "UPDATE play_history SET end_reason='stopped',ended_at=played_at+listened_ms/1000
             WHERE ended_at IS NULL AND listened_ms IS NOT NULL AND id IS NOT ?1",
            [keep],
        )?;
        if closed > 0 {
            bump_writes(&transaction)?;
        }
        transaction.commit()?;
        Ok(closed)
    }

    /// Counted plays, newest first.
    pub fn history_page(&self, offset: usize, limit: usize) -> Result<Page<HistoryEntry>> {
        let total = self.connection.query_row(
            "SELECT COUNT(*) FROM play_history WHERE counted=1",
            [],
            |row| row.get(0),
        )?;
        let mut query = self.connection.prepare(
            "SELECT id,played_at,track_id,title,artist,album,duration_ms,source_kind,source_name
             FROM play_history WHERE counted=1 ORDER BY id DESC LIMIT ?1 OFFSET ?2",
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

    /// How many plays have counted: it moves only when a play counts.
    pub fn history_revision(&self) -> Result<u64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM play_history WHERE counted=1",
            [],
            |row| row.get(0),
        )?)
    }

    /// How many play writes have been made: it moves on every one.
    pub fn stats_revision(&self) -> Result<u64> {
        Ok(self
            .connection
            .query_row("SELECT writes FROM history_control", [], |row| row.get(0))?)
    }
}

/// Counts a play write, for `stats_revision`.
fn bump_writes(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute("UPDATE history_control SET writes=writes+1", [])?;
    Ok(())
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
            raw_title: None,
        }
    }
    /// Opens and counts `play`, as a play that counts at once.
    fn record(store: &mut Store, play: &PlayRecord) -> Result<i64> {
        let row = store.open_play(play)?;
        store.count_play(row)?;
        Ok(row)
    }
    /// The columns of row `row` that a close sets.
    fn timing(database: &Database, row: i64) -> (Option<u64>, Option<String>, Option<i64>) {
        database
            .query(
                &format!("SELECT listened_ms,end_reason,ended_at FROM play_history WHERE id={row}"),
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .remove(0)
    }
    fn titles(page: &Page<HistoryEntry>) -> Vec<&str> {
        page.entries
            .iter()
            .map(|entry| entry.title.as_str())
            .collect()
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
        record(&mut store, &play(1, None, "Loose", 100))?;
        assert!(database.stats().is_empty());
        record(&mut store, &play(2, Some(3), "Indexed", 200))?;
        record(&mut store, &play(3, Some(3), "Indexed", 300))?;
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
            record(&mut store, &play(id, Some(3), title, 1_000 + id as i64))?;
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

    #[test]
    fn an_open_play_is_absent_from_history_and_stats_until_it_counts() -> Result<()> {
        let (database, mut store) = Database::open();
        let before = store.stats_revision()?;
        let row = store.open_play(&play(1, Some(3), "Open", 100))?;
        assert_eq!(store.history_page(0, 10)?.total, 0);
        assert!(database.stats().is_empty());
        assert_eq!(store.history_revision()?, 0);
        assert!(store.stats_revision()? > before);
        store.count_play(row)?;
        assert_eq!(store.history_page(0, 10)?.total, 1);
        assert_eq!(database.stats(), [(3, 1, Some(100))]);
        assert_eq!(store.history_revision()?, 1);
        store.count_play(row)?;
        assert_eq!(database.stats(), [(3, 1, Some(100))]);
        assert_eq!(store.history_revision()?, 1);
        Ok(())
    }

    #[test]
    fn closing_records_listened_time_and_reason() -> Result<()> {
        let (database, mut store) = Database::open();
        let row = store.open_play(&play(1, Some(3), "Heard", 100))?;
        assert_eq!(timing(&database, row), (Some(0), None, None));
        store.checkpoint_play(row, 12_000)?;
        assert_eq!(timing(&database, row), (Some(12_000), None, None));
        store.close_play(row, 31_500, EndReason::Skipped, 140)?;
        assert_eq!(
            timing(&database, row),
            (Some(31_500), Some("skipped".into()), Some(140))
        );
        Ok(())
    }

    #[test]
    fn close_open_plays_except_keeps_one_and_stops_the_rest() -> Result<()> {
        let (database, mut store) = Database::open();
        let mut rows = vec![];
        for (id, listened) in [(1, 10_000), (2, 20_000), (3, 0)] {
            let mut open = play(id, None, "Open", 100 * id as i64);
            open.raw_title = (id == 2).then(|| "Burial - Archangel".into());
            let row = store.open_play(&open)?;
            store.checkpoint_play(row, listened)?;
            rows.push(row);
        }
        assert_eq!(store.close_open_plays_except(Some(rows[1]))?, 2);
        assert_eq!(
            timing(&database, rows[0]),
            (Some(10_000), Some("stopped".into()), Some(110))
        );
        assert_eq!(
            timing(&database, rows[2]),
            (Some(0), Some("stopped".into()), Some(300))
        );
        assert_eq!(timing(&database, rows[1]), (Some(20_000), None, None));
        assert_eq!(
            store
                .open_play_rows()?
                .iter()
                .map(|open| (
                    open.row,
                    open.entry_id,
                    open.counted,
                    open.listened_ms,
                    open.raw_title.clone()
                ))
                .collect::<Vec<_>>(),
            [(rows[1], 2, false, 20_000, Some("Burial - Archangel".into()))]
        );
        Ok(())
    }

    #[test]
    fn pending_history_writes_in_order_and_maps_rows() -> Result<()> {
        let (database, mut store) = Database::open();
        let mut pending = PendingHistory::default();
        pending.push(HistoryEvent::Open {
            play: 1,
            record: play(1, Some(3), "Queued", 100),
        });
        pending.push(HistoryEvent::Count { play: 1 });
        pending.push(HistoryEvent::Close {
            play: 1,
            listened_ms: 200_000,
            reason: EndReason::Finished,
            ended_at: 300,
        });
        let result = pending.write(&mut store);
        assert!(result.wrote && result.counted && result.error.is_none());
        assert!(pending.is_empty());
        assert_eq!(
            timing(&database, 1),
            (Some(200_000), Some("finished".into()), Some(300))
        );
        assert_eq!(database.stats(), [(3, 1, Some(100))]);
        // The close freed the play's row: a later event of it has none.
        pending.push(HistoryEvent::Count { play: 1 });
        let result = pending.write(&mut store);
        assert!(!result.wrote && result.error.is_none() && pending.is_empty());
        Ok(())
    }

    #[test]
    fn pending_history_drops_events_of_a_play_whose_open_was_dropped() {
        let (database, mut store) = Database::open();
        let mut pending = PendingHistory::default();
        assert_eq!(
            pending.push(HistoryEvent::Open {
                play: 9,
                record: play(9, None, "Lost", 100),
            }),
            None
        );
        for _ in 0..MAX_PENDING_PLAYS - 2 {
            pending.push(HistoryEvent::Checkpoint {
                play: 9,
                listened_ms: 1,
            });
        }
        pending.push(HistoryEvent::Checkpoint {
            play: 9,
            listened_ms: 2,
        });
        let dropped = pending.push(HistoryEvent::Count { play: 9 });
        assert!(matches!(dropped, Some(HistoryEvent::Open { play: 9, .. })));
        assert_eq!(pending.len(), MAX_PENDING_PLAYS);
        let result = pending.write(&mut store);
        assert!(!result.wrote && !result.counted && result.error.is_none());
        assert!(pending.is_empty());
        assert!(
            database
                .query("SELECT id FROM play_history", |row| row.get::<_, i64>(0))
                .is_empty()
        );
    }

    #[test]
    fn pending_history_forgets_the_row_of_a_play_whose_close_was_dropped() {
        let mut pending = PendingHistory::default();
        pending.adopt(9, 4);
        pending.push(HistoryEvent::Close {
            play: 9,
            listened_ms: 1,
            reason: EndReason::Finished,
            ended_at: 100,
        });
        for _ in 0..MAX_PENDING_PLAYS - 1 {
            pending.push(HistoryEvent::Count { play: 10 });
        }
        assert!(pending.rows.contains_key(&9));
        let dropped = pending.push(HistoryEvent::Count { play: 10 });
        assert!(matches!(dropped, Some(HistoryEvent::Close { play: 9, .. })));
        // Its row stays open until startup closes it as stopped.
        assert!(!pending.rows.contains_key(&9));
    }

    #[test]
    fn pending_history_stops_at_a_failure_and_keeps_the_rest() {
        let (_database, mut store) = Database::open();
        let mut pending = PendingHistory::default();
        // A play of a track that is not in the library fails its insert.
        pending.push(HistoryEvent::Open {
            play: 1,
            record: play(1, Some(99), "Bad", 100),
        });
        pending.push(HistoryEvent::Count { play: 1 });
        let result = pending.write(&mut store);
        assert!(!result.wrote && result.error.is_some());
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn stats_revision_moves_on_every_write_and_history_revision_only_on_count() -> Result<()> {
        let (_database, mut store) = Database::open();
        let mut last = store.stats_revision()?;
        let mut moved = |store: &Store| -> Result<bool> {
            let now = store.stats_revision()?;
            let moved = now > last;
            last = now;
            Ok(moved)
        };
        let row = store.open_play(&play(1, Some(3), "Rev", 100))?;
        assert!(moved(&store)?);
        store.checkpoint_play(row, 1_000)?;
        assert!(moved(&store)?);
        assert_eq!(store.history_revision()?, 0);
        store.count_play(row)?;
        assert!(moved(&store)?);
        assert_eq!(store.history_revision()?, 1);
        store.close_play(row, 2_000, EndReason::Stopped, 102)?;
        assert!(moved(&store)?);
        assert_eq!(store.history_revision()?, 1);
        Ok(())
    }

    #[test]
    fn end_reasons_round_trip_through_text() {
        for reason in [EndReason::Finished, EndReason::Skipped, EndReason::Stopped] {
            assert_eq!(EndReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(EndReason::parse("other"), None);
    }
}
