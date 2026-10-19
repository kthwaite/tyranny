//! List commands over `listing::compile`: track pages with totals, album
//! summaries, the artist and genre columns, and group spans. Each command runs
//! in one read transaction with its revision check, so the playlist header that
//! `compile` reads belongs to the same snapshot as the rows.
use crate::{
    filter::Filter,
    library_types::*,
    listing::{CompiledList, Grouping, ListDescription, compile, compile_members, recent_cutoff},
    protocol::MAX_PAGE_SIZE,
};
use anyhow::{Result, anyhow};
use rusqlite::{Row, Transaction, params_from_iter, types::Value};
use std::collections::HashMap;

/// A `ListGroups` reply holds at most this many spans.
const MAX_GROUPS: usize = 100_000;
const VISIBLE: &str = "a.indexed=1 AND a.last_seen_scan>=r.completed_scan";
/// Joins a track `t` to its asset `a` and root `r`.
const ASSET: &str =
    "JOIN library_assets a ON a.id=t.asset_id JOIN library_roots r ON r.id=a.root_id";
/// Columns read by `track_summary`, over `t`, `a`, its album `al` and stats `s`.
const TRACK_COLUMNS: &str = "t.id,t.album_id,t.artist_id,al.artist_id,
    COALESCE(json_extract(t.metadata,'$.title'),''),COALESCE(json_extract(t.metadata,'$.artist'),''),
    al.title,al.album_artist,al.compilation,t.disc_number,t.track_number,
    json_extract(t.metadata,'$.duration_ms'),t.year,
    COALESCE((SELECT group_concat(g.name,', ' ORDER BY tg.position) FROM library_track_genres tg
        JOIN library_genres g ON g.id=tg.genre_id WHERE tg.track_id=t.id),''),
    t.codec,t.bit_depth,COALESCE(json_extract(t.metadata,'$.sample_rate'),0),t.bitrate_kbps,
    COALESCE(s.play_count,0),s.last_played,t.date_added,a.path";

fn invalid(message: &str) -> anyhow::Error {
    LibraryError {
        kind: LibraryErrorKind::Invalid,
        message: message.into(),
    }
    .into()
}

fn page_bounds(offset: usize, limit: usize) -> Result<()> {
    if limit == 0 || limit > MAX_PAGE_SIZE || offset > i64::MAX as usize {
        return Err(invalid(
            "library page requires limit 1..=500 and a representable offset",
        ));
    }
    Ok(())
}

/// `now` is the request time in UTC seconds; the Recent source counts back from it.
pub(super) fn execute(
    connection: &mut rusqlite::Connection,
    command: LibraryCommand,
    now: i64,
) -> Result<LibraryReply> {
    let (expected, filter) = match &command {
        LibraryCommand::ListTracks {
            revision,
            offset,
            limit,
            ..
        }
        | LibraryCommand::ListAlbums {
            revision,
            offset,
            limit,
            ..
        } => {
            page_bounds(*offset, *limit)?;
            (*revision, None)
        }
        LibraryCommand::ListArtists { page, .. } => {
            page_bounds(page.offset, page.limit)?;
            (page.revision, Some(Filter::parse(&page.query, 1)?))
        }
        LibraryCommand::ListGenres(page) => {
            page_bounds(page.offset, page.limit)?;
            (page.revision, Some(Filter::parse(&page.query, 1)?))
        }
        LibraryCommand::AlbumSummaries {
            album_ids,
            revision,
        } => {
            if album_ids.is_empty() || album_ids.len() > MAX_PAGE_SIZE {
                return Err(invalid("album summaries require 1..=500 album IDs"));
            }
            if album_ids.iter().any(|&id| id == 0 || id > i64::MAX as u64) {
                return Err(invalid("invalid album ID"));
            }
            (*revision, None)
        }
        LibraryCommand::ListGroups { list, revision } => {
            if list.group == Grouping::None {
                return Err(invalid("list groups require a grouped list"));
            }
            (*revision, None)
        }
        _ => return Err(anyhow!("non-list command passed to list query")),
    };
    let transaction = connection.transaction()?;
    let revision: u64 = transaction.query_row(
        "SELECT revision FROM library_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if expected.is_some_and(|expected| expected != revision) {
        return Err(StaleLibraryRevision { current: revision }.into());
    }
    let cutoff = recent_cutoff(now);
    let reply = match command {
        LibraryCommand::ListTracks {
            list,
            offset,
            limit,
            ..
        } => {
            let compiled = compile(&transaction, &list, cutoff, 1)?;
            let members = compile_members(&transaction, &list, cutoff, 1)?;
            let (page, totals) = list_tracks(&transaction, compiled, members, offset, limit)?;
            LibraryReply::ListTracks {
                page: Page { revision, ..page },
                totals,
            }
        }
        LibraryCommand::ListAlbums {
            list,
            sort,
            descending,
            offset,
            limit,
            ..
        } => {
            // Only membership matters; ordering and grouping would be wasted work.
            let membership = ListDescription {
                sort: Vec::new(),
                group: Grouping::None,
                ..list
            };
            let CompiledList { sql, values } =
                compile_members(&transaction, &membership, cutoff, 1)?;
            let direction = if descending { "DESC" } else { "ASC" };
            let order = match sort {
                AlbumSort::AlbumArtist => format!("artist_search {direction}"),
                AlbumSort::Year => format!("year {direction} NULLS LAST"),
                AlbumSort::DateAdded => format!("NULLIF(date_added,0) {direction} NULLS LAST"),
            };
            let albums = format!("SELECT DISTINCT album_key FROM ({sql})");
            let (total, entries) = albums_page(
                &transaction,
                &albums,
                values,
                &format!("{order},artist_search,title_sort,id"),
                offset,
                limit,
            )?;
            LibraryReply::ListAlbums(Page {
                revision,
                total,
                offset,
                entries,
            })
        }
        LibraryCommand::ListArtists { page, list } => {
            // The filter was validated above; `artists` parses it again after the list.
            // Only membership matters; ordering and grouping would be wasted work.
            let members = list
                .map(|list| {
                    let membership = ListDescription {
                        sort: Vec::new(),
                        group: Grouping::None,
                        ..list
                    };
                    compile_members(&transaction, &membership, cutoff, 1)
                })
                .transpose()?;
            let (total, entries, compilations) =
                artists(&transaction, members, &page.query, page.offset, page.limit)?;
            LibraryReply::ListArtists {
                page: Page {
                    revision,
                    total,
                    offset: page.offset,
                    entries,
                },
                compilations,
            }
        }
        LibraryCommand::ListGenres(page) => {
            let filter = filter.expect("genre filter parsed above");
            let (total, entries) = genres(&transaction, filter, page.offset, page.limit)?;
            LibraryReply::ListGenres(Page {
                revision,
                total,
                offset: page.offset,
                entries,
            })
        }
        LibraryCommand::AlbumSummaries { album_ids, .. } => {
            let placeholders: Vec<String> =
                (1..=album_ids.len()).map(|n| format!("?{n}")).collect();
            let values = album_ids
                .iter()
                .map(|&id| Value::Integer(id as i64))
                .collect();
            let albums = format!(
                "SELECT id FROM library_albums WHERE id IN ({})",
                placeholders.join(",")
            );
            let (_, found) = albums_page(&transaction, &albums, values, "id", 0, MAX_PAGE_SIZE)?;
            let found: HashMap<u64, AlbumSummary> =
                found.into_iter().map(|album| (album.id, album)).collect();
            LibraryReply::AlbumSummaries {
                revision,
                entries: album_ids
                    .iter()
                    .filter_map(|id| found.get(id).cloned())
                    .collect(),
            }
        }
        LibraryCommand::ListGroups { list, .. } => {
            let compiled = compile(&transaction, &list, cutoff, 1)?;
            let groups = group_spans(&transaction, compiled, list.group)?;
            LibraryReply::ListGroups {
                revision,
                total: groups.iter().map(|group| group.count).sum(),
                groups,
            }
        }
        _ => unreachable!("non-list commands rejected above"),
    };
    transaction.commit()?;
    Ok(reply)
}

/// The page's revision is left 0 for the caller to fill.
fn list_tracks(
    transaction: &Transaction<'_>,
    CompiledList { sql, mut values }: CompiledList,
    members: CompiledList,
    offset: usize,
    limit: usize,
) -> Result<(Page<ListRow>, Option<ListTotals>)> {
    let CompiledList {
        sql: members,
        values: member_values,
    } = members;
    let (total, totals) = if offset == 0 {
        let totals = transaction.query_row(
            &format!(
                "SELECT COUNT(*),COALESCE(SUM(json_extract(t.metadata,'$.duration_ms')),0),
                 COALESCE(SUM(t.file_size),0)
                 FROM ({members}) l JOIN library_tracks t ON t.id=l.track_id"
            ),
            params_from_iter(member_values.iter()),
            |row| {
                Ok(ListTotals {
                    tracks: row.get(0)?,
                    duration_ms: row.get(1)?,
                    size_bytes: row.get(2)?,
                })
            },
        )?;
        (totals.tracks, Some(totals))
    } else {
        let total = transaction.query_row(
            &format!("SELECT COUNT(*) FROM ({members})"),
            params_from_iter(member_values.iter()),
            |row| row.get(0),
        )?;
        (total, None)
    };
    let offset_parameter = values.len() + 1;
    let limit_parameter = offset_parameter + 1;
    values.extend([Value::Integer(offset as i64), Value::Integer(limit as i64)]);
    let mut statement = transaction.prepare(&format!(
        "SELECT l.row_id,l.artist_key,l.album_key,l.available,{TRACK_COLUMNS}
         FROM (SELECT * FROM ({sql}) WHERE position>?{offset_parameter}
               AND position<=?{offset_parameter}+?{limit_parameter}) l
         CROSS JOIN library_tracks t ON t.id=l.track_id
         JOIN library_assets a ON a.id=t.asset_id JOIN library_albums al ON al.id=t.album_id
         LEFT JOIN track_stats s ON s.track_id=t.id
         ORDER BY l.position"
    ))?;
    let entries = statement
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(ListRow {
                row_id: row.get(0)?,
                artist_key: row.get(1)?,
                album_key: row.get(2)?,
                available: row.get(3)?,
                track: track_summary(row, 4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((
        Page {
            revision: 0,
            total,
            offset,
            entries,
        },
        totals,
    ))
}

/// Reads `TRACK_COLUMNS` starting at column `first`.
fn track_summary(row: &Row<'_>, first: usize) -> rusqlite::Result<TrackSummary> {
    let column = |n: usize| first + n;
    let codec: Option<String> = row.get(column(14))?;
    let bit_depth = row.get(column(15))?;
    let sample_rate = row.get(column(16))?;
    let bitrate_kbps = row.get(column(17))?;
    Ok(TrackSummary {
        id: row.get(column(0))?,
        album_id: row.get(column(1))?,
        artist_id: row.get(column(2))?,
        album_artist_id: row.get(column(3))?,
        title: row.get(column(4))?,
        artist: row.get(column(5))?,
        album: row.get(column(6))?,
        album_artist: row.get(column(7))?,
        compilation: row.get(column(8))?,
        disc_number: row.get(column(9))?,
        track_number: row.get(column(10))?,
        duration_ms: row.get(column(11))?,
        year: row.get(column(12))?,
        genre: row.get(column(13))?,
        format: format_label(codec.as_deref(), bit_depth, sample_rate, bitrate_kbps),
        sample_rate,
        bitrate_kbps,
        play_count: row.get(column(18))?,
        last_played: row.get(column(19))?,
        date_added: row.get(column(20))?,
        path: row.get(column(21))?,
    })
}

/// Summaries of the distinct album IDs that `albums` selects, over each
/// album's visible tracks. Sums that read every track's JSON run for the page only. Albums without a visible track are left out.
/// Returns the count of summarized albums and one page in `order`.
fn albums_page(
    transaction: &Transaction<'_>,
    albums: &str,
    mut values: Vec<Value>,
    order: &str,
    offset: usize,
    limit: usize,
) -> Result<(u64, Vec<AlbumSummary>)> {
    let summary = format!(
        "WITH listed(id) AS ({albums}), summary AS (
         SELECT al.id,al.title,al.album_artist,al.artist_id,al.compilation,MIN(t.year) AS year,
         COUNT(*) AS track_count,MAX(MAX(t.disc_number,t.disc_total,1)) AS disc_count,
         MAX(t.date_added) AS date_added,al.musicbrainz_release_id,
         aa.search AS artist_search,al.title_sort
         FROM listed JOIN library_albums al ON al.id=listed.id
         JOIN library_artists aa ON aa.id=al.artist_id
         JOIN library_tracks t ON t.album_id=al.id {ASSET}
         WHERE {VISIBLE} GROUP BY al.id)"
    );
    let total = transaction.query_row(
        &format!("{summary} SELECT COUNT(*) FROM summary"),
        params_from_iter(values.iter()),
        |row| row.get(0),
    )?;
    let limit_parameter = values.len() + 1;
    let offset_parameter = limit_parameter + 1;
    values.extend([Value::Integer(limit as i64), Value::Integer(offset as i64)]);
    let mut statement = transaction.prepare(&format!(
        "{summary}, page AS (SELECT * FROM summary ORDER BY {order}
         LIMIT ?{limit_parameter} OFFSET ?{offset_parameter})
         SELECT id,title,album_artist,artist_id,compilation,year,track_count,disc_count,
         (SELECT COALESCE(SUM(json_extract(t.metadata,'$.duration_ms')),0)
          FROM library_tracks t {ASSET} WHERE t.album_id=page.id AND {VISIBLE}),
         (SELECT MAX(json_extract(t.metadata,'$.album_gain_db'))
          FROM library_tracks t {ASSET} WHERE t.album_id=page.id AND {VISIBLE}),
         date_added,musicbrainz_release_id
         FROM page ORDER BY {order}"
    ))?;
    let mut entries = statement
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(AlbumSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                album_artist: row.get(2)?,
                album_artist_id: row.get(3)?,
                compilation: row.get(4)?,
                year: row.get(5)?,
                genre: String::new(),
                track_count: row.get(6)?,
                disc_count: row.get(7)?,
                duration_ms: row.get(8)?,
                format: String::new(),
                album_gain_db: row.get(9)?,
                date_added: row.get(10)?,
                musicbrainz_release_id: row.get(11)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    album_votes(transaction, &mut entries)?;
    Ok((total, entries))
}

/// One format class's share of an album. A lossless class is its full
/// `format_label`; a lossy class is the codec alone, so variable bitrates vote
/// together and the album reports their mean.
#[derive(Default)]
struct FormatVote {
    tracks: u64,
    lossy: bool,
    kbps_sum: u64,
    kbps_tracks: u64,
}

/// Fills `format` and `genre` by a vote of each album's visible tracks.
/// Tracks without a format or a genre do not vote. A format class tie is
/// "Mixed"; a genre tie goes to the first genre by name.
fn album_votes(transaction: &Transaction<'_>, albums: &mut [AlbumSummary]) -> Result<()> {
    if albums.is_empty() {
        return Ok(());
    }
    let placeholders: Vec<String> = (1..=albums.len()).map(|n| format!("?{n}")).collect();
    let mut statement = transaction.prepare(&format!(
        "SELECT t.album_id,t.codec,t.bit_depth,COALESCE(json_extract(t.metadata,'$.sample_rate'),0),
         t.bitrate_kbps,g.search,g.name
         FROM library_tracks t {ASSET}
         LEFT JOIN library_track_genres tg ON tg.track_id=t.id AND tg.position=0
         LEFT JOIN library_genres g ON g.id=tg.genre_id
         WHERE {VISIBLE} AND t.album_id IN ({})",
        placeholders.join(",")
    ))?;
    type Votes = (HashMap<String, FormatVote>, HashMap<(String, String), u64>);
    let mut votes: HashMap<u64, Votes> = HashMap::new();
    let mut rows = statement.query(params_from_iter(albums.iter().map(|album| album.id as i64)))?;
    while let Some(row) = rows.next()? {
        let (formats, genres) = votes.entry(row.get(0)?).or_default();
        let codec: Option<String> = row.get(1)?;
        let bitrate: Option<u32> = row.get(4)?;
        let label = format_label(codec.as_deref(), row.get(2)?, row.get(3)?, bitrate);
        if let Some(codec) = codec.filter(|_| !label.is_empty()) {
            let lossy = !is_lossless(&codec);
            let vote = formats
                .entry(if lossy { codec } else { label })
                .or_default();
            vote.tracks += 1;
            vote.lossy = lossy;
            if let Some(kbps) = bitrate.filter(|_| lossy) {
                vote.kbps_sum += u64::from(kbps);
                vote.kbps_tracks += 1;
            }
        }
        if let (Some(search), Some(name)) = (row.get(5)?, row.get(6)?) {
            *genres.entry((search, name)).or_default() += 1;
        }
    }
    for album in albums {
        let Some((formats, genres)) = votes.remove(&album.id) else {
            continue;
        };
        let most = formats.values().map(|vote| vote.tracks).max().unwrap_or(0);
        let mut leaders = formats.into_iter().filter(|(_, vote)| vote.tracks == most);
        album.format = match (leaders.next(), leaders.next()) {
            (Some((class, vote)), None) if vote.lossy && vote.kbps_tracks > 0 => {
                let mean = (vote.kbps_sum + vote.kbps_tracks / 2) / vote.kbps_tracks;
                format!("{class} {mean}")
            }
            (Some((class, _)), None) => class,
            (Some(_), Some(_)) => "Mixed".into(),
            (None, _) => String::new(),
        };
        album.genre = genres
            .into_iter()
            .max_by(|(left, left_count), (right, right_count)| {
                left_count.cmp(right_count).then_with(|| right.cmp(left))
            })
            .map(|((_, name), _)| name)
            .unwrap_or_default();
    }
    Ok(())
}

fn filtered_values(filter: Filter, offset: usize, limit: usize) -> (String, Vec<Value>, usize) {
    let Filter { sql, mut values } = filter;
    let limit_parameter = values.len() + 1;
    values.extend([Value::Integer(limit as i64), Value::Integer(offset as i64)]);
    (sql, values, limit_parameter)
}

/// Album artists of non-compilation albums with a visible track matching the
/// filter, and, given `members`, the track also belonging to that list. The
/// compilations row counts compilation albums the same way.
fn artists(
    transaction: &Transaction<'_>,
    members: Option<CompiledList>,
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<(u64, Vec<NamedCount>, Option<NamedCount>)> {
    let (member_sql, mut member_values) = match members {
        Some(CompiledList { sql, values }) => (
            format!(" AND t.id IN (SELECT track_id FROM ({sql}))"),
            values,
        ),
        None => (String::new(), Vec::new()),
    };
    let filter = Filter::parse(query, member_values.len() + 1)?;
    let filter_count = member_values.len() + filter.values.len();
    let Filter {
        sql: predicate,
        values,
    } = filter;
    member_values.extend(values);
    let limit_parameter = member_values.len() + 1;
    member_values.extend([Value::Integer(limit as i64), Value::Integer(offset as i64)]);
    let values = member_values;
    let filter_values = &values[..filter_count];
    let tracks = format!(
        "FROM library_tracks t JOIN library_albums al ON al.id=t.album_id {ASSET}
         WHERE {VISIBLE} AND ({predicate}){member_sql}"
    );
    let total = transaction.query_row(
        &format!("SELECT COUNT(DISTINCT al.artist_id) {tracks} AND al.compilation=0"),
        params_from_iter(filter_values.iter()),
        |row| row.get(0),
    )?;
    let offset_parameter = limit_parameter + 1;
    let mut statement = transaction.prepare(&format!(
        "SELECT ar.id,ar.name,COUNT(DISTINCT al.id),COUNT(*)
         FROM library_artists ar JOIN library_albums al ON al.artist_id=ar.id AND al.compilation=0
         JOIN library_tracks t ON t.album_id=al.id {ASSET}
         WHERE {VISIBLE} AND ({predicate}){member_sql}
         GROUP BY ar.id ORDER BY ar.search,ar.name,ar.id
         LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}"
    ))?;
    let entries = statement
        .query_map(params_from_iter(values.iter()), named_count)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let compilations = if offset == 0 {
        let (album_count, track_count): (u64, u64) = transaction.query_row(
            &format!("SELECT COUNT(DISTINCT al.id),COUNT(*) {tracks} AND al.compilation=1"),
            params_from_iter(filter_values.iter()),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        (track_count > 0).then(|| NamedCount {
            id: 0,
            name: "Compilations".into(),
            album_count,
            track_count,
        })
    } else {
        None
    };
    Ok((total, entries, compilations))
}

/// Genres at any position of a visible track matching the filter. Genre rows
/// outlive their tracks, so only the track join decides membership.
fn genres(
    transaction: &Transaction<'_>,
    filter: Filter,
    offset: usize,
    limit: usize,
) -> Result<(u64, Vec<NamedCount>)> {
    let filter_count = filter.values.len();
    let (predicate, values, limit_parameter) = filtered_values(filter, offset, limit);
    let source = format!(
        "FROM library_genres g JOIN library_track_genres tg ON tg.genre_id=g.id
         JOIN library_tracks t ON t.id=tg.track_id {ASSET}
         WHERE {VISIBLE} AND ({predicate})"
    );
    let total = transaction.query_row(
        &format!("SELECT COUNT(DISTINCT g.id) {source}"),
        params_from_iter(values[..filter_count].iter()),
        |row| row.get(0),
    )?;
    let offset_parameter = limit_parameter + 1;
    let mut statement = transaction.prepare(&format!(
        "SELECT g.id,g.name,COUNT(DISTINCT t.album_id),COUNT(*) {source}
         GROUP BY g.id ORDER BY g.search,g.name,g.id
         LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}"
    ))?;
    let entries = statement
        .query_map(params_from_iter(values.iter()), named_count)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((total, entries))
}

fn named_count(row: &Row<'_>) -> rusqlite::Result<NamedCount> {
    Ok(NamedCount {
        id: row.get(0)?,
        name: row.get(1)?,
        album_count: row.get(2)?,
        track_count: row.get(3)?,
    })
}

/// A grouped list keeps each group's rows contiguous, so a group's span runs
/// from its first position for its row count.
fn group_spans(
    transaction: &Transaction<'_>,
    CompiledList { sql, values }: CompiledList,
    group: Grouping,
) -> Result<Vec<GroupSpan>> {
    let keys = match group {
        Grouping::Artist => "artist_key",
        Grouping::Album => "album_key",
        Grouping::ArtistAlbum => "artist_key,album_key",
        Grouping::None => unreachable!("ungrouped lists rejected above"),
    };
    let mut statement = transaction.prepare(&format!(
        "SELECT artist_key,album_key,MIN(position)-1,COUNT(*) FROM ({sql})
         GROUP BY {keys} ORDER BY MIN(position) LIMIT {}",
        MAX_GROUPS + 1
    ))?;
    let spans = statement
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(GroupSpan {
                artist_key: row.get(0)?,
                album_key: if group == Grouping::Artist {
                    None
                } else {
                    Some(row.get(1)?)
                },
                start: row.get(2)?,
                count: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if spans.len() > MAX_GROUPS {
        return Err(invalid("list has more than 100000 groups"));
    }
    Ok(spans)
}
