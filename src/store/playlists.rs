use super::{Checkpoint, Store};
use crate::{
    filter::{Filter, InvalidFilter},
    library_types::{Page, PageRequest, StaleLibraryRevision},
    protocol::{ErrorCode, MAX_PAGE_SIZE, Playlist, PlaylistEntry, PlaylistKind},
};
use anyhow::{Result, ensure};
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter,
    types::{Type, Value},
};
use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct PlaylistFailure {
    pub code: ErrorCode,
    message: String,
}
impl fmt::Display for PlaylistFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for PlaylistFailure {}
fn invalid(message: impl Into<String>) -> anyhow::Error {
    PlaylistFailure {
        code: ErrorCode::InvalidCommand,
        message: message.into(),
    }
    .into()
}
fn check_revision(expected: Option<u64>, current: u64, code: ErrorCode, kind: &str) -> Result<()> {
    if let Some(expected) = expected.filter(|expected| *expected != current) {
        return Err(PlaylistFailure {
            code,
            message: format!("{kind} revision {expected} is stale; current {current}"),
        }
        .into());
    }
    Ok(())
}
fn check_id(id: u64) -> Result<()> {
    if id == 0 || id > i64::MAX as u64 {
        return Err(invalid("invalid playlist or occurrence ID"));
    }
    Ok(())
}
fn next_revision(revision: u64) -> Result<u64> {
    if revision >= i64::MAX as u64 {
        return Err(invalid("playlist revision exhausted"));
    }
    Ok(revision + 1)
}
fn page_bounds(offset: usize, limit: usize) -> Result<()> {
    if limit == 0 || limit > MAX_PAGE_SIZE || offset > i64::MAX as usize {
        return Err(invalid(
            "playlist page requires limit 1..=500 and a representable offset",
        ));
    }
    Ok(())
}
fn library_revision(connection: &Connection, expected: Option<u64>) -> Result<u64> {
    let revision = connection.query_row(
        "SELECT revision FROM library_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if expected.is_some_and(|expected| expected != revision) {
        return Err(StaleLibraryRevision { current: revision }.into());
    }
    Ok(revision)
}
fn header(connection: &Connection, id: u64, expected: Option<u64>) -> Result<Playlist> {
    check_id(id)?;
    let playlist = connection
        .query_row(
            "SELECT id,name,revision,CASE WHEN query IS NULL THEN
             (SELECT COUNT(*) FROM playlist_entries WHERE playlist_id=p.id) END,query
         FROM playlists p WHERE id=?1",
            [id],
            playlist_row,
        )
        .optional()?
        .ok_or_else(|| invalid("saved playlist not found"))?;
    check_revision(
        expected,
        playlist.revision,
        ErrorCode::StalePlaylistRevision,
        "playlist",
    )?;
    Ok(playlist)
}
/// A list source names playlist `id` at `revision`, smart exactly when `smart`.
/// Returns the smart rule. Unknown, stale and wrong-kind playlists are rejected.
pub(crate) fn list_source_rule(
    connection: &Connection,
    id: u64,
    revision: u64,
    smart: bool,
) -> Result<Option<String>> {
    match (header(connection, id, Some(revision))?.query, smart) {
        (rule @ Some(_), true) | (rule @ None, false) => Ok(rule),
        (Some(_), false) => Err(invalid("playlist source names a smart playlist")),
        (None, true) => Err(invalid("smart source names a regular playlist")),
    }
}
fn playlist_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Playlist> {
    let query: Option<String> = row.get(4)?;
    Ok(Playlist {
        id: row.get(0)?,
        name: row.get(1)?,
        revision: row.get(2)?,
        entry_count: row.get(3)?,
        kind: if query.is_some() {
            PlaylistKind::Smart
        } else {
            PlaylistKind::Regular
        },
        query,
    })
}

fn parse_filter(query: &str, first_parameter: usize) -> Result<Filter> {
    Filter::parse(query, first_parameter).map_err(|error: InvalidFilter| {
        PlaylistFailure {
            code: ErrorCode::InvalidFilter,
            message: error.to_string(),
        }
        .into()
    })
}

const TRACK_ORDER: &str = "t.disc_number,t.track_number,t.title_sort,a.path,t.id";

// One predicate and ordering for live pages, full snapshots and enqueue rechecks.
fn smart_source(
    query: &str,
    transient: &str,
    first_parameter: usize,
) -> Result<(String, Vec<Value>)> {
    let mut rule = parse_filter(query, first_parameter)?;
    let filter = parse_filter(transient, first_parameter + rule.values.len())?;
    let source = format!(
        "FROM library_tracks t CROSS JOIN library_assets a ON a.id=t.asset_id
         JOIN library_roots r ON r.id=a.root_id
         WHERE a.indexed=1 AND a.last_seen_scan>=r.completed_scan AND ({}) AND ({})",
        rule.sql, filter.sql
    );
    rule.values.extend(filter.values);
    Ok((source, rule.values))
}

fn source_query(playlist: &Playlist) -> Result<(String, Vec<Value>)> {
    if let Some(query) = &playlist.query {
        let (source, values) = smart_source(query, "", 1)?;
        Ok((
            format!("SELECT t.id,a.path,1 {source} ORDER BY {TRACK_ORDER}"),
            values,
        ))
    } else {
        Ok((
            "SELECT e.track_id,a.path,a.indexed=1 AND a.last_seen_scan>=r.completed_scan
             FROM playlist_entries e JOIN library_tracks t ON t.id=e.track_id
             JOIN library_assets a ON a.id=t.asset_id JOIN library_roots r ON r.id=a.root_id
             WHERE e.playlist_id=?1 ORDER BY e.ordinal"
                .into(),
            vec![Value::Integer(playlist.id as i64)],
        ))
    }
}

fn entry_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaylistEntry> {
    let metadata: String = row.get(3)?;
    let available: bool = row.get(4)?;
    let error: Option<String> = row.get(5)?;
    Ok(PlaylistEntry {
        id: row.get(0)?,
        track_id: row.get(1)?,
        path: row.get(2)?,
        metadata: serde_json::from_str(&metadata).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(error))
        })?,
        available,
        error: error.or_else(|| (!available).then(|| "library track is unavailable".into())),
    })
}
fn catalog_revision(connection: &Connection) -> Result<u64> {
    Ok(connection.query_row(
        "SELECT revision FROM playlists_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?)
}
fn name_parts(
    connection: &Connection,
    name: &str,
    own_id: Option<u64>,
) -> Result<(String, String)> {
    let name = name.trim();
    if name.is_empty() || name.len() > 256 || name.contains('\0') {
        return Err(invalid(
            "playlist name must be 1..=256 UTF-8 bytes after trimming and contain no NUL",
        ));
    }
    let search = name.to_lowercase();
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM playlists WHERE search=?1 AND (?2 IS NULL OR id<>?2))",
        params![search, own_id],
        |row| row.get(0),
    )?;
    if exists {
        return Err(invalid("playlist name is already in use"));
    }
    Ok((name.to_owned(), search))
}
fn batch(ids: &[u64], distinct: bool) -> Result<()> {
    if ids.is_empty() || ids.len() > MAX_PAGE_SIZE {
        return Err(invalid("playlist batch must contain 1..=500 IDs"));
    }
    for &id in ids {
        check_id(id)?;
    }
    if distinct && ids.iter().copied().collect::<HashSet<_>>().len() != ids.len() {
        return Err(invalid("playlist occurrence IDs must be distinct"));
    }
    Ok(())
}
fn own_entry(connection: &Connection, playlist_id: u64, entry_id: u64) -> Result<i64> {
    check_id(entry_id)?;
    connection
        .query_row(
            "SELECT ordinal FROM playlist_entries WHERE playlist_id=?1 AND id=?2",
            params![playlist_id, entry_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| invalid("occurrence does not belong to this playlist"))
}

// SQL owns the full ordering work; only the bounded selection is hydrated.
fn move_occurrences(
    transaction: &Transaction<'_>,
    playlist: Option<u64>,
    ids: &[u64],
    before: Option<u64>,
) -> Result<bool> {
    batch(ids, true)?;
    let (table, scope) = match playlist {
        Some(id) => ("playlist_entries", format!("playlist_id={id}")),
        None => ("queue", "1".to_owned()),
    };
    let ordinal = |id: u64| -> Result<i64> {
        check_id(id)?;
        transaction
            .query_row(
                &format!("SELECT ordinal FROM {table} WHERE {scope} AND id=?1"),
                [id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| invalid("occurrence does not belong to this source"))
    };
    for &id in ids {
        ordinal(id)?;
    }
    let anchor = before.map(ordinal).transpose()?;
    if before.is_some_and(|id| ids.contains(&id)) {
        return Ok(false);
    }
    transaction.execute_batch(
        "CREATE TEMP TABLE drag_selected(id INTEGER PRIMARY KEY);
         CREATE TEMP TABLE drag_order(id INTEGER PRIMARY KEY, ordinal INTEGER NOT NULL, old_ordinal INTEGER NOT NULL);"
    )?;
    {
        let mut insert = transaction.prepare("INSERT INTO drag_selected VALUES(?1)")?;
        for &id in ids {
            insert.execute([id])?;
        }
    }
    transaction.execute(
        &format!(
            "INSERT INTO drag_order
            SELECT e.id,
              ROW_NUMBER() OVER (ORDER BY CASE
                WHEN s.id IS NOT NULL THEN 1
                WHEN ?1 IS NULL OR e.ordinal<?1 THEN 0 ELSE 2 END,e.ordinal)-1,
              ROW_NUMBER() OVER (ORDER BY e.ordinal)-1
            FROM {table} e LEFT JOIN drag_selected s ON s.id=e.id WHERE {scope}"
        ),
        [anchor],
    )?;
    let changed: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM drag_order WHERE ordinal<>old_ordinal)",
        [],
        |row| row.get(0),
    )?;
    if changed {
        transaction.execute(
            &format!("UPDATE {table} SET ordinal=-ordinal-1 WHERE {scope}"),
            [],
        )?;
        transaction.execute(
            &format!("UPDATE {table} SET ordinal=(SELECT ordinal FROM drag_order WHERE id={table}.id) WHERE {scope}"),
            [],
        )?;
    }
    transaction.execute_batch("DROP TABLE drag_order; DROP TABLE drag_selected;")?;
    Ok(changed)
}

fn insertion_ordinal(
    transaction: &Transaction<'_>,
    playlist: Option<u64>,
    before: Option<u64>,
    count: usize,
) -> Result<i64> {
    let (table, scope) = match playlist {
        Some(id) => ("playlist_entries", format!("playlist_id={id}")),
        None => ("queue", "1".to_owned()),
    };
    let maximum: i64 = transaction.query_row(
        &format!("SELECT COALESCE(MAX(ordinal),-1) FROM {table} WHERE {scope}"),
        [],
        |row| row.get(0),
    )?;
    maximum
        .checked_add(count as i64)
        .ok_or_else(|| invalid("occurrence order exhausted"))?;
    let destination = match before {
        Some(id) => {
            check_id(id)?;
            transaction
                .query_row(
                    &format!("SELECT ordinal FROM {table} WHERE {scope} AND id=?1"),
                    [id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| invalid("destination does not belong to this source"))?
        }
        None => maximum + 1,
    };
    transaction.execute(
        &format!("UPDATE {table} SET ordinal=-ordinal-1 WHERE {scope} AND ordinal>=?1"),
        [destination],
    )?;
    transaction.execute(
        &format!("UPDATE {table} SET ordinal=-ordinal-1+?1 WHERE {scope} AND ordinal<0"),
        [count as i64],
    )?;
    Ok(destination)
}

pub(super) fn migrate(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        "CREATE TABLE playlists_state (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            revision INTEGER NOT NULL CHECK(revision>0)
        );
        INSERT INTO playlists_state VALUES(1,1);
        CREATE TABLE playlists (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            search TEXT NOT NULL UNIQUE,
            revision INTEGER NOT NULL CHECK(revision>0)
        );
        CREATE INDEX playlists_order ON playlists(search,name,id);
        CREATE TABLE playlist_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            track_id INTEGER NOT NULL REFERENCES library_tracks(id),
            ordinal INTEGER NOT NULL,
            UNIQUE(playlist_id,ordinal)
        );",
    )?;
    Ok(())
}

pub(crate) enum PlaylistEdit {
    Create {
        name: String,
    },
    CreateSmart {
        name: String,
        query: String,
    },
    UpdateSmart {
        query: String,
    },
    Rename {
        name: String,
    },
    Delete,
    Add {
        library_revision: u64,
        track_ids: Vec<u64>,
        before: Option<u64>,
    },
    Remove {
        entry_ids: Vec<u64>,
    },
    Move {
        entry_ids: Vec<u64>,
        before: Option<u64>,
    },
}
pub(crate) struct PlaylistChange {
    pub revision: u64,
    pub playlist: Option<Playlist>,
}

impl Store {
    pub fn move_queue(&mut self, expected: u64, ids: &[u64], before: Option<u64>) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision = transaction.query_row(
            "SELECT revision FROM control WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        check_revision(Some(expected), revision, ErrorCode::StaleRevision, "queue")?;
        if move_occurrences(&transaction, None, ids, before)? {
            transaction.execute(
                "UPDATE control SET revision=?1 WHERE singleton=1",
                [next_revision(revision)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_tracks<'a>(
        &mut self,
        expected_queue: u64,
        expected_library: u64,
        entries: impl ExactSizeIterator<Item = (u64, u64, &'a Path)>,
        before: Option<u64>,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        library_revision(&transaction, Some(expected_library))?;
        let (revision, next_id): (u64, u64) = transaction.query_row(
            "SELECT revision,next_id FROM control WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        check_revision(
            Some(expected_queue),
            revision,
            ErrorCode::StaleRevision,
            "queue",
        )?;
        let count = entries.len();
        if count == 0 || count > MAX_PAGE_SIZE {
            return Err(invalid("insert requires 1..=500 tracks"));
        }
        let following_id = next_id
            .checked_add(count as u64)
            .filter(|id| *id <= i64::MAX as u64)
            .ok_or_else(|| invalid("queue ID exhausted"))?;
        let revision = next_revision(revision)?;
        let ordinal = insertion_ordinal(&transaction, None, before, count)?;
        {
            let mut insert = transaction
                .prepare("INSERT INTO queue(id,ordinal,path,track_id) VALUES(?1,?2,?3,?4)")?;
            for (index, (id, track, path)) in entries.enumerate() {
                ensure!(
                    id == next_id + index as u64,
                    "prepared queue identity changed"
                );
                let indexed = crate::library::resolve_track(&transaction, track, expected_library)?;
                if Path::new(&indexed) != path {
                    return Err(invalid("indexed source changed before insertion"));
                }
                insert.execute(params![
                    id,
                    ordinal + index as i64,
                    path.to_str()
                        .ok_or_else(|| invalid("source path must be UTF-8"))?,
                    track
                ])?;
            }
        }
        transaction.execute(
            "UPDATE control SET revision=?1,next_id=?2 WHERE singleton=1",
            params![revision, following_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn playlists_revision(&self) -> Result<u64> {
        catalog_revision(&self.connection)
    }

    pub fn saved_playlists(
        &mut self,
        page: PageRequest,
        kind: Option<PlaylistKind>,
    ) -> Result<Page<Playlist>> {
        page_bounds(page.offset, page.limit)?;
        if page.query.len() > 1024 {
            return Err(invalid("playlist filter exceeds 1024 UTF-8 bytes"));
        }
        let transaction = self.connection.transaction()?;
        let revision = catalog_revision(&transaction)?;
        check_revision(
            page.revision,
            revision,
            ErrorCode::StalePlaylistRevision,
            "playlist catalog",
        )?;
        let search = page.query.to_lowercase();
        let smart = kind.map(|kind| kind == PlaylistKind::Smart);
        let total = transaction.query_row(
            "SELECT COUNT(*) FROM playlists WHERE instr(search,?1)>0 AND (?2 IS NULL OR (query IS NOT NULL)=?2)",
            params![search, smart],
            |row| row.get(0),
        )?;
        let entries = {
            let mut query = transaction.prepare(
                "SELECT id,name,revision,CASE WHEN query IS NULL THEN
                 (SELECT COUNT(*) FROM playlist_entries WHERE playlist_id=p.id) END,query
                 FROM playlists p WHERE instr(search,?1)>0 AND (?2 IS NULL OR (query IS NOT NULL)=?2)
                 ORDER BY search,name,id LIMIT ?3 OFFSET ?4"
            )?;
            query
                .query_map(
                    params![search, smart, page.limit, page.offset],
                    playlist_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.commit()?;
        Ok(Page {
            revision,
            total,
            offset: page.offset,
            entries,
        })
    }

    pub fn playlist_entries(
        &mut self,
        id: u64,
        revision: Option<u64>,
        expected_library: Option<u64>,
        offset: usize,
        limit: usize,
        query: &str,
    ) -> Result<(u64, Page<PlaylistEntry>)> {
        page_bounds(offset, limit)?;
        if offset > 0 && (revision.is_none() || expected_library.is_none()) {
            return Err(invalid(
                "subsequent playlist entry pages require playlist and library revisions",
            ));
        }
        let transaction = self.connection.transaction()?;
        let playlist = header(&transaction, id, revision)?;
        let library_revision = library_revision(&transaction, expected_library)?;
        if let Some(rule) = &playlist.query {
            let (source, mut values) = smart_source(rule, query, 1)?;
            let total = transaction.query_row(
                &format!("SELECT COUNT(*) {source}"),
                params_from_iter(values.iter()),
                |row| row.get(0),
            )?;
            let limit_parameter = values.len() + 1;
            let offset_parameter = limit_parameter + 1;
            values.extend([Value::Integer(limit as i64), Value::Integer(offset as i64)]);
            let entries = {
                let mut statement = transaction.prepare(&format!(
                    "WITH track_page AS MATERIALIZED (
                     SELECT t.id,t.disc_number,t.track_number,t.title_sort,a.path {source}
                     ORDER BY {TRACK_ORDER} LIMIT ?{limit_parameter} OFFSET ?{offset_parameter})
                     SELECT t.id,t.id,p.path,t.metadata,1,a.error
                     FROM track_page p JOIN library_tracks t ON t.id=p.id
                     JOIN library_assets a ON a.id=t.asset_id
                     ORDER BY p.disc_number,p.track_number,p.title_sort,p.path,p.id"
                ))?;
                statement
                    .query_map(params_from_iter(values.iter()), entry_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            transaction.commit()?;
            return Ok((
                library_revision,
                Page {
                    revision: playlist.revision,
                    total,
                    offset,
                    entries,
                },
            ));
        }
        let filter = parse_filter(query, 2)?;
        let total = if filter.values.is_empty() {
            playlist.entry_count.expect("regular playlist count")
        } else {
            let id_value = Value::Integer(id as i64);
            let parameters = std::iter::once(&id_value).chain(filter.values.iter());
            transaction.query_row(
                &format!(
                    "SELECT COUNT(*) FROM playlist_entries e
                    JOIN library_tracks t ON t.id=e.track_id
                    WHERE e.playlist_id=?1 AND ({})",
                    filter.sql
                ),
                params_from_iter(parameters),
                |row| row.get(0),
            )?
        };
        let entries = {
            let limit_parameter = filter.values.len() + 2;
            let offset_parameter = limit_parameter + 1;
            let mut statement = transaction.prepare(&format!(
                "WITH entry_page AS MATERIALIZED (
                 SELECT e.id,e.track_id,e.ordinal FROM playlist_entries e
                 JOIN library_tracks t ON t.id=e.track_id
                 WHERE e.playlist_id=?1 AND ({})
                 ORDER BY e.ordinal LIMIT ?{limit_parameter} OFFSET ?{offset_parameter})
                 SELECT e.id,e.track_id,a.path,t.metadata,
                 a.indexed=1 AND a.last_seen_scan>=r.completed_scan,a.error
                 FROM entry_page e JOIN library_tracks t ON t.id=e.track_id
                 JOIN library_assets a ON a.id=t.asset_id JOIN library_roots r ON r.id=a.root_id
                 ORDER BY e.ordinal",
                filter.sql
            ))?;
            let parameters = std::iter::once(Value::Integer(id as i64))
                .chain(filter.values)
                .chain([Value::Integer(limit as i64), Value::Integer(offset as i64)]);
            statement
                .query_map(params_from_iter(parameters), entry_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.commit()?;
        Ok((
            library_revision,
            Page {
                revision: playlist.revision,
                total,
                offset,
                entries,
            },
        ))
    }

    pub fn edit_playlist(
        &mut self,
        playlist_id: Option<u64>,
        expected: u64,
        edit: PlaylistEdit,
    ) -> Result<PlaylistChange> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let catalog = catalog_revision(&transaction)?;
        let existing = match playlist_id {
            Some(id) => Some(header(&transaction, id, Some(expected))?),
            None => {
                check_revision(
                    Some(expected),
                    catalog,
                    ErrorCode::StalePlaylistRevision,
                    "playlist catalog",
                )?;
                None
            }
        };
        let id = match edit {
            PlaylistEdit::Create { name } => {
                if existing.is_some() {
                    return Err(invalid("create must not specify a playlist"));
                }
                let (name, search) = name_parts(&transaction, &name, None)?;
                transaction.execute(
                    "INSERT INTO playlists(name,search,revision) VALUES(?1,?2,1)",
                    params![name, search],
                )?;
                Some(transaction.last_insert_rowid() as u64)
            }
            PlaylistEdit::CreateSmart { name, query } => {
                if existing.is_some() {
                    return Err(invalid("create must not specify a playlist"));
                }
                parse_filter(&query, 1)?;
                let (name, search) = name_parts(&transaction, &name, None)?;
                transaction.execute(
                    "INSERT INTO playlists(name,search,revision,query) VALUES(?1,?2,1,?3)",
                    params![name, search, query],
                )?;
                Some(transaction.last_insert_rowid() as u64)
            }
            edit => {
                let playlist = existing
                    .as_ref()
                    .ok_or_else(|| invalid("playlist ID is required"))?;
                let id = playlist.id;
                if playlist.kind == PlaylistKind::Smart
                    && matches!(
                        &edit,
                        PlaylistEdit::Add { .. }
                            | PlaylistEdit::Remove { .. }
                            | PlaylistEdit::Move { .. }
                    )
                {
                    return Err(invalid("smart playlist results are read-only"));
                }
                match edit {
                    PlaylistEdit::UpdateSmart { query } => {
                        if playlist.kind != PlaylistKind::Smart {
                            return Err(invalid("playlist is not smart"));
                        }
                        parse_filter(&query, 1)?;
                        transaction.execute(
                            "UPDATE playlists SET query=?1 WHERE id=?2",
                            params![query, id],
                        )?;
                    }
                    PlaylistEdit::Rename { name } => {
                        let (name, search) = name_parts(&transaction, &name, Some(id))?;
                        transaction.execute(
                            "UPDATE playlists SET name=?1,search=?2 WHERE id=?3",
                            params![name, search, id],
                        )?;
                    }
                    PlaylistEdit::Delete => {
                        transaction.execute("DELETE FROM playlists WHERE id=?1", [id])?;
                    }
                    PlaylistEdit::Add {
                        library_revision: expected_library,
                        track_ids,
                        before,
                    } => {
                        batch(&track_ids, false)?;
                        library_revision(&transaction, Some(expected_library))?;
                        let mut available = transaction.prepare(
                            "SELECT EXISTS(SELECT 1 FROM library_tracks t JOIN library_assets a ON a.id=t.asset_id
                             JOIN library_roots r ON r.id=a.root_id WHERE t.id=?1 AND a.indexed=1 AND a.last_seen_scan>=r.completed_scan)"
                        )?;
                        for &track in &track_ids {
                            if !available.query_row([track], |row| row.get::<_, bool>(0))? {
                                return Err(invalid("library track is unknown or unavailable"));
                            }
                        }
                        let ordinal =
                            insertion_ordinal(&transaction, Some(id), before, track_ids.len())?;
                        let mut insert = transaction.prepare("INSERT INTO playlist_entries(playlist_id,track_id,ordinal) VALUES(?1,?2,?3)")?;
                        for (index, track) in track_ids.into_iter().enumerate() {
                            insert.execute(params![id, track, ordinal + index as i64])?;
                        }
                    }
                    PlaylistEdit::Remove { entry_ids } => {
                        batch(&entry_ids, true)?;
                        for &entry in &entry_ids {
                            own_entry(&transaction, id, entry)?;
                        }
                        let mut delete = transaction.prepare(
                            "DELETE FROM playlist_entries WHERE playlist_id=?1 AND id=?2",
                        )?;
                        for entry in entry_ids {
                            delete.execute(params![id, entry])?;
                        }
                    }
                    PlaylistEdit::Move { entry_ids, before } => {
                        if !move_occurrences(&transaction, Some(id), &entry_ids, before)? {
                            return Ok(PlaylistChange {
                                revision: catalog,
                                playlist: existing,
                            });
                        }
                    }
                    PlaylistEdit::Create { .. } | PlaylistEdit::CreateSmart { .. } => {
                        unreachable!()
                    }
                }
                let exists: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM playlists WHERE id=?1)",
                    [id],
                    |row| row.get(0),
                )?;
                if exists {
                    transaction.execute(
                        "UPDATE playlists SET revision=?1 WHERE id=?2",
                        params![next_revision(playlist.revision)?, id],
                    )?;
                    Some(id)
                } else {
                    None
                }
            }
        };
        let revision = next_revision(catalog)?;
        transaction.execute(
            "UPDATE playlists_state SET revision=?1 WHERE singleton=1",
            [revision],
        )?;
        let playlist = id.map(|id| header(&transaction, id, None)).transpose()?;
        transaction.commit()?;
        Ok(PlaylistChange { revision, playlist })
    }

    pub fn snapshot_smart_playlist(
        &mut self,
        id: u64,
        playlist_revision: u64,
        expected_library: u64,
        expected_catalog: u64,
        name: &str,
    ) -> Result<PlaylistChange> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let playlist = header(&transaction, id, Some(playlist_revision))?;
        library_revision(&transaction, Some(expected_library))?;
        let catalog = catalog_revision(&transaction)?;
        check_revision(
            Some(expected_catalog),
            catalog,
            ErrorCode::StalePlaylistRevision,
            "playlist catalog",
        )?;
        let rule = playlist
            .query
            .as_deref()
            .ok_or_else(|| invalid("playlist is not smart"))?;
        let (source, values) = smart_source(rule, "", 2)?;
        let (name, search) = name_parts(&transaction, name, None)?;
        transaction.execute(
            "INSERT INTO playlists(name,search,revision) VALUES(?1,?2,1)",
            params![name, search],
        )?;
        let new_id = transaction.last_insert_rowid() as u64;
        let parameters = std::iter::once(Value::Integer(new_id as i64)).chain(values);
        transaction.execute(
            &format!(
                "INSERT INTO playlist_entries(playlist_id,track_id,ordinal)
                SELECT ?1,t.id,ROW_NUMBER() OVER (ORDER BY {TRACK_ORDER})-1 {source}
                ORDER BY {TRACK_ORDER}"
            ),
            params_from_iter(parameters),
        )?;
        let revision = next_revision(catalog)?;
        transaction.execute(
            "UPDATE playlists_state SET revision=?1 WHERE singleton=1",
            [revision],
        )?;
        let playlist = Some(header(&transaction, new_id, None)?);
        transaction.commit()?;
        Ok(PlaylistChange { revision, playlist })
    }

    /// A narrow source snapshot, never metadata hydration or a UI-sized page.
    pub fn playlist_sources(
        &mut self,
        id: u64,
        revision: u64,
        expected_library: u64,
    ) -> Result<Vec<(u64, PathBuf)>> {
        let transaction = self.connection.transaction()?;
        let playlist = header(&transaction, id, Some(revision))?;
        library_revision(&transaction, Some(expected_library))?;
        let sources = {
            let (sql, values) = source_query(&playlist)?;
            let mut query = transaction.prepare(&sql)?;
            let mut rows = query.query(params_from_iter(values.iter()))?;
            let mut sources = Vec::new();
            if let Some(count) = playlist.entry_count {
                sources.try_reserve(usize::try_from(count)?)?;
            }
            while let Some(row) = rows.next()? {
                if !row.get::<_, bool>(2)? {
                    return Err(invalid("playlist contains an unavailable library track"));
                }
                sources.push((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?)));
            }
            sources
        };
        if sources.is_empty() {
            return Err(invalid("cannot enqueue an empty playlist"));
        }
        transaction.commit()?;
        Ok(sources)
    }

    /// Rechecks every authority under the write lock, then appends and checkpoints
    /// in one commit. The prepared engine batch owns all post-commit resources.
    pub fn enqueue_playlist<'a>(
        &mut self,
        playlist_id: u64,
        playlist_revision: u64,
        expected_library: u64,
        expected_queue: u64,
        entries: impl ExactSizeIterator<Item = (u64, u64, &'a Path)>,
        checkpoint: Option<Checkpoint>,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let playlist = header(&transaction, playlist_id, Some(playlist_revision))?;
        library_revision(&transaction, Some(expected_library))?;
        let (revision, next_id): (u64, u64) = transaction.query_row(
            "SELECT revision,next_id FROM control WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        check_revision(
            Some(expected_queue),
            revision,
            ErrorCode::StaleRevision,
            "queue",
        )?;
        ensure!(
            entries.len() > 0
                && playlist
                    .entry_count
                    .is_none_or(|count| entries.len() as u64 == count),
            "prepared playlist length changed"
        );
        let count = entries.len() as u64;
        let following_id = next_id
            .checked_add(count)
            .filter(|id| *id <= i64::MAX as u64)
            .ok_or_else(|| invalid("queue ID exhausted"))?;
        let revision = revision
            .checked_add(1)
            .filter(|revision| *revision <= i64::MAX as u64)
            .ok_or_else(|| invalid("queue revision exhausted"))?;
        let ordinal: i64 =
            transaction.query_row("SELECT COALESCE(MAX(ordinal),-1) FROM queue", [], |row| {
                row.get(0)
            })?;
        ordinal
            .checked_add(i64::try_from(count)?)
            .ok_or_else(|| invalid("queue order exhausted"))?;
        {
            let (sql, values) = source_query(&playlist)?;
            let mut query = transaction.prepare(&sql)?;
            let mut rows = query.query(params_from_iter(values.iter()))?;
            let mut insert = transaction
                .prepare("INSERT INTO queue(id,ordinal,path,track_id) VALUES(?1,?2,?3,?4)")?;
            for (index, (id, track_id, path)) in entries.enumerate() {
                let row = rows
                    .next()?
                    .ok_or_else(|| invalid("playlist source disappeared"))?;
                ensure!(
                    id == next_id + index as u64,
                    "prepared queue identity changed"
                );
                if row.get::<_, u64>(0)? != track_id
                    || !row.get::<_, bool>(2)?
                    || Path::new(row.get_ref(1)?.as_str()?) != path
                {
                    return Err(invalid("playlist source changed before enqueue"));
                }
                insert.execute(params![
                    id,
                    ordinal + index as i64 + 1,
                    path.to_str()
                        .ok_or_else(|| invalid("source path must be UTF-8"))?,
                    track_id
                ])?;
            }
            if rows.next()?.is_some() {
                return Err(invalid("playlist gained sources before enqueue"));
            }
        }
        transaction.execute(
            "UPDATE control SET revision=?1,next_id=?2 WHERE singleton=1",
            params![revision, following_id],
        )?;
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
}
