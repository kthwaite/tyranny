use crate::{
    library_types::*,
    scanner::{Candidate, ProbeResult},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) const BATCH: usize = 64;
/// Bump when extraction learns new fields so unchanged files are re-read once.
pub(super) const METADATA_VERSION: i64 = 2;
pub(super) const ERROR_LIMIT: usize = 1024;

/// The completed-scan watermark is an atomic tombstone set: discovery may mark
/// assets seen, but only a fully enumerated/indexed scan may advance this mark.
/// Interrupted enumeration therefore cannot accidentally remove old tracks.
pub(crate) fn migrate(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        "CREATE TABLE library_state (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            revision INTEGER NOT NULL CHECK(revision>0),
            scan_serial INTEGER NOT NULL DEFAULT 0,
            root_count INTEGER NOT NULL DEFAULT 0,
            track_count INTEGER NOT NULL DEFAULT 0,
            scan TEXT NOT NULL
        );
        CREATE TABLE library_roots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            search TEXT NOT NULL,
            completed_scan INTEGER NOT NULL DEFAULT 0,
            last_scan TEXT
        );
        CREATE TABLE library_assets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            root_id INTEGER NOT NULL REFERENCES library_roots(id),
            path TEXT NOT NULL UNIQUE,
            size INTEGER,
            modified_ns INTEGER,
            indexed INTEGER NOT NULL DEFAULT 0 CHECK(indexed IN (0,1)),
            last_seen_scan INTEGER NOT NULL,
            error TEXT
        );
        CREATE INDEX library_assets_seen ON library_assets(root_id,indexed,last_seen_scan);
        CREATE TABLE library_artists (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            identity TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            search TEXT NOT NULL
        );
        CREATE INDEX library_artists_order ON library_artists(search,name,id);
        CREATE TABLE library_albums (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            identity TEXT NOT NULL UNIQUE,
            artist_id INTEGER NOT NULL REFERENCES library_artists(id),
            title TEXT NOT NULL,
            album_artist TEXT NOT NULL,
            search TEXT NOT NULL,
            title_sort TEXT NOT NULL,
            musicbrainz_release_id TEXT,
            compilation INTEGER NOT NULL CHECK(compilation IN (0,1))
        );
        CREATE INDEX library_albums_artist ON library_albums(artist_id,title_sort,id);
        CREATE INDEX library_albums_order ON library_albums(title_sort,album_artist,id);
        CREATE TABLE library_tracks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            asset_id INTEGER NOT NULL UNIQUE REFERENCES library_assets(id),
            artist_id INTEGER NOT NULL REFERENCES library_artists(id),
            album_id INTEGER NOT NULL REFERENCES library_albums(id),
            segment_start_ms INTEGER NOT NULL DEFAULT 0 CHECK(segment_start_ms>=0),
            segment_end_ms INTEGER CHECK(segment_end_ms>segment_start_ms),
            metadata TEXT NOT NULL,
            search TEXT NOT NULL,
            title_sort TEXT NOT NULL,
            disc_number INTEGER NOT NULL,
            disc_total INTEGER NOT NULL,
            track_number INTEGER NOT NULL
        );
        CREATE INDEX library_tracks_album ON library_tracks(album_id,disc_number,track_number,title_sort,id);
        CREATE INDEX library_tracks_artist ON library_tracks(artist_id);
        CREATE INDEX library_tracks_order ON library_tracks(disc_number,track_number,title_sort,id);
        CREATE TABLE library_worklist (
            path TEXT PRIMARY KEY,
            scan_id INTEGER NOT NULL,
            size INTEGER NOT NULL,
            modified_ns INTEGER NOT NULL
        );
        CREATE INDEX library_worklist_scan ON library_worklist(scan_id,path);"
    )?;
    transaction.execute(
        "INSERT INTO library_state(singleton,revision,scan) VALUES(1,1,?1)",
        [serde_json::to_string(&ScanStatus::default())?],
    )?;
    Ok(())
}

/// Schema 5: richer track fields, genres, play statistics and history. Existing
/// rows keep blank new fields; `pending_upgrade` asks for a rescan to fill them.
pub(crate) fn migrate_v5(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        "ALTER TABLE library_tracks ADD COLUMN year INTEGER;
        ALTER TABLE library_tracks ADD COLUMN codec TEXT;
        ALTER TABLE library_tracks ADD COLUMN bit_depth INTEGER;
        ALTER TABLE library_tracks ADD COLUMN bitrate_kbps INTEGER;
        ALTER TABLE library_tracks ADD COLUMN file_size INTEGER;
        ALTER TABLE library_tracks ADD COLUMN date_added INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE library_assets ADD COLUMN metadata_version INTEGER NOT NULL DEFAULT 1;
        ALTER TABLE library_assets ADD COLUMN created_ns INTEGER;
        ALTER TABLE library_state ADD COLUMN pending_upgrade INTEGER NOT NULL DEFAULT 0;
        CREATE TABLE library_genres (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            search TEXT NOT NULL UNIQUE
        );
        CREATE TABLE library_track_genres (
            track_id INTEGER NOT NULL REFERENCES library_tracks(id),
            genre_id INTEGER NOT NULL REFERENCES library_genres(id),
            position INTEGER NOT NULL,
            PRIMARY KEY(track_id,genre_id)
        );
        CREATE TABLE track_stats (
            track_id INTEGER PRIMARY KEY REFERENCES library_tracks(id),
            play_count INTEGER NOT NULL DEFAULT 0,
            last_played INTEGER
        );
        CREATE TABLE play_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            played_at INTEGER NOT NULL,
            entry_id INTEGER NOT NULL,
            track_id INTEGER REFERENCES library_tracks(id),
            path TEXT NOT NULL,
            title TEXT NOT NULL,
            artist TEXT NOT NULL,
            album TEXT NOT NULL,
            duration_ms INTEGER,
            source_kind TEXT,
            source_name TEXT
        );
        CREATE INDEX library_tracks_year ON library_tracks(year,id);
        CREATE INDEX library_tracks_added ON library_tracks(date_added,id);
        CREATE INDEX library_track_genres_genre ON library_track_genres(genre_id,track_id);
        CREATE INDEX track_stats_count ON track_stats(play_count,track_id);
        CREATE INDEX track_stats_last ON track_stats(last_played,track_id);
        UPDATE library_state SET pending_upgrade=
            EXISTS(SELECT 1 FROM library_tracks) WHERE singleton=1;",
    )?;
    Ok(())
}

/// One statement also works inside the caller's queue transaction; the revision
/// and optional path necessarily belong to the same SQLite read snapshot.
pub(crate) fn resolve_track(
    connection: &Connection,
    track_id: u64,
    expected_revision: u64,
) -> Result<String> {
    let track_id = i64::try_from(track_id).ok();
    let (revision, path): (u64, Option<String>) = connection.query_row(
        "SELECT s.revision,(SELECT a.path FROM library_tracks t
         JOIN library_assets a ON a.id=t.asset_id
         JOIN library_roots r ON r.id=a.root_id
         WHERE t.id=?1 AND a.indexed=1 AND a.last_seen_scan>=r.completed_scan)
         FROM library_state s WHERE s.singleton=1",
        [track_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if revision != expected_revision {
        return Err(StaleLibraryRevision { current: revision }.into());
    }
    path.ok_or_else(|| {
        LibraryError {
            kind: LibraryErrorKind::Invalid,
            message: "library track is unknown or unavailable".into(),
        }
        .into()
    })
}

pub(super) fn bounded_error(error: impl AsRef<str>) -> String {
    let error = error.as_ref();
    let mut end = error.len().min(ERROR_LIMIT);
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error[..end].to_owned()
}

/// Duration and size are totals over visible tracks. They change only with
/// visible rows, so `previous` lends its totals while the revision holds:
/// summing them reads every track.
pub(super) fn state(
    connection: &Connection,
    previous: Option<&LibraryState>,
) -> Result<LibraryState> {
    let (revision, roots, tracks, scan): (u64, u64, u64, String) = connection.query_row(
        "SELECT revision,root_count,track_count,scan FROM library_state WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let (duration_ms, size_bytes) = match previous.filter(|state| state.revision == revision) {
        Some(state) => (state.duration_ms, state.size_bytes),
        None => connection.query_row(
            "SELECT COALESCE(SUM(json_extract(t.metadata,'$.duration_ms')),0),
             COALESCE(SUM(t.file_size),0)
             FROM library_tracks t JOIN library_assets a ON a.id=t.asset_id
             JOIN library_roots r ON r.id=a.root_id
             WHERE a.indexed=1 AND a.last_seen_scan>=r.completed_scan",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?,
    };
    Ok(LibraryState {
        revision,
        roots,
        tracks,
        duration_ms,
        size_bytes,
        scan: serde_json::from_str(&scan)?,
    })
}

/// The revision is the content revision: it changes only when rows visible to
/// list queries change, so clients may cache windows keyed by it. Call once per
/// transaction that makes such a change.
fn bump_revision(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute(
        "UPDATE library_state SET revision=revision+1 WHERE singleton=1",
        [],
    )?;
    Ok(())
}

/// Scan status only; scan progress never changes the content revision.
pub(super) fn save_status(transaction: &Transaction<'_>, status: &ScanStatus) -> Result<()> {
    let json = serde_json::to_string(status)?;
    transaction.execute(
        "UPDATE library_state SET scan=?1 WHERE singleton=1",
        [&json],
    )?;
    if let Some(root_id) = status.root_id {
        transaction.execute(
            "UPDATE library_roots SET last_scan=?1 WHERE id=?2",
            params![json, root_id],
        )?;
    }
    Ok(())
}

pub(super) fn recover(connection: &mut Connection) -> Result<LibraryState> {
    let mut current = state(connection, None)?;
    if matches!(
        current.scan.phase,
        ScanPhase::Discovering | ScanPhase::Indexing
    ) {
        current.scan.phase = ScanPhase::Interrupted;
        current.scan.last_error =
            Some("scan interrupted before durable completion; rescan manually".into());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        save_status(&transaction, &current.scan)?;
        transaction.commit()?;
        current = state(connection, Some(&current))?;
    }
    Ok(current)
}

pub(super) fn begin_scan(transaction: &Transaction<'_>, root_id: u64) -> Result<(u64, ScanStatus)> {
    transaction.execute(
        "UPDATE library_state SET scan_serial=scan_serial+1 WHERE singleton=1",
        [],
    )?;
    let serial = transaction.query_row(
        "SELECT scan_serial FROM library_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    let status = ScanStatus {
        root_id: Some(root_id),
        phase: ScanPhase::Discovering,
        ..ScanStatus::default()
    };
    save_status(transaction, &status)?;
    Ok((serial, status))
}

pub(super) fn discover(
    connection: &mut Connection,
    serial: u64,
    status: &mut ScanStatus,
    candidates: &[Candidate],
) -> Result<()> {
    ensure!(
        candidates.len() <= 128,
        "scanner discovery batch exceeded bound"
    );
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let root_id = status.root_id.context("scan has no root")?;
    let watermark: u64 = transaction.query_row(
        "SELECT completed_scan FROM library_roots WHERE id=?1",
        [root_id],
        |row| row.get(0),
    )?;
    let mut restored = 0u64;
    for candidate in candidates {
        let path = candidate
            .path
            .to_str()
            .context("non-UTF-8 candidate path; reconciliation prohibited")?;
        let old: Option<(bool, u64)> = transaction
            .query_row(
                "SELECT indexed,last_seen_scan FROM library_assets WHERE path=?1",
                [path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if old.is_some_and(|(indexed, seen)| indexed && seen < watermark) {
            restored += 1;
        }
        transaction.execute(
            "INSERT INTO library_assets(root_id,path,last_seen_scan) VALUES(?1,?2,?3)
            ON CONFLICT(path) DO UPDATE SET last_seen_scan=excluded.last_seen_scan",
            params![root_id, path, serial],
        )?;
        transaction.execute("INSERT INTO library_worklist(path,scan_id,size,modified_ns) VALUES(?1,?2,?3,?4)
            ON CONFLICT(path) DO UPDATE SET scan_id=excluded.scan_id,size=excluded.size,modified_ns=excluded.modified_ns",
            params![path,serial,candidate.size,candidate.modified_ns])?;
    }
    let mut next = status.clone();
    next.discovered += candidates.len() as u64;
    transaction.execute(
        "UPDATE library_state SET track_count=track_count+?1 WHERE singleton=1",
        [restored],
    )?;
    if restored > 0 {
        bump_revision(&transaction)?;
    }
    save_status(&transaction, &next)?;
    transaction.commit()?;
    *status = next;
    Ok(())
}

/// Successful unchanged fingerprints skip native probing, including after a
/// process restart. Errors deliberately prevent this fast path on the next scan.
pub(super) fn next_batch(
    connection: &mut Connection,
    serial: u64,
    status: &mut ScanStatus,
) -> Result<Vec<Candidate>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let entries = {
        let mut statement = transaction.prepare(
            "SELECT w.path,w.size,w.modified_ns,
            a.indexed=1 AND a.error IS NULL AND a.size=w.size AND a.modified_ns=w.modified_ns
            AND a.metadata_version=?3
            FROM library_worklist w JOIN library_assets a ON a.path=w.path
            WHERE w.scan_id=?1 ORDER BY w.path LIMIT ?2",
        )?;
        statement
            .query_map(params![serial, BATCH as u64, METADATA_VERSION], |row| {
                Ok((
                    Candidate {
                        path: row.get::<_, String>(0)?.into(),
                        size: row.get(1)?,
                        modified_ns: row.get(2)?,
                        created_ns: None,
                    },
                    row.get::<_, bool>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut probes = Vec::with_capacity(entries.len());
    let mut next = status.clone();
    for (candidate, unchanged) in entries {
        if unchanged {
            transaction.execute(
                "DELETE FROM library_worklist WHERE scan_id=?1 AND path=?2",
                params![
                    serial,
                    candidate.path.to_str().context("invalid worklist path")?
                ],
            )?;
            next.processed += 1;
            next.unchanged += 1;
        } else {
            probes.push(candidate);
        }
    }
    if next.processed != status.processed {
        save_status(&transaction, &next)?;
    }
    transaction.commit()?;
    *status = next;
    Ok(probes)
}

fn filled(value: &str) -> bool {
    !value.trim().is_empty()
}

fn artist(transaction: &Transaction<'_>, name: &str, musicbrainz_id: Option<&str>) -> Result<u64> {
    let identity = match musicbrainz_id.filter(|value| filled(value)) {
        Some(id) => serde_json::to_string(&("mb", id))?,
        None => serde_json::to_string(&("name", name))?,
    };
    transaction.execute(
        "INSERT INTO library_artists(identity,name,search) VALUES(?1,?2,?3)
        ON CONFLICT(identity) DO UPDATE SET name=excluded.name,search=excluded.search",
        params![identity, name, name.to_lowercase()],
    )?;
    Ok(transaction.query_row(
        "SELECT id FROM library_artists WHERE identity=?1",
        [identity],
        |row| row.get(0),
    )?)
}

fn install(
    transaction: &Transaction<'_>,
    candidate: &Candidate,
    metadata: TrackMetadata,
) -> Result<()> {
    let path = candidate.path.to_str().context("invalid probe path")?;
    let artist_id = artist(
        transaction,
        &metadata.artist,
        metadata.musicbrainz_artist_id.as_deref(),
    )?;
    let album_artist_id = artist(
        transaction,
        &metadata.album_artist,
        metadata.musicbrainz_album_artist_id.as_deref(),
    )?;
    let release_id = metadata
        .musicbrainz_release_id
        .as_deref()
        .filter(|value| filled(value));
    let identity = match release_id {
        Some(id) => serde_json::to_string(&("mb", id))?,
        None => serde_json::to_string(&(
            "folder",
            &metadata.album_artist,
            &metadata.album,
            candidate
                .path
                .parent()
                .and_then(|parent| parent.to_str())
                .context("track has no UTF-8 source directory")?,
        ))?,
    };
    transaction.execute("INSERT INTO library_albums(identity,artist_id,title,album_artist,search,title_sort,musicbrainz_release_id,compilation)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(identity) DO UPDATE SET
        artist_id=excluded.artist_id,title=excluded.title,album_artist=excluded.album_artist,
        search=excluded.search,title_sort=excluded.title_sort,musicbrainz_release_id=excluded.musicbrainz_release_id,
        compilation=excluded.compilation",
        params![identity,album_artist_id,metadata.album,metadata.album_artist,
            format!("{}\n{}",metadata.album,metadata.album_artist).to_lowercase(),metadata.album.to_lowercase(),release_id,metadata.compilation])?;
    let album_id: u64 = transaction.query_row(
        "SELECT id FROM library_albums WHERE identity=?1",
        [identity],
        |row| row.get(0),
    )?;
    let (asset_id, indexed): (u64, bool) = transaction.query_row(
        "SELECT id,indexed FROM library_assets WHERE path=?1",
        [path],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let added = (candidate.created_ns.unwrap_or(candidate.modified_ns) / 1_000_000_000).min(now);
    let genres = metadata.genres.join("\n");
    transaction.execute("INSERT INTO library_tracks(asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number,
        year,codec,bit_depth,bitrate_kbps,file_size,date_added)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) ON CONFLICT(asset_id) DO UPDATE SET
        artist_id=excluded.artist_id,album_id=excluded.album_id,metadata=excluded.metadata,search=excluded.search,
        title_sort=excluded.title_sort,disc_number=excluded.disc_number,disc_total=excluded.disc_total,track_number=excluded.track_number,
        year=excluded.year,codec=excluded.codec,bit_depth=excluded.bit_depth,bitrate_kbps=excluded.bitrate_kbps,file_size=excluded.file_size,
        date_added=CASE WHEN library_tracks.date_added=0 THEN excluded.date_added ELSE library_tracks.date_added END",
        params![asset_id,artist_id,album_id,serde_json::to_string(&metadata)?,
            format!("{}\n{}\n{}\n{}\n{genres}\n{path}",metadata.title,metadata.artist,metadata.album,metadata.album_artist).to_lowercase(),
            metadata.title.to_lowercase(),metadata.disc_number.unwrap_or(1),metadata.disc_total.unwrap_or(1),metadata.track_number.unwrap_or(0),
            metadata.year,metadata.codec,metadata.bit_depth,metadata.bitrate_kbps,candidate.size,added])?;
    let track_id: u64 = transaction.query_row(
        "SELECT id FROM library_tracks WHERE asset_id=?1",
        [asset_id],
        |row| row.get(0),
    )?;
    transaction.execute(
        "DELETE FROM library_track_genres WHERE track_id=?1",
        [track_id],
    )?;
    // One genre per lowercased name, spelt as the track indexed last spells it,
    // so a retag that changes only case shows.
    for (position, genre) in metadata.genres.iter().enumerate() {
        transaction.execute(
            "INSERT INTO library_genres(name,search) VALUES(?1,?2)
            ON CONFLICT(search) DO UPDATE SET name=excluded.name",
            params![genre, genre.to_lowercase()],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO library_track_genres(track_id,genre_id,position)
            SELECT ?1,id,?3 FROM library_genres WHERE search=?2",
            params![track_id, genre.to_lowercase(), position as u64],
        )?;
    }
    transaction.execute(
        "UPDATE library_assets SET size=?1,modified_ns=?2,created_ns=?3,metadata_version=?4,indexed=1,error=NULL WHERE id=?5",
        params![candidate.size, candidate.modified_ns, candidate.created_ns, METADATA_VERSION, asset_id],
    )?;
    if !indexed {
        transaction.execute(
            "UPDATE library_state SET track_count=track_count+1 WHERE singleton=1",
            [],
        )?;
    }
    Ok(())
}

pub(super) fn probed(
    connection: &mut Connection,
    serial: u64,
    status: &mut ScanStatus,
    results: Vec<ProbeResult>,
) -> Result<()> {
    ensure!(results.len() <= BATCH, "scanner probe batch exceeded bound");
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut next = status.clone();
    let mut changed = false;
    for probe in results {
        let path = probe
            .candidate
            .path
            .to_str()
            .context("invalid probe path")?;
        let pending: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_worklist
            WHERE scan_id=?1 AND path=?2 AND size=?3 AND modified_ns=?4)",
            params![
                serial,
                path,
                probe.candidate.size,
                probe.candidate.modified_ns
            ],
            |row| row.get(0),
        )?;
        ensure!(pending, "scanner returned an unexpected candidate");
        match probe.result {
            Ok(metadata) => {
                install(&transaction, &probe.candidate, metadata)?;
                changed = true;
                next.updated += 1;
            }
            Err(error) => {
                let error = bounded_error(error);
                let indexed: bool = transaction.query_row(
                    "SELECT indexed FROM library_assets WHERE path=?1",
                    [path],
                    |row| row.get(0),
                )?;
                changed |= indexed;
                transaction.execute(
                    "UPDATE library_assets SET error=?1 WHERE path=?2",
                    params![error, path],
                )?;
                next.failures += 1;
                next.last_error = Some(error);
            }
        }
        transaction.execute(
            "DELETE FROM library_worklist WHERE scan_id=?1 AND path=?2",
            params![serial, path],
        )?;
        next.processed += 1;
    }
    if changed {
        bump_revision(&transaction)?;
    }
    save_status(&transaction, &next)?;
    transaction.commit()?;
    *status = next;
    Ok(())
}

pub(super) fn finish(
    connection: &mut Connection,
    serial: u64,
    status: &mut ScanStatus,
    complete: bool,
) -> Result<()> {
    let root_id = status.root_id.context("scan has no root")?;
    let pending: u64 = connection.query_row(
        "SELECT COUNT(*) FROM library_worklist WHERE scan_id=?1",
        [serial],
        |row| row.get(0),
    )?;
    ensure!(
        pending == 0 && status.total == Some(status.processed),
        "indexing did not complete"
    );
    // Only this coordinator writes library rows. Count before taking SQLite's
    // writer reservation so even a large missing set cannot delay queue commits.
    let missing: u64 = if complete {
        connection.query_row("SELECT COUNT(*) FROM library_assets a JOIN library_roots r ON r.id=a.root_id
            WHERE a.root_id=?1 AND a.indexed=1 AND a.last_seen_scan>=r.completed_scan AND a.last_seen_scan<?2",
            params![root_id,serial], |row| row.get(0))?
    } else {
        0
    };
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut next = status.clone();
    if complete {
        transaction.execute(
            "UPDATE library_roots SET completed_scan=?1 WHERE id=?2",
            params![serial, root_id],
        )?;
        transaction.execute(
            "UPDATE library_state SET track_count=track_count-?1 WHERE singleton=1",
            [missing],
        )?;
        // Advancing the watermark only changes visible rows when it hides
        // assets the scan did not see.
        if missing > 0 {
            bump_revision(&transaction)?;
        }
        next.phase = ScanPhase::Complete;
    } else {
        next.phase = ScanPhase::Failed;
        if next.last_error.is_none() {
            next.last_error = Some("enumeration incomplete; existing tracks preserved".into());
        }
    }
    save_status(&transaction, &next)?;
    transaction.commit()?;
    *status = next;
    Ok(())
}

pub(super) fn discard_old_work(connection: &Connection, active_scan: Option<u64>) -> Result<usize> {
    Ok(connection.execute(
        "DELETE FROM library_worklist WHERE rowid IN
        (SELECT rowid FROM library_worklist WHERE scan_id!=?1 LIMIT 128)",
        [active_scan.unwrap_or(0)],
    )?)
}
