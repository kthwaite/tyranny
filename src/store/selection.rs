//! Resolves a `TrackSelection` to `(track_id, path)` pairs. Resolution runs in
//! one read transaction under the library revision the client saw; list rows
//! come from `listing::compile`, which also checks a playlist source's revision.
use super::{Store, list_source_rule, playlists::library_revision};
use crate::{
    library::resolve_track,
    library_types::{LibraryError, LibraryErrorKind},
    listing::{CompiledList, ListDescription, ListSource, compile},
    protocol::{MAX_PAGE_SIZE, MAX_RUN_ENTRIES, MAX_SELECTION_RANGES, TrackSelection},
};
use anyhow::Result;
use rusqlite::{Connection, params_from_iter, types::Value};
use std::{collections::HashSet, path::PathBuf};

fn failure(kind: LibraryErrorKind, message: impl Into<String>) -> anyhow::Error {
    LibraryError {
        kind,
        message: message.into(),
    }
    .into()
}
fn invalid(message: impl Into<String>) -> anyhow::Error {
    failure(LibraryErrorKind::Invalid, message)
}

fn selection_revision(selection: &TrackSelection) -> u64 {
    match selection {
        TrackSelection::Ids {
            library_revision, ..
        }
        | TrackSelection::List {
            library_revision, ..
        } => *library_revision,
    }
}

/// Rechecks every revision `selection` was made under: the library's, and a
/// playlist source's. A write that must match the resolved rows runs this in
/// its own transaction.
pub(super) fn check_revisions(connection: &Connection, selection: &TrackSelection) -> Result<()> {
    library_revision(connection, Some(selection_revision(selection)))?;
    if let TrackSelection::List { list, .. } = selection {
        match list.source {
            ListSource::Playlist { id, revision } => {
                list_source_rule(connection, id, revision, false)?;
            }
            ListSource::Smart { id, revision } => {
                list_source_rule(connection, id, revision, true)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate(selection: &TrackSelection) -> Result<()> {
    match selection {
        TrackSelection::Ids { track_ids, .. } => {
            if track_ids.is_empty() || track_ids.len() > MAX_PAGE_SIZE {
                return Err(invalid(format!(
                    "an ID selection names 1..={MAX_PAGE_SIZE} tracks"
                )));
            }
        }
        TrackSelection::List {
            ranges, excluded, ..
        } => {
            if ranges.is_empty() || ranges.len() > MAX_SELECTION_RANGES {
                return Err(invalid(format!(
                    "a list selection names 1..={MAX_SELECTION_RANGES} ranges"
                )));
            }
            if ranges.iter().any(|[start, end]| start >= end)
                || ranges.windows(2).any(|pair| pair[0][1] > pair[1][0])
            {
                return Err(invalid(
                    "selection ranges must be non-empty, sorted and disjoint",
                ));
            }
            if excluded.len() > MAX_PAGE_SIZE {
                return Err(invalid(format!(
                    "a list selection excludes at most {MAX_PAGE_SIZE} rows"
                )));
            }
        }
    }
    Ok(())
}

/// The selected rows of `list`, in list order.
fn list_rows(
    connection: &Connection,
    list: &ListDescription,
    ranges: &[[u64; 2]],
    excluded: &[u64],
    first_row_id: Option<u64>,
    recent_cutoff: i64,
) -> Result<Vec<(u64, PathBuf)>> {
    let CompiledList { sql, mut values } = compile(connection, list, recent_cutoff, 1)?;
    // Row `r` (0-based) has position `r + 1`, so `[start, end)` is
    // `start < position <= end`. Bounds past `i64::MAX` mean the end.
    let mut bound = |value: u64| {
        values.push(Value::Integer(i64::try_from(value).unwrap_or(i64::MAX)));
        values.len()
    };
    let within = ranges
        .iter()
        .map(|&[start, end]| {
            let start = bound(start);
            format!("(c.position>?{start} AND c.position<=?{})", bound(end))
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut statement = connection.prepare(&format!(
        "SELECT c.position,c.row_id,c.track_id,c.available,a.path FROM ({sql}) c
         JOIN library_tracks t ON t.id=c.track_id JOIN library_assets a ON a.id=t.asset_id
         WHERE {within} ORDER BY c.position"
    ))?;
    let mut rows = statement.query(params_from_iter(values.iter()))?;
    let changed = || failure(LibraryErrorKind::StaleRevision, "list changed; reload");
    let excluded: HashSet<u64> = excluded.iter().copied().collect();
    let mut sources = Vec::new();
    let mut first = true;
    while let Some(row) = rows.next()? {
        let (position, row_id): (u64, u64) = (row.get(0)?, row.get(1)?);
        if std::mem::take(&mut first)
            && first_row_id
                .is_some_and(|expected| (expected, ranges[0][0] + 1) != (row_id, position))
        {
            return Err(changed());
        }
        if excluded.contains(&row_id) {
            continue;
        }
        if !row.get::<_, bool>(3)? {
            return Err(invalid("selection contains an unavailable library track"));
        }
        if sources.len() == MAX_RUN_ENTRIES {
            return Err(invalid(format!(
                "selection resolves to more than {MAX_RUN_ENTRIES} tracks"
            )));
        }
        sources.push((row.get(2)?, PathBuf::from(row.get::<_, String>(4)?)));
    }
    if first && first_row_id.is_some() {
        return Err(changed());
    }
    Ok(sources)
}

impl Store {
    /// The selected tracks and their indexed paths, in selection order.
    pub fn resolve_selection(
        &mut self,
        selection: &TrackSelection,
        recent_cutoff: i64,
    ) -> Result<Vec<(u64, PathBuf)>> {
        validate(selection)?;
        let transaction = self.connection.transaction()?;
        library_revision(&transaction, Some(selection_revision(selection)))?;
        let sources = match selection {
            TrackSelection::Ids {
                library_revision,
                track_ids,
            } => track_ids
                .iter()
                .map(|&id| {
                    let path = resolve_track(&transaction, id, *library_revision)?;
                    Ok((id, PathBuf::from(path)))
                })
                .collect::<Result<Vec<_>>>()?,
            TrackSelection::List {
                list,
                ranges,
                excluded,
                first_row_id,
                ..
            } => list_rows(
                &transaction,
                list,
                ranges,
                excluded,
                *first_row_id,
                recent_cutoff,
            )?,
        };
        if sources.is_empty() {
            return Err(invalid("selection resolves to no tracks"));
        }
        transaction.commit()?;
        Ok(sources)
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::{
        library_types::{LibraryError, LibraryErrorKind, StaleLibraryRevision},
        listing::{Browser, Grouping, ListDescription, ListSource, SortField, SortKey},
        protocol::{ErrorCode, MAX_RUN_ENTRIES, TrackSelection},
        store::PlaylistFailure,
    };

    /// `count` tracks in one album, in track-ID order by default. Track 5 is
    /// unavailable: its asset was last seen before the root's completed scan.
    /// Playlist 1 (revision 3) holds occurrences 10..=13 of tracks 2, 5, 2, 1.
    fn fixture(count: u32) -> Store {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate(&mut connection, SCHEMA_VERSION).unwrap();
        connection
            .execute_batch(
                "INSERT INTO library_roots(id,path,search,completed_scan) VALUES(1,'/m','/m',2);
                INSERT INTO library_artists(id,identity,name,search) VALUES(1,'a','A','a');
                INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation)
                    VALUES(1,'b',1,'B','A','b','b',0);
                UPDATE library_state SET revision=7 WHERE singleton=1;",
            )
            .unwrap();
        connection
            .execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
                 INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan)
                 SELECT i,1,printf('/m/%06d.flac',i),1,CASE WHEN i=5 THEN 1 ELSE 2 END FROM n",
                [count],
            )
            .unwrap();
        connection
            .execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
                 INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,
                     disc_number,disc_total,track_number)
                 SELECT i,i,1,1,'{}',printf('t%06d',i),printf('t%06d',i),1,1,i FROM n",
                [count],
            )
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO playlists(id,name,search,revision) VALUES(1,'Mix','mix',3);
                INSERT INTO playlist_entries(id,playlist_id,track_id,ordinal) VALUES
                    (10,1,2,0),(11,1,5,1),(12,1,2,2),(13,1,1,3);",
            )
            .unwrap();
        Store { connection }
    }
    fn songs(descending: bool) -> ListDescription {
        ListDescription {
            source: ListSource::Library,
            browser: Browser::default(),
            query: String::new(),
            sort: vec![SortKey {
                field: SortField::Title,
                descending,
            }],
            group: Grouping::None,
        }
    }
    fn playlist(revision: u64) -> ListDescription {
        ListDescription {
            source: ListSource::Playlist { id: 1, revision },
            sort: Vec::new(),
            ..songs(false)
        }
    }
    fn rows(
        list: ListDescription,
        ranges: &[[u64; 2]],
        excluded: &[u64],
        first_row_id: Option<u64>,
    ) -> TrackSelection {
        TrackSelection::List {
            list,
            library_revision: 7,
            ranges: ranges.to_vec(),
            excluded: excluded.to_vec(),
            first_row_id,
        }
    }
    fn ids(track_ids: &[u64]) -> TrackSelection {
        TrackSelection::Ids {
            library_revision: 7,
            track_ids: track_ids.to_vec(),
        }
    }
    fn resolved(track_ids: &[u64]) -> Vec<(u64, PathBuf)> {
        track_ids
            .iter()
            .map(|&id| (id, PathBuf::from(format!("/m/{id:06}.flac"))))
            .collect()
    }
    fn code(error: anyhow::Error) -> (ErrorCode, String) {
        let message = error.to_string();
        if error.is::<StaleLibraryRevision>() {
            return (ErrorCode::StaleLibraryRevision, message);
        }
        if let Some(failure) = error.downcast_ref::<PlaylistFailure>() {
            return (failure.code, message);
        }
        match error.downcast_ref::<LibraryError>().map(|error| error.kind) {
            Some(LibraryErrorKind::Invalid) => (ErrorCode::InvalidCommand, message),
            Some(LibraryErrorKind::StaleRevision) => (ErrorCode::StaleLibraryRevision, message),
            _ => panic!("unexpected selection error: {error:#}"),
        }
    }
    fn rejected(store: &mut Store, selection: TrackSelection) -> (ErrorCode, String) {
        code(store.resolve_selection(&selection, 0).unwrap_err())
    }

    #[test]
    fn ids_and_list_selections_resolve_identically_for_the_same_rows() {
        let mut store = fixture(10);
        // Title descending, without unavailable track 5: 10,9,8,7,6,4,3,2,1.
        let listed = store
            .resolve_selection(&rows(songs(true), &[[1, 3], [5, 7]], &[], Some(9)), 0)
            .unwrap();
        assert_eq!(listed, resolved(&[9, 8, 4, 3]));
        let named = store.resolve_selection(&ids(&[9, 8, 4, 3]), 0).unwrap();
        assert_eq!(listed, named);
    }

    #[test]
    fn ranges_and_exclusions_select_exact_rows_in_list_order() {
        let mut store = fixture(10);
        // Library order 1,2,3,4,6,7,8,9,10. Exclusions name row IDs, and an
        // exclusion outside the ranges changes nothing.
        let selection = rows(songs(false), &[[0, 2], [3, u64::MAX]], &[7, 99], None);
        assert_eq!(
            store.resolve_selection(&selection, 0).unwrap(),
            resolved(&[1, 2, 4, 6, 8, 9, 10])
        );
        assert_eq!(
            store
                .resolve_selection(&rows(songs(true), &[[7, 9]], &[], Some(2)), 0)
                .unwrap(),
            resolved(&[2, 1])
        );
        // Playlist rows are occurrences: duplicates stay, exclusions name
        // occurrence IDs, and an excluded unavailable row is allowed.
        assert_eq!(
            store
                .resolve_selection(&rows(playlist(3), &[[0, u64::MAX]], &[11], Some(10)), 0)
                .unwrap(),
            resolved(&[2, 2, 1])
        );
        assert_eq!(
            store
                .resolve_selection(&rows(playlist(3), &[[2, 4]], &[], Some(12)), 0)
                .unwrap(),
            resolved(&[2, 1])
        );
        let (code, message) = rejected(&mut store, rows(playlist(3), &[[0, 2]], &[], None));
        assert_eq!(code, ErrorCode::InvalidCommand);
        assert!(message.contains("unavailable"), "{message}");
    }

    #[test]
    fn first_row_id_mismatch_is_stale() {
        let mut store = fixture(10);
        assert_eq!(
            store
                .resolve_selection(&rows(songs(false), &[[3, 5]], &[], Some(4)), 0)
                .unwrap(),
            resolved(&[4, 6])
        );
        let stale = (
            ErrorCode::StaleLibraryRevision,
            "list changed; reload".to_owned(),
        );
        for selection in [
            rows(songs(false), &[[3, 5]], &[], Some(6)),
            // The first row is checked even when it is excluded.
            rows(songs(false), &[[3, 5]], &[4], Some(6)),
            // No row at the range start.
            rows(songs(false), &[[20, 30]], &[], Some(1)),
        ] {
            assert_eq!(rejected(&mut store, selection), stale);
        }
        for selection in [
            TrackSelection::Ids {
                library_revision: 6,
                track_ids: vec![1],
            },
            TrackSelection::List {
                list: songs(false),
                library_revision: 6,
                ranges: vec![[0, 1]],
                excluded: Vec::new(),
                first_row_id: Some(1),
            },
        ] {
            assert_eq!(
                rejected(&mut store, selection).0,
                ErrorCode::StaleLibraryRevision
            );
        }
        assert_eq!(
            rejected(&mut store, rows(playlist(2), &[[0, 1]], &[], None)).0,
            ErrorCode::StalePlaylistRevision
        );
    }

    #[test]
    fn selection_limits_are_enforced() {
        let mut store = fixture(10);
        let invalid = |store: &mut Store, selection| {
            assert_eq!(
                rejected(store, selection).0,
                ErrorCode::InvalidCommand,
                "accepted an invalid selection"
            );
        };
        invalid(&mut store, ids(&[]));
        invalid(&mut store, ids(&[1; 501]));
        invalid(&mut store, ids(&[0]));
        invalid(&mut store, ids(&[999]));
        invalid(&mut store, ids(&[5]));
        assert_eq!(
            store.resolve_selection(&ids(&[1; 500]), 0).unwrap().len(),
            500
        );

        let ranges = |count: u64| (0..count).map(|i| [2 * i, 2 * i + 1]).collect::<Vec<_>>();
        invalid(&mut store, rows(songs(false), &ranges(65), &[], None));
        // 64 ranges is the limit; past the end they select nothing.
        assert_eq!(
            store
                .resolve_selection(&rows(songs(false), &ranges(64), &[], None), 0)
                .unwrap(),
            resolved(&[1, 3, 6, 8, 10])
        );
        let bad: [&[[u64; 2]]; 5] = [
            &[],
            &[[3, 3]],
            &[[4, 2]],
            &[[0, 3], [2, 5]],
            &[[4, 5], [0, 1]],
        ];
        for bad in bad {
            invalid(&mut store, rows(songs(false), bad, &[], None));
        }
        let excluded: Vec<u64> = (1000..1501).collect();
        invalid(&mut store, rows(songs(false), &[[0, 1]], &excluded, None));
        assert_eq!(
            store
                .resolve_selection(&rows(songs(false), &[[0, 1]], &excluded[..500], None), 0)
                .unwrap(),
            resolved(&[1])
        );
        // Zero rows: past the end, or every selected row excluded.
        invalid(&mut store, rows(songs(false), &[[20, 30]], &[], None));
        invalid(&mut store, rows(songs(false), &[[0, 2]], &[1, 2], None));

        let count = MAX_RUN_ENTRIES as u64 + 2;
        let mut large = fixture(count as u32);
        let largest = rows(songs(false), &[[0, MAX_RUN_ENTRIES as u64]], &[], Some(1));
        assert_eq!(
            large.resolve_selection(&largest, 0).unwrap().len(),
            MAX_RUN_ENTRIES
        );
        // Track 5 is unavailable, so the whole list is one row over the limit.
        invalid(&mut large, rows(songs(false), &[[0, u64::MAX]], &[], None));
    }
}
