//! One list description for every track list, and its compiler to ordered SQL.
//! The compiled `SELECT` yields `position, row_id, track_id, artist_key,
//! album_key, available`, ordered by `position` (1-based). Callers wrap it as a
//! subquery for pages, counts, totals, group spans and run selections.
use crate::{
    filter::Filter,
    library_types::{LibraryError, LibraryErrorKind},
    store::list_source_rule,
};
use anyhow::Result;
use rusqlite::{Connection, types::Value};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", from = "WireSource")]
pub enum ListSource {
    Library,
    Album { id: u64 },
    Artist { id: u64 },
    AppearsOn { artist_id: u64 },
    Compilations,
    Genre { id: u64 },
    Recent,
    Playlist { id: u64, revision: u64 },
    Smart { id: u64, revision: u64 },
}
/// Serde ignores extra fields on internally tagged unit variants even with
/// `deny_unknown_fields`. Empty struct variants reject them, so decoding goes
/// through this mirror.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireSource {
    Library {},
    Album { id: u64 },
    Artist { id: u64 },
    AppearsOn { artist_id: u64 },
    Compilations {},
    Genre { id: u64 },
    Recent {},
    Playlist { id: u64, revision: u64 },
    Smart { id: u64, revision: u64 },
}
impl From<WireSource> for ListSource {
    fn from(source: WireSource) -> Self {
        match source {
            WireSource::Library {} => Self::Library,
            WireSource::Album { id } => Self::Album { id },
            WireSource::Artist { id } => Self::Artist { id },
            WireSource::AppearsOn { artist_id } => Self::AppearsOn { artist_id },
            WireSource::Compilations {} => Self::Compilations,
            WireSource::Genre { id } => Self::Genre { id },
            WireSource::Recent {} => Self::Recent,
            WireSource::Playlist { id, revision } => Self::Playlist { id, revision },
            WireSource::Smart { id, revision } => Self::Smart { id, revision },
        }
    }
}

/// Column-browser narrowing. Each non-empty dimension filters; dimensions combine with AND.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "snake_case", deny_unknown_fields)]
pub struct Browser {
    pub genre_ids: Vec<u64>,
    pub artist_ids: Vec<u64>,
    pub compilations: bool,
    pub album_ids: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Duration,
    Year,
    Genre,
    Format,
    Plays,
    DateAdded,
    LastPlayed,
    AlbumOrder,
    PlaylistOrder,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SortKey {
    pub field: SortField,
    pub descending: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Grouping {
    None,
    Artist,
    Album,
    ArtistAlbum,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ListDescription {
    pub source: ListSource,
    #[serde(default)]
    pub browser: Browser,
    pub query: String,
    pub sort: Vec<SortKey>,
    pub group: Grouping,
}

/// The Recent source covers tracks added within this many seconds.
pub const RECENT_WINDOW_SECS: i64 = 2_592_000;
/// Request time in UTC seconds, the `now` that `recent_cutoff` counts back from.
pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}
/// The oldest `date_added` the Recent source shows at `now`.
pub(crate) fn recent_cutoff(now: i64) -> i64 {
    now.saturating_sub(RECENT_WINDOW_SECS)
}
/// The most IDs one browser dimension may name.
pub const MAX_BROWSER_IDS: usize = 500;

pub(crate) struct CompiledList {
    pub sql: String,
    pub values: Vec<Value>,
}

/// `t` is the track, `a`/`r` its asset and root, `al` its album, `aa` the album
/// artist, `ta` (added only for the artist sort) the track artist and `s` its play statistics.
const JOINS: &str = "JOIN library_assets a ON a.id=t.asset_id
    JOIN library_roots r ON r.id=a.root_id
    JOIN library_albums al ON al.id=t.album_id
    JOIN library_artists aa ON aa.id=al.artist_id
    LEFT JOIN track_stats s ON s.track_id=t.id";
/// Only the artist sort reads the track artist `ta`; a join per row is wasted otherwise.
const TRACK_ARTIST: &str = " JOIN library_artists ta ON ta.id=t.artist_id";
const VISIBLE: &str = "a.indexed=1 AND a.last_seen_scan>=r.completed_scan";
/// Compilations group under artist key 0, not under their album artist.
const ARTIST_KEY: &str = "CASE WHEN al.compilation=1 THEN 0 ELSE al.artist_id END";
const GENRE: &str = "(SELECT g.search FROM library_track_genres tg
    JOIN library_genres g ON g.id=tg.genre_id WHERE tg.track_id=t.id AND tg.position=0)";
/// Appended to every sort; ends with the row ID, so the order is total. The
/// album rank `ar.rank` stands for album artist, album title and album ID,
/// ranked once over the albums, so each track compares one integer.
const TIEBREAK: [&str; 5] = [
    "ar.rank",
    "t.disc_number",
    "t.track_number",
    "t.title_sort",
    "a.path",
];
/// Ranks every album by album artist, title and ID.
const ALBUM_RANK: &str = "WITH ar(id,rank) AS MATERIALIZED (
    SELECT al.id,ROW_NUMBER() OVER (ORDER BY aa.search,al.title_sort,al.id)
    FROM library_albums al JOIN library_artists aa ON aa.id=al.artist_id)";

fn sort_columns(field: SortField) -> &'static [&'static str] {
    match field {
        SortField::Title => &["t.title_sort"],
        SortField::Artist => &["ta.search"],
        SortField::AlbumArtist => &["aa.search"],
        SortField::Album => &["al.title_sort"],
        SortField::Duration => &["json_extract(t.metadata,'$.duration_ms')"],
        // The unary plus keeps SQLite from walking `library_tracks_year`: a full
        // sort of the window is faster than 50,000 index-ordered row reads.
        SortField::Year => &["+t.year"],
        SortField::Genre => &[GENRE],
        SortField::Format => &[
            "t.codec",
            "t.bit_depth",
            "json_extract(t.metadata,'$.sample_rate')",
            "t.bitrate_kbps",
        ],
        SortField::Plays => &["COALESCE(s.play_count,0)"],
        // Date added 0 is unknown and sorts with NULL, last in both directions.
        SortField::DateAdded => &["NULLIF(t.date_added,0)"],
        SortField::LastPlayed => &["s.last_played"],
        // The tiebreak's album prefix: album rank, disc, track.
        SortField::AlbumOrder => &TIEBREAK[..3],
        SortField::PlaylistOrder => &["e.ordinal"],
    }
}

fn invalid(message: &str) -> anyhow::Error {
    LibraryError {
        kind: LibraryErrorKind::Invalid,
        message: message.into(),
    }
    .into()
}

/// Placeholders are numbered in bind order from the caller's first parameter.
struct Parameters {
    next: usize,
    values: Vec<Value>,
}
impl Parameters {
    fn bind(&mut self, value: Value) -> String {
        let placeholder = format!("?{}", self.next);
        self.next += 1;
        self.values.push(value);
        placeholder
    }
    fn id(&mut self, id: u64) -> Result<String> {
        if id == 0 || id > i64::MAX as u64 {
            return Err(invalid("invalid list source or browser ID"));
        }
        Ok(self.bind(Value::Integer(id as i64)))
    }
    fn ids(&mut self, ids: &[u64]) -> Result<String> {
        let placeholders = ids
            .iter()
            .map(|&id| self.id(id))
            .collect::<Result<Vec<_>>>()?;
        Ok(placeholders.join(","))
    }
    fn filter(&mut self, query: &str) -> Result<String> {
        let filter = Filter::parse(query, self.next)?;
        self.next += filter.values.len();
        self.values.extend(filter.values);
        Ok(filter.sql)
    }
}

/// Compiles `list` to one ordered `SELECT`. `connection` (the caller's open
/// transaction) is read only for the playlist header: its revision and smart rule.
pub(crate) fn compile(
    connection: &Connection,
    list: &ListDescription,
    recent_cutoff: i64,
    first_parameter: usize,
) -> Result<CompiledList> {
    build(connection, list, recent_cutoff, first_parameter, true)
}

/// The rows of `list` without positions or order, for counts, totals and
/// album membership. Yields `row_id, track_id, artist_key, album_key,
/// available`. Sorting every row costs far more than the filter itself.
pub(crate) fn compile_members(
    connection: &Connection,
    list: &ListDescription,
    recent_cutoff: i64,
    first_parameter: usize,
) -> Result<CompiledList> {
    build(connection, list, recent_cutoff, first_parameter, false)
}

fn build(
    connection: &Connection,
    list: &ListDescription,
    recent_cutoff: i64,
    first_parameter: usize,
    ordered: bool,
) -> Result<CompiledList> {
    let browser = &list.browser;
    if [&browser.genre_ids, &browser.artist_ids, &browser.album_ids]
        .iter()
        .any(|ids| ids.len() > MAX_BROWSER_IDS)
    {
        return Err(invalid(&format!(
            "a browser dimension names more than {MAX_BROWSER_IDS} IDs"
        )));
    }
    let mut parameters = Parameters {
        next: first_parameter,
        values: Vec::new(),
    };
    let playlist = matches!(list.source, ListSource::Playlist { .. });
    let mut conditions = Vec::new();
    match &list.source {
        ListSource::Library => {}
        ListSource::Album { id } => conditions.push(format!("t.album_id={}", parameters.id(*id)?)),
        ListSource::Artist { id } => conditions.push(format!(
            "al.compilation=0 AND al.artist_id={}",
            parameters.id(*id)?
        )),
        ListSource::AppearsOn { artist_id } => conditions.push(format!(
            "al.compilation=1 AND t.artist_id={}",
            parameters.id(*artist_id)?
        )),
        ListSource::Compilations => conditions.push("al.compilation=1".into()),
        ListSource::Genre { id } => conditions.push(format!(
            "t.id IN (SELECT track_id FROM library_track_genres WHERE genre_id={})",
            parameters.id(*id)?
        )),
        // Recent membership depends on request time: `recent_cutoff` counts
        // back from each request's `unix_now`. Near the 30-day edge a track can
        // be in one request's Recent list and gone from the next, even at one
        // library revision, so page totals and positions can drift between
        // requests.
        ListSource::Recent => conditions.push(format!(
            "t.date_added>0 AND t.date_added>={}",
            parameters.bind(Value::Integer(recent_cutoff))
        )),
        ListSource::Playlist { id, revision } => {
            list_source_rule(connection, *id, *revision, false)?;
            conditions.push(format!("e.playlist_id={}", parameters.id(*id)?));
        }
        ListSource::Smart { id, revision } => {
            let rule =
                list_source_rule(connection, *id, *revision, true)?.expect("smart playlist rule");
            conditions.push(format!("({})", parameters.filter(&rule)?));
        }
    }
    if !browser.genre_ids.is_empty() {
        conditions.push(format!(
            "t.id IN (SELECT track_id FROM library_track_genres WHERE genre_id IN ({}))",
            parameters.ids(&browser.genre_ids)?
        ));
    }
    match (browser.artist_ids.is_empty(), browser.compilations) {
        (true, false) => {}
        (true, true) => conditions.push("al.compilation=1".into()),
        (false, compilations) => conditions.push(format!(
            "((al.compilation=0 AND al.artist_id IN ({})){})",
            parameters.ids(&browser.artist_ids)?,
            if compilations {
                " OR al.compilation=1"
            } else {
                ""
            }
        )),
    }
    if !browser.album_ids.is_empty() {
        conditions.push(format!(
            "t.album_id IN ({})",
            parameters.ids(&browser.album_ids)?
        ));
    }
    conditions.push(format!("({})", parameters.filter(&list.query)?));

    let default_sort = [SortKey {
        field: match list.source {
            ListSource::Album { .. } | ListSource::Artist { .. } | ListSource::Genre { .. } => {
                SortField::AlbumOrder
            }
            ListSource::Playlist { .. } => SortField::PlaylistOrder,
            ListSource::Recent => SortField::DateAdded,
            _ => SortField::AlbumArtist,
        },
        descending: matches!(list.source, ListSource::Recent),
    }];
    let keys = if list.sort.is_empty() {
        &default_sort[..]
    } else {
        &list.sort[..]
    };
    let mut order = Vec::new();
    for key in keys {
        if key.field == SortField::PlaylistOrder && !playlist {
            return Err(invalid("playlist_order sort requires a playlist source"));
        }
        let direction = if key.descending { "DESC" } else { "ASC" };
        for column in sort_columns(key.field) {
            order.push(format!("{column} {direction} NULLS LAST"));
        }
    }
    let row_id = if playlist { "e.id" } else { "t.id" };
    order.extend(
        TIEBREAK
            .iter()
            .chain([&row_id])
            .map(|column| column.to_string()),
    );
    let order = order.join(",");

    let (from, available) = if playlist {
        (
            format!("FROM playlist_entries e JOIN library_tracks t ON t.id=e.track_id {JOINS}"),
            format!("({VISIBLE})"),
        )
    } else {
        conditions.insert(0, VISIBLE.into());
        (format!("FROM library_tracks t {JOINS}"), "1".into())
    };
    if !ordered {
        return Ok(CompiledList {
            sql: format!(
                "SELECT {row_id} AS row_id,t.id AS track_id,{ARTIST_KEY} AS artist_key,
                 t.album_id AS album_key,{available} AS available {from} WHERE {}",
                conditions.join(" AND ")
            ),
            values: parameters.values,
        });
    }
    // Album blocks retain the column sort only for choosing the block's rank.
    // Carry the fixed within-album keys only when needed; other lists stay narrow.
    let album_columns = if list.group == Grouping::Album {
        ",t.disc_number AS disc_number,NULLIF(t.track_number,0) AS track_number,
         t.title_sort AS title_sort,a.path AS path"
    } else {
        ""
    };
    let rows = format!(
        "SELECT ROW_NUMBER() OVER (ORDER BY {order}) AS row_order,{row_id} AS row_id,
         t.id AS track_id,{ARTIST_KEY} AS artist_key,t.album_id AS album_key,
         {available} AS available{album_columns} {from} JOIN ar ON ar.id=t.album_id{} WHERE {}",
        if order.contains("ta.search") {
            TRACK_ARTIST
        } else {
            ""
        },
        conditions.join(" AND ")
    );
    // Each group takes the position of its first row in the list's sort. Its
    // best value of the first sort key therefore decides its place (the highest
    // for a descending sort, the lowest for an ascending one); later keys and the
    // tiebreak settle ties between groups.
    let groups: &[&str] = match list.group {
        Grouping::None => &[],
        Grouping::Artist => &["artist_key"],
        Grouping::Album => &["album_key"],
        Grouping::ArtistAlbum => &["artist_key", "album_key"],
    };
    let sql = if groups.is_empty() {
        format!(
            "{ALBUM_RANK} SELECT row_order AS position,row_id AS row_id,track_id AS track_id,
             artist_key AS artist_key,album_key AS album_key,available AS available
             FROM ({rows}) ORDER BY position"
        )
    } else {
        // One narrow pass finds each group's first row; joining it back needs
        // no wide window per group key.
        let firsts: String = groups
            .iter()
            .map(|key| {
                format!(
                    ",{key}_first AS MATERIALIZED (SELECT {key} AS {key},MIN(row_order) AS first_row
                     FROM listed GROUP BY {key})"
                )
            })
            .collect();
        let joins: String = groups
            .iter()
            .map(|key| format!(" JOIN {key}_first ON {key}_first.{key}=listed.{key}"))
            .collect();
        let order: String = groups
            .iter()
            .map(|key| format!("{key}_first.first_row,"))
            .collect();
        let within_group = if list.group == Grouping::Album {
            "listed.disc_number,listed.track_number NULLS LAST,
             listed.title_sort,listed.path,listed.row_id"
        } else {
            "listed.row_order"
        };
        format!(
            "{ALBUM_RANK},listed AS MATERIALIZED ({rows}){firsts}
             SELECT ROW_NUMBER() OVER (ORDER BY {order}{within_group}) AS position,
             listed.row_id AS row_id,listed.track_id AS track_id,listed.artist_key AS artist_key,
             listed.album_key AS album_key,listed.available AS available
             FROM listed{joins} ORDER BY position"
        )
    };
    Ok(CompiledList {
        sql,
        values: parameters.values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{protocol::ErrorCode, store::PlaylistFailure};
    use rusqlite::params_from_iter;

    /// Five albums by three album artists plus one compilation (track artists
    /// Coltrane and Davis). Track 7 has `date_added=0`. Track 10 is unavailable:
    /// its asset was last seen before the root's completed scan.
    fn fixture() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::store::migrate(&mut connection, 5).unwrap();
        connection
            .execute_batch(
                r#"INSERT INTO library_roots(id,path,search,completed_scan) VALUES(1,'/m','/m',2);
                INSERT INTO library_artists(id,identity,name,search) VALUES
                    (1,'abba','ABBA','abba'),(2,'beatles','Beatles','beatles'),
                    (3,'coltrane','Coltrane','coltrane'),(4,'various','Various Artists','various artists'),
                    (5,'davis','Davis','davis');
                INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation) VALUES
                    (1,'arrival',1,'Arrival','ABBA','arrival','arrival',0),
                    (2,'help',2,'Help!','Beatles','help','help',0),
                    (3,'abbey',2,'Abbey Road','Beatles','abbey road','abbey road',0),
                    (4,'giant',3,'Giant Steps','Coltrane','giant steps','giant steps',0),
                    (5,'jazz',4,'Jazz Hits','Various Artists','jazz hits','jazz hits',1);
                INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan) VALUES
                    (1,1,'/m/01.flac',1,2),(2,1,'/m/02.flac',1,2),(3,1,'/m/03.flac',1,2),
                    (4,1,'/m/04.flac',1,2),(5,1,'/m/05.flac',1,2),(6,1,'/m/06.mp3',1,2),
                    (7,1,'/m/07.m4a',1,2),(8,1,'/m/08.mp3',1,2),(9,1,'/m/09.m4a',1,2),
                    (10,1,'/m/10.mp3',1,1);
                INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,
                    disc_number,disc_total,track_number,year,codec,bit_depth,bitrate_kbps,date_added) VALUES
                    (1,1,1,1,'{"duration_ms":230000,"sample_rate":44100}','dancing queen','dancing queen',1,1,2,1976,'FLAC',16,900,1000),
                    (2,2,1,1,'{"duration_ms":180000,"sample_rate":44100}','when i kissed the teacher','when i kissed the teacher',1,1,1,1976,'FLAC',16,880,2500),
                    (3,3,2,3,'{"duration_ms":259000,"sample_rate":96000}','come together','come together',1,2,1,1969,'FLAC',24,2500,3000),
                    (4,4,2,3,'{"duration_ms":140000,"sample_rate":96000}','the end','the end',2,2,1,1969,'FLAC',24,2400,3500),
                    (5,5,2,3,'{"duration_ms":182000,"sample_rate":48000}','something','something',1,2,2,1969,'FLAC',24,3000,3000),
                    (6,6,2,2,'{"duration_ms":138000,"sample_rate":44100}','help','help',1,1,1,1965,'MP3',NULL,320,2000),
                    (7,7,3,4,'{"duration_ms":286000,"sample_rate":44100}','giant steps','giant steps',1,1,1,1960,'ALAC',16,1000,0),
                    (8,8,3,5,'{"duration_ms":260000,"sample_rate":44100}','naima','naima',1,1,1,1959,'MP3',NULL,320,4000),
                    (9,9,5,5,'{"duration_ms":null,"sample_rate":44100}','so what','so what',1,1,2,NULL,'AAC',NULL,256,4000),
                    (10,10,2,2,'{"duration_ms":150000,"sample_rate":44100}','the night before','the night before',1,1,2,1965,'MP3',NULL,320,2000);
                INSERT INTO library_genres(id,name,search) VALUES(1,'Pop','pop'),(2,'Rock','rock');
                INSERT INTO library_track_genres(track_id,genre_id,position) VALUES
                    (1,1,0),(2,1,0),(3,2,0),(4,2,0),(5,2,0),(6,2,0),(6,1,1),(8,1,0),(10,2,0);
                INSERT INTO track_stats(track_id,play_count,last_played) VALUES
                    (1,5,500),(3,2,900),(4,1,100),(6,9,1000),(7,3,300),(9,4,700);
                INSERT INTO playlists(id,name,search,revision,query) VALUES
                    (1,'Mix','mix',4,NULL),(2,'Beatles','beatles',2,'albumartist:beatles');
                INSERT INTO playlist_entries(id,playlist_id,track_id,ordinal) VALUES
                    (10,1,6,2),(11,1,10,0),(12,1,1,3),(13,1,6,1);"#,
            )
            .unwrap();
        connection
    }

    fn list(source: ListSource) -> ListDescription {
        ListDescription {
            source,
            browser: Browser::default(),
            query: String::new(),
            sort: Vec::new(),
            group: Grouping::None,
        }
    }
    fn sorted(source: ListSource, field: SortField, descending: bool) -> ListDescription {
        ListDescription {
            sort: vec![SortKey { field, descending }],
            ..list(source)
        }
    }

    /// `(row_id, track_id, artist_key, album_key, available)`.
    type Row = (u64, u64, u64, u64, bool);
    /// Rows in list order. `first_parameter > 1` binds placeholder values for
    /// the caller's own leading parameters.
    fn run(
        connection: &Connection,
        list: &ListDescription,
        cutoff: i64,
        first_parameter: usize,
    ) -> Result<Vec<Row>> {
        let compiled = compile(connection, list, cutoff, first_parameter)?;
        let leading = (1..first_parameter).map(|_| Value::Null);
        let values: Vec<Value> = leading.chain(compiled.values).collect();
        let mut statement = connection.prepare(&format!(
            "SELECT position,row_id,track_id,artist_key,album_key,available FROM ({})",
            compiled.sql
        ))?;
        let rows = statement
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    (
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let positions: Vec<u64> = rows.iter().map(|row| row.0).collect();
        assert_eq!(positions, (1..=rows.len() as u64).collect::<Vec<_>>());
        Ok(rows.into_iter().map(|row| row.1).collect())
    }
    fn row_ids(connection: &Connection, list: &ListDescription) -> Vec<u64> {
        run(connection, list, 0, 1)
            .unwrap()
            .into_iter()
            .map(|row| row.0)
            .collect()
    }
    fn failure_code(error: anyhow::Error) -> ErrorCode {
        if let Some(failure) = error.downcast_ref::<PlaylistFailure>() {
            return failure.code;
        }
        match error.downcast_ref::<LibraryError>() {
            Some(LibraryError {
                kind: LibraryErrorKind::Invalid,
                ..
            }) => ErrorCode::InvalidCommand,
            _ => panic!("unexpected list error: {error:#}"),
        }
    }

    #[test]
    fn unordered_members_match_the_ordered_rows() {
        let connection = fixture();
        for source in [ListSource::Library, ListSource::Compilations] {
            for group in [Grouping::None, Grouping::ArtistAlbum] {
                let list = ListDescription {
                    group,
                    ..list(source.clone())
                };
                let ordered = compile(&connection, &list, 0, 1).unwrap();
                let members = compile_members(&connection, &list, 0, 1).unwrap();
                let mut expected: Vec<(u64, u64, u64, u64, bool)> = connection
                    .prepare(&format!(
                        "SELECT row_id,track_id,artist_key,album_key,available FROM ({})",
                        ordered.sql
                    ))
                    .unwrap()
                    .query_map(params_from_iter(ordered.values.iter()), |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let mut actual: Vec<(u64, u64, u64, u64, bool)> = connection
                    .prepare(&members.sql)
                    .unwrap()
                    .query_map(params_from_iter(members.values.iter()), |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                expected.sort_unstable();
                actual.sort_unstable();
                assert!(!actual.is_empty());
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn library_source_default_order_and_tiebreak() {
        let connection = fixture();
        // Album artist, then album title_sort, then disc and track. Track 10 is unavailable.
        assert_eq!(
            row_ids(&connection, &list(ListSource::Library)),
            [2, 1, 3, 5, 4, 6, 7, 8, 9]
        );
        let rows = run(&connection, &list(ListSource::Library), 0, 1).unwrap();
        assert!(rows.iter().all(|row| row.0 == row.1 && row.4));
        // Album and Artist sources default to album order; disc 2 follows disc 1.
        assert_eq!(
            row_ids(&connection, &list(ListSource::Album { id: 3 })),
            [3, 5, 4]
        );
        assert_eq!(
            row_ids(&connection, &list(ListSource::Artist { id: 2 })),
            [3, 5, 4, 6]
        );
        // Genre matches any genre position.
        assert_eq!(
            row_ids(&connection, &list(ListSource::Genre { id: 1 })),
            [2, 1, 6, 8]
        );
    }

    #[test]
    fn sort_by_each_field_ascending_and_descending() {
        use SortField::*;
        let connection = fixture();
        let cases: [(SortField, [u64; 9], [u64; 9]); 12] = [
            (
                Title,
                [3, 1, 7, 6, 8, 9, 5, 4, 2],
                [2, 4, 5, 9, 8, 6, 7, 1, 3],
            ),
            (
                Artist,
                [2, 1, 3, 5, 4, 6, 7, 8, 9],
                [9, 7, 8, 3, 5, 4, 6, 2, 1],
            ),
            (
                AlbumArtist,
                [2, 1, 3, 5, 4, 6, 7, 8, 9],
                [8, 9, 7, 3, 5, 4, 6, 2, 1],
            ),
            (
                Album,
                [3, 5, 4, 2, 1, 7, 6, 8, 9],
                [8, 9, 6, 7, 2, 1, 3, 5, 4],
            ),
            // Unknown duration, year, genre and last played sort last both ways.
            (
                Duration,
                [6, 4, 2, 5, 1, 3, 8, 7, 9],
                [7, 8, 3, 1, 5, 2, 4, 6, 9],
            ),
            (
                Year,
                [8, 7, 6, 3, 5, 4, 2, 1, 9],
                [2, 1, 3, 5, 4, 6, 7, 8, 9],
            ),
            // Genre is the genre at position 0: track 6 is Rock, not Pop.
            (
                Genre,
                [2, 1, 8, 3, 5, 4, 6, 7, 9],
                [3, 5, 4, 6, 2, 1, 8, 7, 9],
            ),
            // Codec, bit depth, sample rate, then bitrate: 24/48 precedes 24/96
            // although track 5 has the highest bitrate.
            (
                Format,
                [9, 7, 2, 1, 5, 4, 3, 6, 8],
                [6, 8, 3, 4, 5, 1, 2, 7, 9],
            ),
            // Missing track_stats rows count as 0 plays.
            (
                Plays,
                [2, 5, 8, 4, 3, 7, 9, 1, 6],
                [6, 1, 9, 7, 3, 4, 2, 5, 8],
            ),
            // Date added 0 is unknown and sorts last both ways.
            (
                DateAdded,
                [1, 6, 2, 3, 5, 4, 8, 9, 7],
                [8, 9, 4, 3, 5, 2, 6, 1, 7],
            ),
            (
                LastPlayed,
                [4, 7, 1, 9, 3, 6, 2, 5, 8],
                [6, 3, 9, 1, 7, 4, 2, 5, 8],
            ),
            (
                AlbumOrder,
                [2, 1, 3, 5, 4, 6, 7, 8, 9],
                [9, 8, 7, 6, 4, 5, 3, 1, 2],
            ),
        ];
        for (field, ascending, descending) in cases {
            assert_eq!(
                row_ids(&connection, &sorted(ListSource::Library, field, false)),
                ascending,
                "{field:?} ascending"
            );
            assert_eq!(
                row_ids(&connection, &sorted(ListSource::Library, field, true)),
                descending,
                "{field:?} descending"
            );
        }
        // A second explicit key orders ties of the first before the fixed tiebreak.
        let mut list = sorted(ListSource::Library, Year, true);
        list.sort.push(SortKey {
            field: Title,
            descending: false,
        });
        assert_eq!(row_ids(&connection, &list), [1, 2, 3, 5, 4, 6, 7, 8, 9]);
        // Playlist order needs a playlist source.
        let error = compile(
            &connection,
            &sorted(ListSource::Library, PlaylistOrder, false),
            0,
            1,
        )
        .err()
        .unwrap();
        assert_eq!(failure_code(error), ErrorCode::InvalidCommand);
    }

    #[test]
    fn album_grouping_keeps_numbered_order_for_every_column_sort() {
        use SortField::*;
        let connection = fixture();
        for field in [
            Title,
            Artist,
            AlbumArtist,
            Album,
            Duration,
            Year,
            Genre,
            Format,
            Plays,
            DateAdded,
            LastPlayed,
            AlbumOrder,
        ] {
            for descending in [false, true] {
                let mut list = sorted(ListSource::Library, field, descending);
                let ungrouped = run(&connection, &list, 0, 1).unwrap();
                let mut albums = Vec::new();
                for row in ungrouped {
                    if !albums.contains(&row.3) {
                        albums.push(row.3);
                    }
                }
                let expected: Vec<u64> = albums
                    .into_iter()
                    .flat_map(|album| match album {
                        1 => vec![2, 1],
                        2 => vec![6],
                        3 => vec![3, 5, 4],
                        4 => vec![7],
                        5 => vec![8, 9],
                        _ => unreachable!(),
                    })
                    .collect();
                list.group = Grouping::Album;
                assert_eq!(
                    row_ids(&connection, &list),
                    expected,
                    "{field:?} descending={descending}"
                );
            }
        }
    }

    #[test]
    fn album_grouping_places_unnumbered_tracks_last_with_stable_fallbacks() {
        let connection = fixture();
        connection
            .execute_batch(
                "UPDATE library_tracks SET disc_number=1 WHERE album_id=3;
             UPDATE library_tracks SET track_number=0,title_sort='same' WHERE id IN (3,5);",
            )
            .unwrap();
        for descending in [false, true] {
            let mut list = sorted(
                ListSource::Album { id: 3 },
                SortField::DateAdded,
                descending,
            );
            list.group = Grouping::Album;
            assert_eq!(row_ids(&connection, &list), [4, 3, 5]);
        }
        connection
            .execute("UPDATE library_tracks SET title_sort='zulu' WHERE id=3", [])
            .unwrap();
        assert_eq!(
            row_ids(
                &connection,
                &list_of(ListSource::Album { id: 3 }, Grouping::Album)
            ),
            [4, 5, 3]
        );
    }

    #[test]
    fn album_grouping_preserves_playlist_occurrences_in_numbered_order() {
        let connection = fixture();
        for (descending, expected) in [(false, vec![10, 13, 11, 12]), (true, vec![12, 10, 13, 11])]
        {
            let mut list = sorted(
                ListSource::Playlist { id: 1, revision: 4 },
                SortField::PlaylistOrder,
                descending,
            );
            list.group = Grouping::Album;
            assert_eq!(row_ids(&connection, &list), expected);
        }
    }

    #[test]
    fn album_grouping_orders_groups_by_newest_track_when_sorted_by_date_added_desc() {
        let connection = fixture();
        let mut list = sorted(ListSource::Library, SortField::DateAdded, true);
        assert_eq!(row_ids(&connection, &list), [8, 9, 4, 3, 5, 2, 6, 1, 7]);
        list.group = Grouping::Album;
        // Arrival's newest track (2500) beats Help (2000), so track 1 moves up with its album.
        assert_eq!(row_ids(&connection, &list), [8, 9, 3, 5, 4, 2, 1, 6, 7]);
        // Ascending: each album takes its oldest known track's value. Giant
        // Steps has only an unknown date (0), so it comes last.
        let mut list = sorted(ListSource::Library, SortField::DateAdded, false);
        assert_eq!(row_ids(&connection, &list), [1, 6, 2, 3, 5, 4, 8, 9, 7]);
        list.group = Grouping::Album;
        assert_eq!(row_ids(&connection, &list), [2, 1, 6, 3, 5, 4, 8, 9, 7]);
        // Recent defaults to date added descending.
        let recent = |group| {
            run(&connection, &list_of(ListSource::Recent, group), 1000, 1)
                .unwrap()
                .into_iter()
                .map(|row| row.0)
                .collect::<Vec<_>>()
        };
        assert_eq!(recent(Grouping::None), [8, 9, 4, 3, 5, 2, 6, 1]);
        assert_eq!(recent(Grouping::Album), [8, 9, 3, 5, 4, 2, 1, 6]);
    }
    fn list_of(source: ListSource, group: Grouping) -> ListDescription {
        ListDescription {
            group,
            ..list(source)
        }
    }

    #[test]
    fn artist_album_grouping_puts_compilations_under_key_zero() {
        let connection = fixture();
        let mut list = sorted(ListSource::Library, SortField::Title, false);
        list.group = Grouping::ArtistAlbum;
        let rows: Vec<(u64, u64, u64)> = run(&connection, &list, 0, 1)
            .unwrap()
            .into_iter()
            .map(|row| (row.0, row.2, row.3))
            .collect();
        // Beatles holds the first title, then ABBA, Coltrane, then the compilation
        // (both track artists) under artist key 0. Albums follow their first title.
        assert_eq!(
            rows,
            [
                (3, 2, 3),
                (5, 2, 3),
                (4, 2, 3),
                (6, 2, 2),
                (1, 1, 1),
                (2, 1, 1),
                (7, 3, 4),
                (8, 0, 5),
                (9, 0, 5),
            ]
        );
        list.group = Grouping::Artist;
        assert_eq!(row_ids(&connection, &list), [3, 6, 5, 4, 1, 2, 7, 8, 9]);
        // Both Beatles albums tie on the sort value (album artist). The tie goes
        // to the rest of the sort (Abbey Road before Help by title), not to the
        // album ID (Help is album 2).
        let rows: Vec<(u64, u64)> = run(
            &connection,
            &list_of(ListSource::Library, Grouping::ArtistAlbum),
            0,
            1,
        )
        .unwrap()
        .into_iter()
        .map(|row| (row.0, row.3))
        .collect();
        assert_eq!(
            rows,
            [
                (2, 1),
                (1, 1),
                (3, 3),
                (5, 3),
                (4, 3),
                (6, 2),
                (7, 4),
                (8, 5),
                (9, 5)
            ]
        );
    }

    #[test]
    fn appears_on_lists_compilation_tracks_by_track_artist() {
        let connection = fixture();
        let appears_on = |artist_id| {
            run(
                &connection,
                &list(ListSource::AppearsOn { artist_id }),
                0,
                1,
            )
            .unwrap()
            .into_iter()
            .map(|row| (row.0, row.2))
            .collect::<Vec<_>>()
        };
        assert_eq!(appears_on(3), [(8, 0)]);
        assert_eq!(appears_on(5), [(9, 0)]);
        assert_eq!(appears_on(2), []);
        // Artist lists only non-compilation albums by album artist.
        assert_eq!(
            row_ids(&connection, &list(ListSource::Artist { id: 3 })),
            [7]
        );
        assert_eq!(
            row_ids(&connection, &list(ListSource::Artist { id: 4 })),
            [] as [u64; 0]
        );
        assert_eq!(
            row_ids(&connection, &list(ListSource::Compilations)),
            [8, 9]
        );
    }

    #[test]
    fn browser_dimensions_and_query_combine_with_and() {
        let connection = fixture();
        let browse = |browser: Browser, query: &str| {
            row_ids(
                &connection,
                &ListDescription {
                    browser,
                    query: query.into(),
                    ..list(ListSource::Library)
                },
            )
        };
        let genres = |genre_ids: &[u64]| Browser {
            genre_ids: genre_ids.to_vec(),
            ..Browser::default()
        };
        assert_eq!(browse(genres(&[2]), ""), [3, 5, 4, 6]);
        assert_eq!(browse(genres(&[1, 2]), ""), [2, 1, 3, 5, 4, 6, 8]);
        let artists = |artist_ids: &[u64], compilations| Browser {
            artist_ids: artist_ids.to_vec(),
            compilations,
            ..Browser::default()
        };
        // Album artist dimension: Coltrane's compilation track is not his album.
        assert_eq!(browse(artists(&[1, 3], false), ""), [2, 1, 7]);
        assert_eq!(browse(artists(&[2], true), ""), [3, 5, 4, 6, 8, 9]);
        assert_eq!(browse(artists(&[], true), ""), [8, 9]);
        assert_eq!(
            browse(
                Browser {
                    genre_ids: vec![1],
                    ..artists(&[2], false)
                },
                ""
            ),
            [6]
        );
        assert_eq!(
            browse(
                Browser {
                    genre_ids: vec![1],
                    ..artists(&[], true)
                },
                ""
            ),
            [8]
        );
        let albums = Browser {
            album_ids: vec![3, 2],
            ..Browser::default()
        };
        assert_eq!(browse(albums.clone(), ""), [3, 5, 4, 6]);
        assert_eq!(browse(albums, "title:the"), [3, 4]);
        assert_eq!(browse(Browser::default(), "title:the"), [2, 3, 4]);
        // Invalid IDs are rejected rather than matching nothing.
        let error = compile(
            &connection,
            &ListDescription {
                browser: genres(&[0]),
                ..list(ListSource::Library)
            },
            0,
            1,
        )
        .err()
        .unwrap();
        assert_eq!(failure_code(error), ErrorCode::InvalidCommand);
    }

    #[test]
    fn source_browser_and_query_combine_after_leading_parameters() {
        let connection = fixture();
        let list = ListDescription {
            browser: Browser {
                artist_ids: vec![2, 3],
                ..Browser::default()
            },
            query: "title:the".into(),
            ..list(ListSource::Genre { id: 2 })
        };
        // Rock tracks by Beatles or Coltrane albums whose title contains "the".
        assert_eq!(
            run(&connection, &list, 0, 4).unwrap(),
            [(3, 3, 2, 3, true), (4, 4, 2, 3, true)]
        );
        let compiled = compile(&connection, &list, 0, 4).unwrap();
        assert_eq!(compiled.values.len(), 4);
        assert!(compiled.sql.contains("?7") && !compiled.sql.contains("?3"));
    }

    #[test]
    fn recent_excludes_unknown_and_old_date_added() {
        let connection = fixture();
        let recent = |cutoff| {
            run(&connection, &list(ListSource::Recent), cutoff, 1)
                .unwrap()
                .into_iter()
                .map(|row| row.0)
                .collect::<Vec<_>>()
        };
        // The cutoff is inclusive. Date added 0 (unknown) never counts as recent.
        assert_eq!(recent(2500), [8, 9, 4, 3, 5, 2]);
        assert_eq!(recent(0), [8, 9, 4, 3, 5, 2, 6, 1]);
        assert_eq!(recent(i64::MIN), [8, 9, 4, 3, 5, 2, 6, 1]);
        assert_eq!(recent(4001), [] as [u64; 0]);
    }

    #[test]
    fn playlist_source_keeps_duplicates_unavailable_rows_and_checks_revision() {
        let connection = fixture();
        let playlist = ListSource::Playlist { id: 1, revision: 4 };
        // Occurrences in playlist order, including the unavailable track 10.
        assert_eq!(
            run(&connection, &list(playlist.clone()), 0, 1).unwrap(),
            [
                (11, 10, 2, 2, false),
                (13, 6, 2, 2, true),
                (10, 6, 2, 2, true),
                (12, 1, 1, 1, true),
            ]
        );
        // Sorted, duplicate occurrences tie on everything but row_id.
        assert_eq!(
            row_ids(
                &connection,
                &sorted(playlist.clone(), SortField::Title, false)
            ),
            [12, 10, 13, 11]
        );
        assert_eq!(
            row_ids(
                &connection,
                &sorted(playlist.clone(), SortField::PlaylistOrder, true)
            ),
            [12, 10, 13, 11]
        );
        assert_eq!(
            row_ids(
                &connection,
                &ListDescription {
                    query: "title:help".into(),
                    ..list(playlist)
                }
            ),
            [13, 10]
        );
        let code = |source| failure_code(compile(&connection, &list(source), 0, 1).err().unwrap());
        assert_eq!(
            code(ListSource::Playlist { id: 1, revision: 3 }),
            ErrorCode::StalePlaylistRevision
        );
        assert_eq!(
            code(ListSource::Playlist {
                id: 99,
                revision: 1
            }),
            ErrorCode::InvalidCommand
        );
        assert_eq!(
            code(ListSource::Playlist { id: 2, revision: 2 }),
            ErrorCode::InvalidCommand
        );
        assert_eq!(
            code(ListSource::Smart { id: 1, revision: 4 }),
            ErrorCode::InvalidCommand
        );
    }

    #[test]
    fn smart_source_applies_rule_and_query() {
        let connection = fixture();
        let smart = ListSource::Smart { id: 2, revision: 2 };
        // The rule matches the unavailable track 10 too; visibility still applies.
        assert_eq!(row_ids(&connection, &list(smart.clone())), [3, 5, 4, 6]);
        let narrowed = ListDescription {
            query: "year:1969".into(),
            ..list(smart.clone())
        };
        assert_eq!(row_ids(&connection, &narrowed), [3, 5, 4]);
        // Placeholders start at the caller's first parameter.
        let rows: Vec<u64> = run(&connection, &narrowed, 0, 4)
            .unwrap()
            .into_iter()
            .map(|row| row.0)
            .collect();
        assert_eq!(rows, [3, 5, 4]);
        let compiled = compile(&connection, &narrowed, 0, 4).unwrap();
        assert!(compiled.sql.contains("?4") && !compiled.sql.contains("?3"));
        let error = compile(
            &connection,
            &list(ListSource::Smart { id: 2, revision: 1 }),
            0,
            1,
        )
        .err()
        .unwrap();
        assert_eq!(failure_code(error), ErrorCode::StalePlaylistRevision);
    }

    #[test]
    fn list_description_wire_shape_round_trips() {
        let sources = [
            (r#"{"kind":"library"}"#, ListSource::Library),
            (r#"{"kind":"album","id":3}"#, ListSource::Album { id: 3 }),
            (r#"{"kind":"artist","id":2}"#, ListSource::Artist { id: 2 }),
            (
                r#"{"kind":"appears_on","artist_id":5}"#,
                ListSource::AppearsOn { artist_id: 5 },
            ),
            (r#"{"kind":"compilations"}"#, ListSource::Compilations),
            (r#"{"kind":"genre","id":1}"#, ListSource::Genre { id: 1 }),
            (r#"{"kind":"recent"}"#, ListSource::Recent),
            (
                r#"{"kind":"playlist","id":1,"revision":4}"#,
                ListSource::Playlist { id: 1, revision: 4 },
            ),
            (
                r#"{"kind":"smart","id":2,"revision":2}"#,
                ListSource::Smart { id: 2, revision: 2 },
            ),
        ];
        for (source_json, source) in sources {
            let text = format!(
                r#"{{"source":{source_json},"browser":{{"genre_ids":[1],"artist_ids":[2,3],"compilations":true,"album_ids":[]}},
                "query":"year:1969","sort":[{{"field":"date_added","descending":true}},{{"field":"album_artist","descending":false}}],
                "group":"artist_album"}}"#
            );
            let parsed: ListDescription = serde_json::from_str(&text).unwrap();
            assert_eq!(
                parsed,
                ListDescription {
                    source,
                    browser: Browser {
                        genre_ids: vec![1],
                        artist_ids: vec![2, 3],
                        compilations: true,
                        album_ids: vec![],
                    },
                    query: "year:1969".into(),
                    sort: vec![
                        SortKey {
                            field: SortField::DateAdded,
                            descending: true
                        },
                        SortKey {
                            field: SortField::AlbumArtist,
                            descending: false
                        },
                    ],
                    group: Grouping::ArtistAlbum,
                }
            );
            let expected: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(serde_json::to_value(&parsed).unwrap(), expected);
        }
        let fields = [
            "title",
            "artist",
            "album_artist",
            "album",
            "duration",
            "year",
            "genre",
            "format",
            "plays",
            "date_added",
            "last_played",
            "album_order",
            "playlist_order",
        ];
        for field in fields {
            let key: SortKey =
                serde_json::from_str(&format!(r#"{{"field":"{field}","descending":false}}"#))
                    .unwrap();
            assert_eq!(serde_json::to_value(key).unwrap()["field"], field);
        }
        for group in ["none", "artist", "album", "artist_album"] {
            let parsed: Grouping = serde_json::from_str(&format!(r#""{group}""#)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), group);
        }
        // Browser is optional and its dimensions default to empty.
        let minimal: ListDescription = serde_json::from_str(
            r#"{"source":{"kind":"library"},"query":"","sort":[],"group":"none"}"#,
        )
        .unwrap();
        assert_eq!(minimal, list(ListSource::Library));
        let partial: Browser = serde_json::from_str(r#"{"album_ids":[4]}"#).unwrap();
        assert_eq!(partial.album_ids, [4]);
        // Unknown fields are rejected at every level, including unit source kinds.
        for rejected in [
            r#"{"source":{"kind":"library"},"query":"","sort":[],"group":"none","extra":1}"#,
            r#"{"source":{"kind":"library","id":1},"query":"","sort":[],"group":"none"}"#,
            r#"{"source":{"kind":"album","id":1,"extra":1},"query":"","sort":[],"group":"none"}"#,
            r#"{"source":{"kind":"playlist","id":1},"query":"","sort":[],"group":"none"}"#,
            r#"{"source":{"kind":"library"},"browser":{"years":[]},"query":"","sort":[],"group":"none"}"#,
            r#"{"source":{"kind":"library"},"query":"","sort":[{"field":"title","descending":false,"x":1}],"group":"none"}"#,
            r#"{"source":{"kind":"library"},"query":"","sort":[{"field":"title"}],"group":"none"}"#,
            r#"{"source":{"kind":"songs"},"query":"","sort":[],"group":"none"}"#,
            r#"{"source":{"kind":"library"},"sort":[],"group":"none"}"#,
        ] {
            assert!(
                serde_json::from_str::<ListDescription>(rejected).is_err(),
                "accepted {rejected}"
            );
        }
    }
}
