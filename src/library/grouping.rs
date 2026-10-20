//! Reconcile only release groups touched by the current root scan. Temporary SQL
//! worklists bound coordinator memory and let large releases update in batches;
//! interrupted scans are seeded again on the next manual rescan.
use super::db;
use crate::library_types::TrackMetadata;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
enum Group {
    Release(String),
    Folder { directory: String, album: String },
}

impl Group {
    fn new(path: &str, album: &str, release: Option<&str>) -> Result<Self> {
        Ok(match release.filter(|id| !id.trim().is_empty()) {
            Some(id) => Self::Release(id.into()),
            None => Self::Folder {
                directory: Path::new(path)
                    .parent()
                    .and_then(Path::to_str)
                    .context("track has no UTF-8 source directory")?
                    .into(),
                album: album.into(),
            },
        })
    }

    fn remember(&self, connection: &Connection) -> Result<String> {
        let key = serde_json::to_string(self)?;
        connection.execute(
            "INSERT OR IGNORE INTO temp.library_album_groups(key) VALUES(?1)",
            [&key],
        )?;
        Ok(key)
    }
}

fn remember_sources(
    connection: &Connection,
    path: &str,
    album: &str,
    release: Option<&str>,
) -> Result<()> {
    // Folder keys sort before release keys, so all unambiguous companions are
    // known before any release is analyzed. A release is still processed once.
    let key = Group::new(path, album, None)?.remember(connection)?;
    connection.execute(
        "INSERT INTO temp.library_album_sources(path,key) VALUES(?1,?2)
        ON CONFLICT(path) DO UPDATE SET key=excluded.key",
        params![path, key],
    )?;
    if release.is_some_and(|id| !id.trim().is_empty()) {
        Group::new(path, album, release)?.remember(connection)?;
    }
    Ok(())
}

pub(super) fn initialize(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TEMP TABLE library_album_groups(key TEXT PRIMARY KEY);
        CREATE TEMP TABLE library_album_seed(
            asset_id INTEGER PRIMARY KEY, path TEXT NOT NULL, album TEXT NOT NULL, release_id TEXT
        );
        CREATE TEMP TABLE library_album_sources(path TEXT PRIMARY KEY,key TEXT NOT NULL);
        CREATE INDEX temp.library_album_sources_group ON library_album_sources(key,path);
        CREATE TEMP TABLE library_album_members(track_id INTEGER PRIMARY KEY);
        CREATE TEMP TABLE library_album_companions(
            track_id INTEGER PRIMARY KEY, release_id TEXT NOT NULL
        );
        CREATE INDEX temp.library_album_companions_release
            ON library_album_companions(release_id,track_id);",
    )?;
    Ok(())
}

/// Snapshot old group keys before retags overwrite metadata. This visits only
/// the scanned root, outside a main-database writer transaction. Tombstoned rows
/// are included so returning unchanged files also reconcile correctly.
pub(super) fn begin(connection: &Connection, root_id: u64) -> Result<()> {
    connection.execute_batch(
        "DELETE FROM temp.library_album_groups;
        DELETE FROM temp.library_album_seed;
        DELETE FROM temp.library_album_sources;
        DELETE FROM temp.library_album_members;
        DELETE FROM temp.library_album_companions;",
    )?;
    connection.execute(
        "INSERT INTO temp.library_album_seed(asset_id,path,album,release_id)
        SELECT a.id,a.path,json_extract(t.metadata,'$.album'),
            json_extract(t.metadata,'$.musicbrainz_release_id')
        FROM library_assets a JOIN library_tracks t ON t.asset_id=a.id WHERE a.root_id=?1",
        [root_id],
    )?;
    Ok(())
}

pub(super) fn seed_step(connection: &mut Connection) -> Result<bool> {
    let rows = connection
        .prepare("SELECT asset_id,path,album,release_id FROM temp.library_album_seed ORDER BY asset_id LIMIT ?1")?
        .query_map([db::BATCH as u64], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(true);
    }
    let transaction = connection.transaction()?;
    for (id, path, album, release) in rows {
        remember_sources(&transaction, &path, &album, release.as_deref())?;
        transaction.execute(
            "DELETE FROM temp.library_album_seed WHERE asset_id=?1",
            [id],
        )?;
    }
    transaction.commit()?;
    Ok(false)
}

pub(super) fn remember(
    transaction: &Transaction<'_>,
    path: &str,
    metadata: &TrackMetadata,
) -> Result<()> {
    remember_sources(
        transaction,
        path,
        &metadata.album,
        metadata.musicbrainz_release_id.as_deref(),
    )
}

fn various_artists(name: &str) -> bool {
    let name = name.trim();
    name.eq_ignore_ascii_case("Various Artists") || name.eq_ignore_ascii_case("VA")
}

fn restore_explicit_metadata(metadata: &mut TrackMetadata, infer: bool) -> bool {
    let tagged_artist = metadata.album_artist_tag.as_deref();
    let compilation = metadata
        .compilation_tag
        .unwrap_or_else(|| tagged_artist.is_some_and(various_artists) || infer);
    let album_artist = match tagged_artist {
        Some(name) if various_artists(name) => "Various Artists",
        Some(name) => name,
        None if compilation => "Various Artists",
        None => &metadata.artist,
    };
    let changed = metadata.album_artist != album_artist || metadata.compilation != compilation;
    if metadata.album_artist != album_artist {
        metadata.album_artist.clear();
        metadata.album_artist.push_str(album_artist);
    }
    metadata.compilation = compilation;
    changed
}

#[derive(Default)]
struct GroupState {
    key: String,
    folder: bool,
    release_id: Option<String>,
    conflicting_releases: bool,
    first_artist: Option<String>,
    diverse: bool,
    protected: bool,
    analyzed: u64,
    ready: bool,
}

#[derive(Default)]
pub(super) struct Reconciliation {
    seeded: bool,
    current: Option<GroupState>,
}

impl Reconciliation {
    pub(super) fn seed(&mut self, connection: &mut Connection) -> Result<bool> {
        if !self.seeded {
            self.seeded = seed_step(connection)?;
        }
        Ok(self.seeded)
    }

    /// Read each group once, then commit at most BATCH track updates per tick.
    /// Main-database writer reservations never cover group discovery or analysis.
    pub(super) fn step(
        &mut self,
        connection: &mut Connection,
        root_id: u64,
        serial: u64,
        complete: bool,
    ) -> Result<bool> {
        if self.current.is_none() {
            let key: Option<String> = connection
                .query_row(
                    "SELECT key FROM temp.library_album_groups ORDER BY key LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(key) = key else {
                return Ok(true);
            };
            let group: Group = serde_json::from_str(&key)?;
            let mut current = GroupState {
                key,
                ..GroupState::default()
            };
            match group {
                Group::Release(id) => {
                    // Both legacy ["mb",id] and current ["mb",id,artist,flag]
                    // identities share this indexed prefix. No library-wide scan.
                    let mut prefix = serde_json::to_string(&("mb", &id))?;
                    prefix.pop();
                    connection.execute(
                        "INSERT INTO temp.library_album_members(track_id)
                        SELECT t.id FROM library_albums al
                        JOIN library_tracks t ON t.album_id=al.id
                        JOIN library_assets a ON a.id=t.asset_id
                        JOIN library_roots r ON r.id=a.root_id
                        WHERE al.identity>=?1 AND al.identity<?2 AND a.indexed=1
                        AND a.last_seen_scan>=CASE WHEN a.root_id=?3 AND ?5 THEN ?4 ELSE r.completed_scan END
                        AND NOT EXISTS(SELECT 1 FROM temp.library_album_companions c
                            WHERE c.track_id=t.id AND c.release_id!=?6)",
                        params![prefix, format!("{prefix}~"), root_id, serial, complete, id],
                    )?;
                    connection.execute(
                        "INSERT OR IGNORE INTO temp.library_album_members(track_id)
                        SELECT track_id FROM temp.library_album_companions WHERE release_id=?1",
                        [&id],
                    )?;
                    current.release_id = Some(id);
                }
                Group::Folder { .. } => {
                    current.folder = true;
                    connection.execute(
                        "INSERT INTO temp.library_album_members(track_id)
                        SELECT t.id FROM temp.library_album_sources s
                        JOIN library_assets a ON a.path=s.path
                        JOIN library_tracks t ON t.asset_id=a.id
                        JOIN library_roots r ON r.id=a.root_id
                        WHERE s.key=?1 AND a.indexed=1
                        AND a.last_seen_scan>=CASE WHEN a.root_id=?2 AND ?4 THEN ?3 ELSE r.completed_scan END",
                        params![current.key, root_id, serial, complete],
                    )?;
                }
            }
            self.current = Some(current);
            return Ok(false);
        }

        let current = self
            .current
            .as_mut()
            .context("missing reconciliation group")?;
        if !current.ready {
            let mut statement = connection.prepare(
                "SELECT t.id,t.metadata,a.metadata_version FROM temp.library_album_members m
                JOIN library_tracks t ON t.id=m.track_id
                JOIN library_assets a ON a.id=t.asset_id
                WHERE m.track_id>?1 ORDER BY m.track_id LIMIT ?2",
            )?;
            let mut rows = statement.query(params![current.analyzed, db::BATCH as u64])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                count += 1;
                current.analyzed = row.get(0)?;
                let metadata: TrackMetadata = serde_json::from_str(&row.get::<_, String>(1)?)?;
                current.protected |= row.get::<_, i64>(2)? != db::METADATA_VERSION
                    || metadata.compilation_tag == Some(false)
                    || metadata
                        .album_artist_tag
                        .as_deref()
                        .is_some_and(|name| !various_artists(name))
                    || metadata.album.trim().is_empty()
                    || metadata.album.eq_ignore_ascii_case("Unknown Album");
                if current.protected && !current.folder {
                    current.ready = true;
                    break;
                }
                let artist = metadata.artist.trim();
                if !current.diverse
                    && !artist.is_empty()
                    && !artist.eq_ignore_ascii_case("Unknown Artist")
                {
                    if let Some(first) = &current.first_artist {
                        current.diverse = *first != artist.to_lowercase();
                    } else {
                        current.first_artist = Some(artist.to_lowercase());
                    }
                }
                if current.folder
                    && let Some(id) = metadata
                        .musicbrainz_release_id
                        .filter(|id| !id.trim().is_empty())
                {
                    if let Some(first) = &current.release_id {
                        current.conflicting_releases |= *first != id;
                    } else {
                        current.release_id = Some(id);
                    }
                }
            }
            current.ready |= count < db::BATCH;
            return Ok(false);
        }
        if current.folder && current.release_id.is_some() && !current.conflicting_releases {
            let release = current
                .release_id
                .take()
                .context("missing folder release")?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT OR IGNORE INTO temp.library_album_companions(release_id,track_id)
                SELECT ?1,track_id FROM temp.library_album_members",
                [&release],
            )?;
            Group::Release(release).remember(&transaction)?;
            transaction.execute(
                "DELETE FROM temp.library_album_groups WHERE key=?1",
                [&current.key],
            )?;
            transaction.execute("DELETE FROM temp.library_album_members", [])?;
            transaction.commit()?;
            self.current = None;
            return Ok(false);
        }
        let infer = current.diverse && !current.protected && !current.conflicting_releases;
        let release_id = if current.folder {
            None
        } else {
            current.release_id.as_deref()
        };
        let rows = connection
            .prepare(
                "SELECT t.id,a.path,t.metadata,a.metadata_version,al.musicbrainz_release_id
            FROM temp.library_album_members m JOIN library_tracks t ON t.id=m.track_id
            JOIN library_assets a ON a.id=t.asset_id
            JOIN library_albums al ON al.id=t.album_id ORDER BY m.track_id LIMIT ?1",
            )?
            .query_map([db::BATCH as u64], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            connection.execute(
                "DELETE FROM temp.library_album_groups WHERE key=?1",
                [&current.key],
            )?;
            self.current = None;
            return Ok(false);
        }
        // Prepare changed metadata without reserving the database writer. Old
        // versions retain their effective metadata until native probing succeeds.
        let mut updates = Vec::with_capacity(rows.len());
        for (id, path, json, version, old_release_id) in &rows {
            if *version != db::METADATA_VERSION {
                continue;
            }
            let mut metadata: TrackMetadata = serde_json::from_str(json)?;
            if current.folder
                && metadata
                    .musicbrainz_release_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty())
            {
                continue;
            }
            let changed = restore_explicit_metadata(&mut metadata, infer);
            if changed || old_release_id.as_deref() != release_id {
                updates.push((*id, path, metadata));
            }
        }
        let behavior = if updates.is_empty() {
            TransactionBehavior::Deferred
        } else {
            TransactionBehavior::Immediate
        };
        let transaction = connection.transaction_with_behavior(behavior)?;
        for (id, path, metadata) in &updates {
            let album_id =
                db::album_for_release(&transaction, Path::new(path), metadata, release_id)?;
            transaction.execute(
                "UPDATE library_tracks SET album_id=?1,metadata=?2,search=?3 WHERE id=?4",
                params![
                    album_id,
                    serde_json::to_string(metadata)?,
                    db::track_search(path, metadata),
                    id
                ],
            )?;
        }
        for (id, _, _, _, _) in &rows {
            transaction.execute(
                "DELETE FROM temp.library_album_members WHERE track_id=?1",
                [id],
            )?;
        }
        if !updates.is_empty() {
            db::bump_revision(&transaction)?;
        }
        transaction.commit()?;
        Ok(false)
    }
}
