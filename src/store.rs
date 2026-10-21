//! SQLite is owned exclusively by the playback command thread. Queue mutations
//! commit before the matching infallible engine change; checkpoints are separate.
use crate::{
    library,
    library_types::format_label,
    play_order::RunInfo,
    protocol::{
        ErrorCode, MAX_RUN_ENTRIES, PLAYED_RETENTION, QueueItem, QueueSource, Settings, SourceKind,
        StreamSpec, TrackSelection,
    },
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

mod history;
mod playlists;
mod selection;
mod stations;
pub(crate) use history::{PendingPlays, PlayRecord, count_threshold_frames};
pub(crate) use playlists::{
    PlaylistChange, PlaylistEdit, PlaylistFailure, list_source_rule, playlist_sources,
};
pub(crate) use selection::resolve_selection;
pub(crate) use stations::{StaleStationsRevision, StationEdit};
#[cfg(test)]
mod playlists_tests;
#[cfg(test)]
mod stations_tests;
const APPLICATION_ID: i64 = 0x7479726e;
const SCHEMA_VERSION: i64 = 7;

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
                ALTER TABLE queue ADD COLUMN played_seq INTEGER;
            -- Schema 4 kept no play order. The entries up to the checkpoint's
            -- current entry were played in queue order; the current one last.
            UPDATE queue SET played_seq=ranked.seq
            FROM (SELECT q.id,ROW_NUMBER() OVER (ORDER BY q.ordinal) AS seq
                  FROM queue q
                  WHERE q.ordinal<=(SELECT c.ordinal FROM control
                                    JOIN queue c ON c.id=control.current_id)) AS ranked
            WHERE queue.id=ranked.id;",
        )?;
        transaction.pragma_update(None, "user_version", 5)?;
        transaction.commit()?;
        version = 5;
    }
    if version == 5 && target > 5 {
        let transaction = connection.transaction()?;
        transaction.execute_batch("ALTER TABLE queue ADD COLUMN stream_title TEXT;")?;
        transaction.pragma_update(None, "user_version", 6)?;
        transaction.commit()?;
        version = 6;
    }
    if version == 6 && target > 6 {
        let transaction = connection.transaction()?;
        stations::migrate(&transaction)?;
        transaction.pragma_update(None, "user_version", 7)?;
        transaction.commit()?;
        version = 7;
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
    pub stream: Option<StreamSpec>,
    pub run: Option<RunInfo>,
    /// Play order: set to the next value each time the entry becomes current.
    pub played_seq: Option<u64>,
}
/// What the library knows about a queue entry's indexed track.
#[derive(Clone)]
pub(crate) struct EntryTrack {
    pub track_id: u64,
    pub album_id: u64,
    pub album_artist_id: u64,
    pub compilation: bool,
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
    AppendStream {
        id: u64,
        stream: &'a StreamSpec,
    },
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
/// One enqueue's queue edit, as the player prepared it.
pub(crate) struct UpcomingEdit<'a> {
    /// Entries removed before the new ones are inserted.
    pub removed: &'a [u64],
    /// The kept entry the new entries go before; `None` puts them last.
    pub before: Option<u64>,
    pub source: &'a QueueSource,
    /// Selects the first new entry in the same commit, and makes it the newest
    /// played entry.
    pub checkpoint: Option<Checkpoint>,
}
/// The played entries to trim, given every played entry newest first: those
/// beyond the newest `PLAYED_RETENTION`. Entries `exempt` names are neither
/// counted nor trimmed (the current entry, entries the same edit removes).
pub(crate) fn played_overflow(newest_first: &[u64], exempt: impl Fn(u64) -> bool) -> Vec<u64> {
    newest_first
        .iter()
        .copied()
        .filter(|id| !exempt(*id))
        .skip(PLAYED_RETENTION)
        .collect()
}
/// Makes queue entry `id` the newest played entry.
fn mark_newest(connection: &Connection, id: u64) -> Result<()> {
    ensure!(
        connection.execute(
            "UPDATE queue SET played_seq=(SELECT COALESCE(MAX(played_seq),0)+1 FROM queue) WHERE id=?1",
            [id],
        )? == 1,
        "persisted queue occurrence not found"
    );
    Ok(())
}
/// The queue revision, after checking it is `expected`.
fn queue_revision(connection: &Connection, expected: u64) -> Result<u64> {
    let revision = connection.query_row(
        "SELECT revision FROM control WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    playlists::check_revision(Some(expected), revision, ErrorCode::StaleRevision, "queue")?;
    Ok(revision)
}
/// File name without extension, for entries the library cannot describe.
fn file_stem(path: &str) -> String {
    Path::new(path).file_stem().map_or_else(
        || path.to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}
/// A queue entry with the display fields of its track; read by `queue_item`.
const QUEUE_ITEM: &str = "SELECT q.id,q.path,q.track_id,q.source_kind,q.source_name,q.run_id,
        q.played_seq,json_extract(t.metadata,'$.title'),json_extract(t.metadata,'$.artist'),
        al.title,t.album_id,json_extract(t.metadata,'$.duration_ms'),q.stream_title
    FROM queue q
    LEFT JOIN library_tracks t ON t.id=q.track_id AND q.stream_title IS NULL
    LEFT JOIN library_albums al ON al.id=t.album_id";
fn queue_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueItem> {
    let path: String = row.get(1)?;
    let title: Option<String> = row.get(7)?;
    let stream = row
        .get::<_, Option<String>>(12)?
        .map(|title| {
            StreamSpec::new(path.clone(), title).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    12,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        error.to_string(),
                    )),
                )
            })
        })
        .transpose()?;
    Ok(QueueItem {
        id: row.get(0)?,
        track_id: if stream.is_some() { None } else { row.get(2)? },
        source: if let Some(stream) = &stream {
            Some(QueueSource::stamped(SourceKind::Stream, &stream.title))
        } else {
            source(row.get(3)?, row.get(4)?)
        },
        run_id: row.get(5)?,
        played: row.get::<_, Option<u64>>(6)?.is_some(),
        title: stream
            .as_ref()
            .map(|stream| stream.title.clone())
            .unwrap_or_else(|| {
                title
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| file_stem(&path))
            }),
        artist: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
        album: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
        album_id: row.get(10)?,
        duration_ms: row.get(11)?,
        path,
        stream,
    })
}
/// A stored source label. A kind this server does not know reads as none.
fn source(kind: Option<String>, name: Option<String>) -> Option<QueueSource> {
    kind.as_deref()
        .and_then(SourceKind::parse)
        .map(|kind| QueueSource {
            kind,
            name: name.unwrap_or_default(),
        })
}
pub(crate) struct Store {
    connection: Connection,
}

/// Legacy graphic EQ is accepted only at the storage boundary, never on the wire.
fn decode_settings(json: &str) -> Result<Settings> {
    let mut value: serde_json::Value = serde_json::from_str(json)?;
    if value
        .get("equalizer")
        .and_then(|eq| eq.get("bands_db"))
        .is_some()
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Graphic {
            enabled: bool,
            preamp_db: f64,
            bands_db: [f64; 10],
        }
        let legacy: Graphic = serde_json::from_value(value["equalizer"].clone())?;
        ensure!(
            legacy.preamp_db.is_finite() && (-24.0..=12.0).contains(&legacy.preamp_db),
            "invalid persisted graphic equalizer preamp"
        );
        ensure!(
            legacy
                .bands_db
                .iter()
                .all(|gain| gain.is_finite() && (-12.0..=12.0).contains(gain)),
            "invalid persisted graphic equalizer gain"
        );
        value["equalizer"] =
            serde_json::to_value(crate::equalizer::EqualizerSettings::from_graphic(
                legacy.enabled,
                legacy.preamp_db,
                legacy.bands_db,
            ))?;
    }
    let mut settings: Settings = serde_json::from_value(value)?;
    settings.equalizer.validate()?;
    ensure!(
        settings.equalizer_presets.len() <= 64,
        "too many persisted equalizer presets"
    );
    settings
        .equalizer_presets
        .sort_by(|a, b| a.name.cmp(&b.name));
    for (index, preset) in settings.equalizer_presets.iter().enumerate() {
        crate::protocol::validate_equalizer_preset_name(&preset.name)?;
        preset.equalizer.validate()?;
        ensure!(
            index == 0 || settings.equalizer_presets[index - 1].name != preset.name,
            "duplicate persisted equalizer preset"
        );
    }
    Ok(settings)
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
        let settings = decode_settings(&settings).context("reading persisted settings")?;
        let mut query = self.connection.prepare(
            "SELECT id,path,ordinal,source_kind,run_id,played_seq,stream_title FROM queue ORDER BY ordinal",
        )?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<u64>>(4)?,
                row.get::<_, Option<u64>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (id, path, ordinal, kind, run_id, played_seq, stream_title) = row?;
            ensure!(
                id > 0 && id < next_id && ordinal >= 0,
                "invalid persisted queue identity/order"
            );
            let stream = stream_title
                .map(|title| StreamSpec::new(path.clone(), title))
                .transpose()
                .context("reading persisted stream descriptor")?;
            let path = PathBuf::from(
                stream
                    .as_ref()
                    .map_or(path.as_str(), |stream| stream.url.as_str()),
            );
            ensure!(
                stream.is_some() || path.is_absolute(),
                "persisted source path is not absolute"
            );
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
                stream,
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
            settings,
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
        let mut query = self.connection.prepare(&format!(
            "{QUEUE_ITEM} ORDER BY q.ordinal LIMIT ?1 OFFSET ?2"
        ))?;
        Ok(query
            .query_map(params![limit, offset], queue_item)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Queue entry `id` as queue pages show it, or `None` when it is gone.
    pub fn queue_entry(&self, id: u64) -> Result<Option<QueueItem>> {
        Ok(self
            .connection
            .query_row(&format!("{QUEUE_ITEM} WHERE q.id=?1"), [id], queue_item)
            .optional()?)
    }

    /// The indexed track behind queue entry `entry_id`, or `None` when it has
    /// no track or the track is gone.
    pub fn entry_track(&self, entry_id: u64) -> Result<Option<EntryTrack>> {
        Ok(self
            .connection
            .query_row(
                "SELECT t.id,t.album_id,al.artist_id,al.compilation,t.codec,t.bit_depth,
                        COALESCE(json_extract(t.metadata,'$.sample_rate'),0),t.bitrate_kbps
                 FROM queue q JOIN library_tracks t ON t.id=q.track_id
                 JOIN library_albums al ON al.id=t.album_id WHERE q.id=?1",
                [entry_id],
                |row| {
                    let codec: Option<String> = row.get(4)?;
                    Ok(EntryTrack {
                        track_id: row.get(0)?,
                        album_id: row.get(1)?,
                        album_artist_id: row.get(2)?,
                        compilation: row.get(3)?,
                        format: format_label(
                            codec.as_deref(),
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                        ),
                    })
                },
            )
            .optional()?)
    }

    /// Rechecks the queue revision and every revision `selection` was resolved
    /// under, then removes, inserts and checkpoints in one commit. The new
    /// entries share one run, whose ID is the first new entry's.
    pub fn replace_upcoming<'a>(
        &mut self,
        expected_queue: u64,
        selection: &TrackSelection,
        edit: UpcomingEdit<'_>,
        entries: impl ExactSizeIterator<Item = (u64, u64, &'a Path)>,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        selection::check_revisions(&transaction, selection)?;
        let (revision, next_id): (u64, u64) = transaction.query_row(
            "SELECT revision,next_id FROM control WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        playlists::check_revision(
            Some(expected_queue),
            revision,
            ErrorCode::StaleRevision,
            "queue",
        )?;
        let count = entries.len();
        if count == 0 || count > MAX_RUN_ENTRIES {
            return Err(playlists::invalid(format!(
                "enqueue requires 1..={MAX_RUN_ENTRIES} tracks"
            )));
        }
        let following_id = next_id
            .checked_add(count as u64)
            .filter(|id| *id <= i64::MAX as u64)
            .ok_or_else(|| playlists::invalid("queue ID exhausted"))?;
        let revision = playlists::next_revision(revision)?;
        {
            let mut delete = transaction.prepare("DELETE FROM queue WHERE id=?1")?;
            for &id in edit.removed {
                ensure!(
                    delete.execute([id])? == 1,
                    "persisted queue occurrence not found"
                );
            }
        }
        let ordinal = playlists::insertion_ordinal(&transaction, None, edit.before, count)?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO queue(id,ordinal,path,track_id,source_kind,source_name,run_id)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
            )?;
            for (index, (id, track, path)) in entries.enumerate() {
                ensure!(
                    id == next_id + index as u64,
                    "prepared queue identity changed"
                );
                insert.execute(params![
                    id,
                    ordinal + index as i64,
                    path.to_str()
                        .ok_or_else(|| playlists::invalid("source path must be UTF-8"))?,
                    track,
                    edit.source.kind.as_str(),
                    &edit.source.name,
                    next_id
                ])?;
            }
        }
        transaction.execute(
            "UPDATE control SET revision=?1,next_id=?2 WHERE singleton=1",
            params![revision, following_id],
        )?;
        if let Some(checkpoint) = edit.checkpoint {
            if let Some(current) = checkpoint.current_id {
                mark_newest(&transaction, current)?;
            }
            transaction.execute(
                "UPDATE control SET current_id=?1,position_frames=?2,playing=?3 WHERE singleton=1",
                params![
                    checkpoint.current_id,
                    checkpoint.position_frames,
                    checkpoint.playing
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Every played queue entry, newest first.
    pub fn played_newest_first(&self) -> Result<Vec<u64>> {
        let mut query = self.connection.prepare(
            "SELECT id FROM queue WHERE played_seq IS NOT NULL ORDER BY played_seq DESC",
        )?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Makes `current` the newest played entry and deletes the played entries
    /// `trimmed` in one commit, with `checkpoint` when given. `cleared` entries
    /// become unplayed first: Repeat All starts a fresh pass over them. Trimming
    /// bumps the queue revision; marking and clearing do not.
    pub fn mark_current(
        &mut self,
        expected: u64,
        current: u64,
        trimmed: &[u64],
        cleared: &[u64],
        checkpoint: Option<Checkpoint>,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revision = queue_revision(&transaction, expected)?;
        {
            let mut delete = transaction
                .prepare("DELETE FROM queue WHERE id=?1 AND id<>?2 AND played_seq IS NOT NULL")?;
            for &id in trimmed {
                ensure!(
                    delete.execute(params![id, current])? == 1,
                    "trimmed queue occurrence is not a played entry"
                );
            }
            // A cleared entry may have been removed since the pass was planned.
            let mut clear = transaction.prepare("UPDATE queue SET played_seq=NULL WHERE id=?1")?;
            for &id in cleared {
                clear.execute([id])?;
            }
        }
        mark_newest(&transaction, current)?;
        if !trimmed.is_empty() {
            transaction.execute(
                "UPDATE control SET revision=?1 WHERE singleton=1",
                [playlists::next_revision(revision)?],
            )?;
        }
        if let Some(checkpoint) = checkpoint {
            transaction.execute(
                "UPDATE control SET current_id=?1,position_frames=?2,playing=?3 WHERE singleton=1",
                params![
                    checkpoint.current_id,
                    checkpoint.position_frames,
                    checkpoint.playing
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Deletes the queue entries `ids` in one commit and bumps the revision.
    pub fn remove_entries(&mut self, expected: u64, ids: &[u64]) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revision = queue_revision(&transaction, expected)?;
        {
            let mut delete = transaction.prepare("DELETE FROM queue WHERE id=?1")?;
            for &id in ids {
                ensure!(
                    delete.execute([id])? == 1,
                    "persisted queue occurrence not found"
                );
            }
        }
        transaction.execute(
            "UPDATE control SET revision=?1 WHERE singleton=1",
            [playlists::next_revision(revision)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Puts the queue entries `ids` in that order, in the positions they already
    /// occupy, in one commit that bumps the revision.
    pub fn reorder_queue(&mut self, expected: u64, ids: &[u64]) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revision = queue_revision(&transaction, expected)?;
        let mut ordinals = Vec::with_capacity(ids.len());
        {
            let mut ordinal = transaction.prepare("SELECT ordinal FROM queue WHERE id=?1")?;
            let mut park =
                transaction.prepare("UPDATE queue SET ordinal=-ordinal-1 WHERE id=?1")?;
            for &id in ids {
                let value: i64 = ordinal
                    .query_row([id], |row| row.get(0))
                    .optional()?
                    .ok_or_else(|| playlists::invalid("queue occurrence not found"))?;
                // A parked (negative) ordinal means the ID was named twice.
                ensure!(value >= 0, "reordered occurrences must be distinct");
                ordinals.push(value);
                park.execute([id])?;
            }
        }
        ordinals.sort_unstable();
        {
            let mut place = transaction.prepare("UPDATE queue SET ordinal=?2 WHERE id=?1")?;
            for (&id, ordinal) in ids.iter().zip(ordinals) {
                place.execute(params![id, ordinal])?;
            }
        }
        transaction.execute(
            "UPDATE control SET revision=?1 WHERE singleton=1",
            [playlists::next_revision(revision)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn edit(&mut self, edit: Edit<'_>, revision: u64, next_id: u64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match edit {
            Edit::AppendStream { id, stream } => {
                let validated = StreamSpec::new(stream.url.clone(), stream.title.clone())?;
                ensure!(&validated == stream, "stream descriptor is not canonical");
                transaction.execute(
                    "INSERT INTO queue(id,ordinal,path,track_id,source_kind,source_name,run_id,stream_title)
                     SELECT ?1,COALESCE(MAX(ordinal),-1)+1,?2,NULL,'stream',?3,?1,?3 FROM queue",
                    params![id, &stream.url, &stream.title],
                )?;
            }
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
    fn graphic_equalizer_migrates_only_at_storage_boundary() -> Result<()> {
        let gains = [6.0, 4.0, 2.0, 0.0, -1.0, -2.0, 0.0, 1.0, 2.0, 12.0];
        let mut value = serde_json::to_value(Settings::default())?;
        value.as_object_mut().unwrap().remove("equalizer_presets");
        value["equalizer"] =
            serde_json::json!({"enabled": true, "preamp_db": -6.0, "bands_db": gains});
        assert!(serde_json::from_value::<Settings>(value.clone()).is_err());
        let migrated = decode_settings(&value.to_string())?;
        assert_eq!(
            migrated.equalizer,
            crate::equalizer::EqualizerSettings::from_graphic(true, -6.0, gains)
        );
        assert!(migrated.equalizer_presets.is_empty());
        let encoded = serde_json::to_value(&migrated)?;
        assert!(encoded["equalizer"].get("bands_db").is_none());
        assert_eq!(encoded["equalizer"]["bands"].as_array().unwrap().len(), 10);
        assert_eq!(decode_settings(&encoded.to_string())?, migrated);
        value["equalizer"]["bands"] = serde_json::json!([]);
        assert!(decode_settings(&value.to_string()).is_err());
        Ok(())
    }

    #[test]
    fn invalid_parametric_settings_and_presets_are_not_silently_reset() -> Result<()> {
        let mut settings = Settings {
            equalizer: crate::equalizer::EqualizerSettings::from_graphic(true, -6.0, [3.0; 10]),
            ..Settings::default()
        };
        let valid = serde_json::to_value(&settings)?;
        for (field, bad) in [
            ("frequency_hz", serde_json::json!(20001)),
            ("gain_db", serde_json::json!(24.01)),
            ("q", serde_json::json!(0.09)),
            ("shelf_slope", serde_json::json!(1.01)),
            ("slope_db", serde_json::json!(18)),
            ("kind", serde_json::json!("graphic")),
        ] {
            let mut value = valid.clone();
            value["equalizer"]["bands"][0][field] = bad;
            assert!(
                decode_settings(&value.to_string()).is_err(),
                "accepted {field}"
            );
        }
        let preset = crate::protocol::EqualizerUserPreset {
            name: "Saved".into(),
            equalizer: settings.equalizer.clone(),
        };
        settings.equalizer_presets = vec![preset.clone(), preset.clone()];
        assert!(decode_settings(&serde_json::to_string(&settings)?).is_err());
        settings.equalizer_presets = vec![preset; 65];
        assert!(decode_settings(&serde_json::to_string(&settings)?).is_err());
        settings.equalizer_presets.truncate(1);
        settings.equalizer_presets[0].name = " invalid ".into();
        assert!(decode_settings(&serde_json::to_string(&settings)?).is_err());
        settings.equalizer_presets[0].name = "Valid".into();
        settings.equalizer_presets[0].equalizer.preamp_db = -96.01;
        assert!(decode_settings(&serde_json::to_string(&settings)?).is_err());
        Ok(())
    }

    #[test]
    fn equalizer_defaults_for_existing_database_and_round_trips() -> Result<()> {
        let path = temp_path();
        let result = (|| -> Result<()> {
            let store = Store::open(&path)?;
            // A current-schema database written before EQ needs no schema migration.
            store.connection.execute(
                "UPDATE control SET settings=?1 WHERE singleton=1",
                [r#"{"gain_mode":"off","resume_on_start":true,"repeat":"off","shuffle":false,"volume":37}"#],
            )?;
            let loaded = store.load()?;
            assert_eq!(
                loaded.settings.equalizer,
                crate::equalizer::EqualizerSettings::default()
            );
            assert_eq!(loaded.settings.volume, 37);
            assert!(loaded.settings.resume_on_start);
            let mut settings = loaded.settings;
            settings.equalizer = crate::equalizer::EqualizerSettings::from_graphic(
                true,
                -6.0,
                [6.0, 4.0, 2.0, 0.0, -1.0, -2.0, 0.0, 1.0, 2.0, 3.0],
            );
            store.checkpoint(loaded.checkpoint, &settings)?;
            drop(store);
            let store = Store::open(&path)?;
            assert_eq!(store.load()?.settings, settings);
            assert_eq!(user_version(&path), 7);
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn invalid_persisted_equalizer_is_rejected() -> Result<()> {
        let path = temp_path();
        let result = (|| -> Result<()> {
            let store = Store::open(&path)?;
            let mut valid = serde_json::to_value(Settings::default())?;
            valid["equalizer"] =
                serde_json::json!({"enabled": true, "preamp_db": 0.0, "bands_db": vec![0.0; 10]});
            let mut invalid = Vec::new();
            for preamp in [-24.01, 12.01] {
                let mut settings = valid.clone();
                settings["equalizer"]["preamp_db"] = serde_json::json!(preamp);
                invalid.push(settings);
            }
            for band in [-12.01, 12.01] {
                let mut settings = valid.clone();
                settings["equalizer"]["bands_db"][9] = serde_json::json!(band);
                invalid.push(settings);
            }
            for bands in [serde_json::to_value([0.0; 9])?, serde_json::Value::Null] {
                let mut settings = valid.clone();
                settings["equalizer"]["bands_db"] = bands;
                invalid.push(settings);
            }
            for settings in invalid {
                store.connection.execute(
                    "UPDATE control SET settings=?1 WHERE singleton=1",
                    [settings.to_string()],
                )?;
                assert!(store.load().is_err(), "accepted {settings}");
            }
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn schema_six_preserves_files_and_round_trips_streams() -> Result<()> {
        let path = temp_path();
        let result = (|| -> Result<()> {
            let mut connection = Connection::open(&path)?;
            migrate(&mut connection, 5)?;
            connection.execute_batch(
                "INSERT INTO queue(id,ordinal,path) VALUES(1,0,'/music/a.flac');
                 UPDATE control SET next_id=2,revision=2,current_id=1,position_frames=48000;",
            )?;
            drop(connection);
            let mut store = Store::open(&path)?;
            let local = store.load()?;
            assert!(local.entries[0].stream.is_none());
            assert_eq!(local.checkpoint.position_frames, 48_000);
            assert_eq!(user_version(&path), 7);
            let stream =
                StreamSpec::new("HTTPS://RADIO.EXAMPLE:443/live".into(), "Station".into())?;
            store.edit(
                Edit::AppendStream {
                    id: 2,
                    stream: &stream,
                },
                3,
                3,
            )?;
            drop(store);
            let store = Store::open(&path)?;
            let loaded = store.load()?;
            assert_eq!((loaded.revision, loaded.next_id), (3, 3));
            assert_eq!(loaded.entries[0].path, PathBuf::from("/music/a.flac"));
            assert!(loaded.entries[0].stream.is_none());
            assert_eq!(loaded.entries[1].stream.as_ref(), Some(&stream));
            assert_eq!(loaded.entries[1].path, PathBuf::from(&stream.url));
            let rows = store.queue_page(0, 10)?;
            assert!(rows[0].stream.is_none());
            assert_eq!(rows[1].stream.as_ref(), Some(&stream));
            assert_eq!(rows[1].title, "Station");
            assert_eq!(rows[1].source.as_ref().unwrap().kind, SourceKind::Stream);
            assert_eq!(rows[1].run_id, Some(2));
            assert!(rows[1].track_id.is_none());
            assert!(rows[1].album_id.is_none());
            assert!(rows[1].duration_ms.is_none());
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn invalid_stream_descriptors_never_commit_and_corrupt_restores_fail() -> Result<()> {
        let path = temp_path();
        let result = (|| -> Result<()> {
            let mut store = Store::open(&path)?;
            let invalid = StreamSpec {
                url: "file:///music/a.mp3".into(),
                title: "Not live".into(),
            };
            assert!(
                store
                    .edit(
                        Edit::AppendStream {
                            id: 1,
                            stream: &invalid
                        },
                        2,
                        2
                    )
                    .is_err()
            );
            let loaded = store.load()?;
            assert_eq!((loaded.revision, loaded.next_id), (1, 1));
            assert!(loaded.entries.is_empty());
            store.connection.execute_batch(
                "INSERT INTO queue(id,ordinal,path,stream_title) VALUES(1,0,'file:///music/a.mp3','Invalid');
                 UPDATE control SET next_id=2;"
            )?;
            assert!(store.load().is_err());
            assert!(store.queue_page(0, 10).is_err());
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }

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
    fn mark_current_renumbers_and_trims_the_oldest_played() -> Result<()> {
        let newest_first: Vec<u64> = (1..=103).rev().collect();
        assert_eq!(played_overflow(&newest_first, |id| id == 103), [2, 1]);
        assert!(played_overflow(&newest_first[..101], |id| id == 103).is_empty());
        let path = temp_path();
        let result = (|| -> Result<()> {
            let mut store = Store::open(&path)?;
            for id in 1..=4 {
                store.edit(
                    Edit::Append {
                        id,
                        path: &format!("/music/{id}.flac"),
                        track: None,
                        source: None,
                        run_id: None,
                    },
                    id + 1,
                    id + 1,
                )?;
            }
            for id in [1, 2, 3] {
                store.mark_current(5, id, &[], &[], None)?;
            }
            assert_eq!(store.played_newest_first()?, [3, 2, 1]);
            // Becoming current again makes an entry the newest.
            store.mark_current(5, 1, &[], &[], None)?;
            assert_eq!(store.played_newest_first()?, [1, 3, 2]);
            // Marking alone keeps the revision.
            assert_eq!(store.load()?.revision, 5);
            // Stale revisions, the current entry and unplayed entries are refused,
            // and change nothing.
            assert!(store.mark_current(4, 4, &[], &[], None).is_err());
            assert!(store.mark_current(5, 3, &[3], &[], None).is_err());
            assert!(store.mark_current(5, 3, &[4], &[], None).is_err());
            assert_eq!(store.played_newest_first()?, [1, 3, 2]);
            let checkpoint = Checkpoint {
                current_id: Some(4),
                position_frames: 0,
                playing: true,
            };
            store.mark_current(5, 4, &[2], &[], Some(checkpoint))?;
            let loaded = store.load()?;
            assert_eq!(
                loaded
                    .entries
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>(),
                [1, 3, 4]
            );
            assert_eq!(loaded.revision, 6);
            assert_eq!(loaded.checkpoint.current_id, Some(4));
            assert_eq!(store.played_newest_first()?, [4, 1, 3]);
            // A fresh pass clears its entries, removed ones included, then marks.
            store.mark_current(6, 3, &[], &[1, 3, 4, 2], None)?;
            assert_eq!(store.played_newest_first()?, [3]);
            assert_eq!(store.load()?.revision, 6);
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
            assert_eq!(user_version(&path), 7);
            assert_eq!(loaded.entries.len(), 1);
            assert_eq!(loaded.checkpoint.current_id, Some(1));
            assert_eq!(pending_upgrade(&path), 1);
            assert_eq!(column_default(&path, "library_tracks", "date_added"), 0);
            assert!(table_exists(&path, "play_history") && table_exists(&path, "track_stats"));
            let connection = Connection::open(&path)?;
            let counts: (i64, i64, i64) = connection.query_row(
                "SELECT (SELECT COUNT(*) FROM playlist_entries),
                        (SELECT COUNT(*) FROM playlists WHERE query IS NOT NULL),
                        (SELECT COUNT(*) FROM queue WHERE source_kind IS NULL AND run_id IS NULL AND played_seq=1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(counts, (1, 1, 1));
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result?;
        // The entries up to the checkpoint's current entry were played, in
        // queue order: they get played ranks, the current entry the highest.
        // Without a current entry nothing was played.
        for (current, played) in [
            (
                Some(3),
                vec![(1, Some(1)), (2, Some(2)), (3, Some(3)), (4, None)],
            ),
            (None, vec![(1, None), (2, None), (3, None), (4, None)]),
        ] {
            let path = schema_4_file(true);
            let result = (|| -> Result<()> {
                let connection = Connection::open(&path)?;
                // Ordinals need not be contiguous.
                connection.execute_batch(
                    "INSERT INTO queue(id,ordinal,path) VALUES(2,5,'/music/b.flac'),
                        (3,9,'/music/c.flac'),(4,12,'/music/d.flac');
                    UPDATE control SET next_id=5;",
                )?;
                connection.execute("UPDATE control SET current_id=?1", [current])?;
                drop(connection);
                Store::open(&path)?;
                let connection = Connection::open(&path)?;
                let mut query =
                    connection.prepare("SELECT id,played_seq FROM queue ORDER BY ordinal")?;
                let rows = query
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<(u64, Option<u64>)>>>()?;
                assert_eq!(rows, played, "current {current:?}");
                Ok(())
            })();
            let _ = fs::remove_file(&path);
            result?;
        }
        Ok(())
    }
    #[test]
    fn empty_schema_4_database_migrates_without_pending_upgrade() -> Result<()> {
        let path = schema_4_file(false);
        let result = (|| -> Result<()> {
            Store::open(&path)?;
            assert_eq!(user_version(&path), 7);
            assert_eq!(pending_upgrade(&path), 0);
            Ok(())
        })();
        let _ = fs::remove_file(&path);
        result
    }
}
