use crate::{filter::Filter, library_types::*};
use anyhow::{Result, anyhow};
use rusqlite::{
    Connection, params, params_from_iter,
    types::{Type, Value},
};

fn invalid(message: &str) -> anyhow::Error {
    LibraryError {
        kind: LibraryErrorKind::Invalid,
        message: message.into(),
    }
    .into()
}

fn json<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<T> {
    let text: String = row.get(column)?;
    serde_json::from_str(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
    })
}

/// Counts, revision checks and page rows share one read transaction. Rust's
/// Unicode lowercase is persisted separately from untouched display metadata;
/// instr implements a literal substring, not SQL LIKE wildcard syntax.
pub(super) fn execute(
    connection: &mut Connection,
    command: LibraryCommand,
) -> Result<LibraryReply> {
    let request = match &command {
        LibraryCommand::Roots(page)
        | LibraryCommand::Artists(page)
        | LibraryCommand::Albums { page, .. }
        | LibraryCommand::Tracks { page, .. } => page,
        _ => return Err(anyhow!("mutation passed to library query")),
    };
    if request.limit == 0 || request.limit > 500 || request.offset > i64::MAX as usize {
        return Err(invalid(
            "library page requires limit 1..=500 and a representable offset",
        ));
    }
    let filter = if matches!(&command, LibraryCommand::Roots(_)) {
        if request.query.len() > 1024 {
            return Err(invalid("library filter exceeds 1024 UTF-8 bytes"));
        }
        Filter::parse("", 1)?
    } else {
        Filter::parse(&request.query, 1)?
    };
    match &command {
        LibraryCommand::Albums {
            artist_id: Some(id),
            ..
        }
        | LibraryCommand::Tracks {
            album_id: Some(id), ..
        } if *id == 0 || *id > i64::MAX as u64 => return Err(invalid("invalid library group ID")),
        _ => {}
    }
    let transaction = connection.transaction()?;
    let revision: u64 = transaction.query_row(
        "SELECT revision FROM library_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if request
        .revision
        .is_some_and(|expected| expected != revision)
    {
        return Err(StaleLibraryRevision { current: revision }.into());
    }
    let offset = request.offset;
    let limit = request.limit as i64;
    let predicate = filter.sql;
    let mut values = filter.values;
    let group_parameter = values.len() + 1;
    let reply = match command {
        LibraryCommand::Roots(page) => {
            let search = page.query.to_lowercase();
            let total = transaction.query_row(
                "SELECT COUNT(*) FROM library_roots WHERE instr(search,?1)>0",
                [&search],
                |row| row.get(0),
            )?;
            let mut statement = transaction.prepare(
                "SELECT id,path,last_scan FROM library_roots
                WHERE instr(search,?1)>0 ORDER BY search,path,id LIMIT ?2 OFFSET ?3",
            )?;
            let entries = statement
                .query_map(params![search, limit, offset as i64], |row| {
                    let last_scan = row
                        .get::<_, Option<String>>(2)?
                        .map(|text| {
                            serde_json::from_str(&text).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    2,
                                    Type::Text,
                                    Box::new(error),
                                )
                            })
                        })
                        .transpose()?;
                    Ok(Root {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        last_scan,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            LibraryReply::Roots(Page {
                revision,
                total,
                offset,
                entries,
            })
        }
        LibraryCommand::Artists(_) => {
            let source = format!(
                "FROM library_artists ar
                JOIN library_albums al ON al.artist_id=ar.id
                JOIN library_tracks t ON t.album_id=al.id
                JOIN library_assets a ON a.id=t.asset_id JOIN library_roots r ON r.id=a.root_id
                WHERE a.indexed=1 AND a.last_seen_scan>=r.completed_scan AND ({predicate})"
            );
            let total = transaction.query_row(
                &format!("SELECT COUNT(DISTINCT ar.id) {source}"),
                params_from_iter(values.iter()),
                |row| row.get(0),
            )?;
            let limit_parameter = values.len() + 1;
            let offset_parameter = limit_parameter + 1;
            values.extend([Value::Integer(limit), Value::Integer(offset as i64)]);
            let mut statement = transaction.prepare(&format!(
                "SELECT ar.id,ar.name,COUNT(DISTINCT al.id) {source}
                GROUP BY ar.id ORDER BY ar.search,ar.name,ar.id
                LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}"
            ))?;
            let entries = statement
                .query_map(params_from_iter(values.iter()), |row| {
                    Ok(AlbumArtist {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        album_count: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            LibraryReply::Artists(Page {
                revision,
                total,
                offset,
                entries,
            })
        }
        LibraryCommand::Albums { artist_id, .. } => {
            values.push(artist_id.map_or(Value::Null, |id| Value::Integer(id as i64)));
            let source = format!(
                "FROM library_albums al
                JOIN library_tracks t ON t.album_id=al.id
                JOIN library_assets a ON a.id=t.asset_id JOIN library_roots r ON r.id=a.root_id
                WHERE a.indexed=1 AND a.last_seen_scan>=r.completed_scan
                AND (?{group_parameter} IS NULL OR al.artist_id=?{group_parameter})
                AND ({predicate})"
            );
            let total = transaction.query_row(
                &format!("SELECT COUNT(DISTINCT al.id) {source}"),
                params_from_iter(values.iter()),
                |row| row.get(0),
            )?;
            let limit_parameter = values.len() + 1;
            let offset_parameter = limit_parameter + 1;
            values.extend([Value::Integer(limit), Value::Integer(offset as i64)]);
            let mut statement = transaction.prepare(&format!(
                "SELECT al.id,al.artist_id,al.title,al.album_artist,
                al.musicbrainz_release_id,al.compilation,MAX(MAX(t.disc_number,t.disc_total,1)),COUNT(t.id)
                {source} GROUP BY al.id ORDER BY al.title_sort,al.album_artist,al.id
                LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}"
            ))?;
            let entries = statement
                .query_map(params_from_iter(values.iter()), |row| {
                    Ok(Album {
                        id: row.get(0)?,
                        artist_id: row.get(1)?,
                        title: row.get(2)?,
                        album_artist: row.get(3)?,
                        musicbrainz_release_id: row.get(4)?,
                        compilation: row.get(5)?,
                        disc_count: row.get(6)?,
                        track_count: row.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            LibraryReply::Albums(Page {
                revision,
                total,
                offset,
                entries,
            })
        }
        LibraryCommand::Tracks { album_id, .. } => {
            values.push(album_id.map_or(Value::Null, |id| Value::Integer(id as i64)));
            let condition = format!("a.indexed=1 AND a.last_seen_scan>=r.completed_scan
                AND (?{group_parameter} IS NULL OR t.album_id=?{group_parameter}) AND ({predicate})");
            let total = transaction.query_row(
                &format!(
                    "SELECT COUNT(*) FROM library_tracks t
                JOIN library_assets a ON a.id=t.asset_id JOIN library_roots r ON r.id=a.root_id
                WHERE {condition}"
                ),
                params_from_iter(values.iter()),
                |row| row.get(0),
            )?;
            // Walk the track-order prefix and materialize only narrow matching
            // rows before hydrating metadata. Empty filters add no group lookup.
            let limit_parameter = values.len() + 1;
            let offset_parameter = limit_parameter + 1;
            values.extend([Value::Integer(limit), Value::Integer(offset as i64)]);
            let mut statement = transaction.prepare(&format!(
                "WITH track_page AS MATERIALIZED (
                SELECT t.id,t.disc_number,t.track_number,t.title_sort,a.path
                FROM library_tracks t CROSS JOIN library_assets a ON a.id=t.asset_id
                JOIN library_roots r ON r.id=a.root_id WHERE {condition}
                ORDER BY t.disc_number,t.track_number,t.title_sort,a.path,t.id
                LIMIT ?{limit_parameter} OFFSET ?{offset_parameter})
                SELECT t.id,t.asset_id,t.album_id,p.path,t.metadata,a.error,t.liked
                FROM track_page p JOIN library_tracks t ON t.id=p.id
                JOIN library_assets a ON a.id=t.asset_id
                ORDER BY p.disc_number,p.track_number,p.title_sort,p.path,p.id"
            ))?;
            let entries = statement
                .query_map(params_from_iter(values.iter()), |row| {
                    Ok(Track {
                        id: row.get(0)?,
                        asset_id: row.get(1)?,
                        album_id: row.get(2)?,
                        path: row.get(3)?,
                        metadata: json(row, 4)?,
                        error: row.get(5)?,
                        liked: row.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            LibraryReply::Tracks(Page {
                revision,
                total,
                offset,
                entries,
            })
        }
        _ => unreachable!("mutations rejected above"),
    };
    transaction.commit()?;
    Ok(reply)
}
