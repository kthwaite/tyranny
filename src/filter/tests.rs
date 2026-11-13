//! Independent search model: persisted fields are already Unicode-lowercased, as
//! in library ingestion. Literal matching is substring containment; fuzzy matching
//! splits that normalized text on non-alphanumeric scalars and uses full OSA DP.
//! The oracle never calls the production tokenizer, distance, or SQL helpers.
//! Proptest's seed reporting and failure persistence retain reproducible failures.
//! These tests cover query semantics and index state, not planner/performance or
//! the full library ingestion schema. Generated ASTs intentionally remain valid.

use super::Filter;
use proptest::prelude::*;
use rusqlite::{Connection, params, params_from_iter, types::Value};

const ARTISTS: &[&str] = &[
    "radiohead",
    "björk",
    "massive attack",
    "o'brien",
    "東京音楽",
];
const ALBUMS: &[&str] = &["in rainbows", "debut", "mezzanine", "100%_live", "夜明音楽"];
const GENRES: &[&str] = &[
    "alternative",
    "électronique",
    "trip-hop",
    "rock",
    "日本音楽",
];

#[derive(Clone, Debug)]
struct Row {
    id: i64,
    search: String,
    title: String,
    artist: usize,
    album: usize,
    genre: Option<usize>,
    year: Option<u16>,
    liked: bool,
}

fn row(id: i64, search: &str, title: &str) -> Row {
    Row {
        id,
        search: search.to_lowercase(),
        title: title.to_lowercase(),
        artist: 0,
        album: 0,
        genre: Some(0),
        year: Some(2007),
        liked: false,
    }
}

fn schema(db: &Connection) {
    db.execute_batch(
        "CREATE TABLE library_tracks (
            id INTEGER PRIMARY KEY, search TEXT NOT NULL, title_sort TEXT NOT NULL,
            artist_id INTEGER NOT NULL, album_id INTEGER NOT NULL, year INTEGER,
            liked INTEGER NOT NULL);
         CREATE TABLE library_artists (id INTEGER PRIMARY KEY, search TEXT NOT NULL);
         CREATE TABLE library_albums (
            id INTEGER PRIMARY KEY, artist_id INTEGER NOT NULL, title_sort TEXT NOT NULL);
         CREATE TABLE library_genres (id INTEGER PRIMARY KEY, search TEXT NOT NULL);
         CREATE TABLE library_track_genres (
            track_id INTEGER NOT NULL, genre_id INTEGER NOT NULL,
            PRIMARY KEY(track_id,genre_id));",
    )
    .unwrap();
    for (index, artist) in ARTISTS.iter().enumerate() {
        db.execute(
            "INSERT INTO library_artists VALUES(?1,?2)",
            params![index as i64, artist],
        )
        .unwrap();
    }
    for (index, album) in ALBUMS.iter().enumerate() {
        // Deliberately different from the track artist to expose scope leakage.
        db.execute(
            "INSERT INTO library_albums VALUES(?1,?2,?3)",
            params![index as i64, ((index + 1) % ARTISTS.len()) as i64, album],
        )
        .unwrap();
    }
    for (index, genre) in GENRES.iter().enumerate() {
        db.execute(
            "INSERT INTO library_genres VALUES(?1,?2)",
            params![index as i64, genre],
        )
        .unwrap();
    }
}

fn upsert(db: &Connection, item: &Row) {
    db.execute(
        "INSERT INTO library_tracks VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(id) DO UPDATE SET search=excluded.search,title_sort=excluded.title_sort,
         artist_id=excluded.artist_id,album_id=excluded.album_id,year=excluded.year,liked=excluded.liked",
        params![item.id, item.search, item.title, item.artist as i64, item.album as i64,
            item.year, item.liked],
    ).unwrap();
    db.execute(
        "DELETE FROM library_track_genres WHERE track_id=?1",
        [item.id],
    )
    .unwrap();
    if let Some(genre) = item.genre {
        db.execute(
            "INSERT INTO library_track_genres VALUES(?1,?2)",
            params![item.id, genre as i64],
        )
        .unwrap();
    }
}

fn delete(db: &Connection, id: i64) {
    db.execute("DELETE FROM library_track_genres WHERE track_id=?1", [id])
        .unwrap();
    db.execute("DELETE FROM library_tracks WHERE id=?1", [id])
        .unwrap();
}

fn migrate(db: &mut Connection) {
    let transaction = db.transaction().unwrap();
    crate::filter::search::migrate(&transaction).unwrap();
    transaction.commit().unwrap();
}

fn fixture(rows: &[Row]) -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    crate::filter::search::register(&db).unwrap();
    schema(&db);
    // Existing content must be indexed by the migration, not just future triggers.
    for item in rows {
        upsert(&db, item);
    }
    migrate(&mut db);
    db
}

fn sql_ids(db: &Connection, query: &str, first_parameter: usize) -> Vec<i64> {
    let filter = Filter::parse(query, first_parameter)
        .unwrap_or_else(|error| panic!("valid generated query {query:?} failed to parse: {error}"));
    let mut values = vec![Value::Integer(17); first_parameter - 1];
    values.extend(filter.values);
    let prefix = (1..first_parameter)
        .map(|index| format!("?{index}=17 AND "))
        .collect::<String>();
    let sql = format!(
        "SELECT t.id FROM library_tracks t WHERE {prefix}({}) ORDER BY t.id",
        filter.sql
    );
    let mut statement = db.prepare(&sql).unwrap();
    statement
        .query_map(params_from_iter(values), |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

// Full matrix optimal-string-alignment distance, intentionally not the bounded
// production algorithm. Adjacent transposition has cost one, including Unicode.
fn osa(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut distance = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (i, row) in distance.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=right.len() {
        distance[0][j] = j;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            distance[i][j] = (distance[i - 1][j] + 1)
                .min(distance[i][j - 1] + 1)
                .min(distance[i - 1][j - 1] + usize::from(left[i - 1] != right[j - 1]));
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                distance[i][j] = distance[i][j].min(distance[i - 2][j - 2] + 1);
            }
        }
    }
    distance[left.len()][right.len()]
}

fn text_matches(text: &str, value: &str, quoted: bool) -> bool {
    let value = value.to_lowercase();
    text.contains(&value)
        || (!quoted
            && (4..=64).contains(&value.chars().count())
            && value.chars().all(char::is_alphabetic)
            && text
                .split(|ch: char| !ch.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .any(|word| osa(word, &value) <= 1))
}

#[derive(Clone, Copy, Debug)]
enum Scope {
    Any,
    Title,
    Artist,
    Album,
    AlbumArtist,
    Genre,
}

#[derive(Clone, Debug)]
enum Query {
    Text(Scope, String, bool),
    Year(u16, u16),
    Liked(bool),
    Not(Box<Query>),
    And(Box<Query>, Box<Query>),
    Or(Box<Query>, Box<Query>),
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Query {
    fn render(&self) -> String {
        match self {
            Self::Text(scope, value, quoted) => {
                let prefix = match scope {
                    Scope::Any => "",
                    Scope::Title => "title:",
                    Scope::Artist => "artist:",
                    Scope::Album => "album:",
                    Scope::AlbumArtist => "albumartist:",
                    Scope::Genre => "genre:",
                };
                format!(
                    "{prefix}{}",
                    if *quoted { quote(value) } else { value.clone() }
                )
            }
            Self::Year(low, high) => format!("year:{low:04}..{high:04}"),
            Self::Liked(value) => format!("liked:{value}"),
            Self::Not(value) => format!("NOT ({})", value.render()),
            Self::And(left, right) => format!("({}) AND ({})", left.render(), right.render()),
            Self::Or(left, right) => format!("({}) OR ({})", left.render(), right.render()),
        }
    }

    fn matches(&self, item: &Row) -> bool {
        match self {
            Self::Text(scope, value, quoted) => {
                let text = match scope {
                    Scope::Any => &item.search,
                    Scope::Title => &item.title,
                    Scope::Artist => ARTISTS[item.artist],
                    Scope::Album => ALBUMS[item.album],
                    Scope::AlbumArtist => ARTISTS[(item.album + 1) % ARTISTS.len()],
                    Scope::Genre => match item.genre {
                        Some(index) => GENRES[index],
                        None => return false,
                    },
                };
                text_matches(text, value, *quoted)
            }
            Self::Year(low, high) => item.year.is_some_and(|year| (*low..=*high).contains(&year)),
            Self::Liked(value) => item.liked == *value,
            Self::Not(value) => !value.matches(item),
            Self::And(left, right) => left.matches(item) && right.matches(item),
            Self::Or(left, right) => left.matches(item) || right.matches(item),
        }
    }
}

fn compare(db: &Connection, rows: &[Row], query: &Query, first: usize) {
    let mut expected: Vec<_> = rows
        .iter()
        .filter(|row| query.matches(row))
        .map(|row| row.id)
        .collect();
    expected.sort_unstable();
    let rendered = query.render();
    assert_eq!(
        sql_ids(db, &rendered, first),
        expected,
        "query={rendered:?}, first_parameter={first}, rows={rows:?}"
    );
}

fn text_strategy() -> BoxedStrategy<String> {
    prop_oneof![
        5 => prop::sample::select(vec!["radiohead", "Radiohead live", "radiohaed", "night light",
            "björk", "Björk lumière", "東京音楽", "夜明音楽", "in rainbows", "massive attack",
            "100%_live", "o'brien", "double\"quote", "back\\slash", "x'); DROP TABLE library_tracks;--",
            "trip-hop", "électronique", "école", "e\u{301}cole", "1234", "abc", "", "prefix-radiohead_suffix"])
            .prop_map(|value| value.to_lowercase()),
        1 => "[a-z]{1,12}".prop_map(|value| value),
    ].boxed()
}

fn row_strategy() -> BoxedStrategy<Row> {
    (
        1i64..7,
        text_strategy(),
        text_strategy(),
        0usize..ARTISTS.len(),
        0usize..ALBUMS.len(),
        prop::option::of(0usize..GENRES.len()),
        prop::option::of(1950u16..2030),
        any::<bool>(),
    )
        .prop_map(
            |(id, search, title, artist, album, genre, year, liked)| Row {
                id,
                search,
                title,
                artist,
                album,
                genre,
                year,
                liked,
            },
        )
        .boxed()
}

fn scope_strategy() -> impl Strategy<Value = Scope> {
    prop::sample::select(vec![
        Scope::Any,
        Scope::Title,
        Scope::Artist,
        Scope::Album,
        Scope::AlbumArtist,
        Scope::Genre,
    ])
}

fn query_strategy() -> BoxedStrategy<Query> {
    let term = prop_oneof![
        (
            scope_strategy(),
            prop::sample::select(vec![
                "radiohaed",
                "RADIOHEAD",
                "björk",
                "björ",
                "massvie",
                "rainbws",
                "mezzanien",
                "alternatvie",
                "électronqiue",
                "東京音楽",
                "東京楽音",
                "rock",
                "1234",
                "abc",
                "100%_live",
                "o'brien",
                "trip-hop",
                "%",
                "_",
                "radioheadx"
            ]),
            any::<bool>()
        )
            .prop_map(|(scope, value, quoted)| Query::Text(
                scope,
                value.to_owned(),
                quoted
            )),
        (scope_strategy(), text_strategy())
            .prop_filter("quoted atoms cannot be empty", |(_, text)| !text.is_empty())
            .prop_map(|(scope, value)| Query::Text(scope, value, true)),
        (1950u16..2020, 0u16..10).prop_map(|(low, width)| Query::Year(low, low + width)),
        any::<bool>().prop_map(Query::Liked),
    ];
    term.prop_recursive(3, 12, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|query| Query::Not(Box::new(query))),
            (inner.clone(), inner.clone())
                .prop_map(|(left, right)| Query::And(Box::new(left), Box::new(right))),
            (inner.clone(), inner)
                .prop_map(|(left, right)| Query::Or(Box::new(left), Box::new(right))),
        ]
    })
    .boxed()
}

fn unique_rows(rows: Vec<Row>) -> Vec<Row> {
    let mut model = Vec::new();
    for row in rows {
        model_upsert(&mut model, row);
    }
    model
}

fn model_upsert(rows: &mut Vec<Row>, item: Row) {
    rows.retain(|row| row.id != item.id);
    rows.push(item);
}

// Select actual FTS indexes, excluding fts5vocab virtual tables and shadow tables.
fn rebuild(db: &Connection) {
    let names: Vec<String> = db
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type='table' AND lower(sql) LIKE '%using fts5(%'",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(!names.is_empty(), "migration must create FTS indexes");
    for name in names {
        let identifier = format!("\"{}\"", name.replace('"', "\"\""));
        db.execute_batch(&format!(
            "INSERT INTO {identifier}({identifier}) VALUES('rebuild');"
        ))
        .unwrap();
    }
}

#[derive(Clone, Debug)]
enum Operation {
    Upsert(Row),
    Delete(i64),
    Rollback(Row, i64),
    Query(Query),
    Rebuild,
}

fn operation_strategy() -> impl Strategy<Value = Operation> {
    prop_oneof![
        5 => row_strategy().prop_map(Operation::Upsert),
        2 => (1i64..7).prop_map(Operation::Delete),
        2 => (row_strategy(), 1i64..7).prop_map(|(row, id)| Operation::Rollback(row, id)),
        3 => query_strategy().prop_map(Operation::Query),
        1 => Just(Operation::Rebuild),
    ]
}

fn probes(rows: &[Row]) -> Vec<Query> {
    let mut queries = vec![
        Query::Text(Scope::Any, "radiohaed".into(), false),
        Query::Text(Scope::Title, "radiohaed".into(), false),
        Query::Not(Box::new(Query::Text(Scope::Any, "radiohaed".into(), false))),
        Query::Text(Scope::Any, "東京楽音".into(), false),
    ];
    for row in rows {
        for (scope, text) in [(Scope::Any, &row.search), (Scope::Title, &row.title)] {
            if !text.is_empty() {
                queries.push(Query::Text(scope, text.clone(), true));
            }
        }
    }
    queries
}

proptest! {

    #[test]
    fn one_edit_and_boundary_terms_match_independent_distance(
        length in prop::sample::select(vec![3usize, 4, 5, 6, 7, 8, 63, 64, 65]),
        alphabet in prop::sample::select(vec![vec!['a', 'b', 'c'], vec!['é', 'ö', '界']]),
        edit in 0usize..4, position in 0usize..80, first in 1usize..6,
    ) {
        let word: String = (0..length).map(|i| alphabet[i % alphabet.len()]).collect();
        let mut changed: Vec<char> = word.chars().collect();
        let at = position % changed.len();
        match edit {
            0 => { changed.insert(at, 'z'); }
            1 => { changed.remove(at); }
            2 => { changed[at] = 'z'; }
            _ => { let next = (at + 1).min(changed.len() - 1); changed.swap(at, next); }
        }
        let changed: String = changed.into_iter().collect();
        prop_assert!(osa(&word, &changed) <= 1);
        let rows = vec![row(1, &format!("start/{word}.flac"), &word),
            row(2, &format!("prefix{word}suffix"), "unrelated"),
            row(3, &changed, &changed)];
        let db = fixture(&rows);
        for scope in [Scope::Any, Scope::Title] {
            for quoted in [false, true] {
                compare(&db, &rows, &Query::Text(scope, changed.clone(), quoted), first);
                // Reverse direction distinguishes insertion from deletion.
                compare(&db, &rows, &Query::Text(scope, word.clone(), quoted), first);
            }
        }
    }

    #[test]
    fn boolean_and_field_queries_match_vec_model(
        rows in prop::collection::vec(row_strategy(), 0..14),
        queries in prop::collection::vec(query_strategy(), 1..9), first in 1usize..7,
    ) {
        let rows = unique_rows(rows);
        let db = fixture(&rows);
        for query in queries { compare(&db, &rows, &query, first); }
    }

    #[test]
    fn index_tracks_mutations_and_rollbacks(
        initial in prop::collection::vec(row_strategy(), 1..9),
        operations in prop::collection::vec(operation_strategy(), 1..25), first in 1usize..5,
    ) {
        let mut rows = unique_rows(initial);
        let mut db = fixture(&rows);
        for operation in operations {
            let mut observations = probes(&rows);
            // Warm query/index paths before changing the same logical contents.
            for query in &observations { compare(&db, &rows, query, first); }
            match operation {
                Operation::Upsert(item) => {
                    upsert(&db, &item);
                    model_upsert(&mut rows, item);
                }
                Operation::Delete(id) => {
                    delete(&db, id);
                    rows.retain(|row| row.id != id);
                }
                Operation::Rollback(item, id) => {
                    let transaction = db.transaction().unwrap();
                    let mut tentative = rows.clone();
                    upsert(&transaction, &item);
                    model_upsert(&mut tentative, item);
                    delete(&transaction, id);
                    tentative.retain(|row| row.id != id);
                    for query in probes(&tentative) { compare(&transaction, &tentative, &query, first); }
                    transaction.rollback().unwrap();
                }
                Operation::Query(query) => compare(&db, &rows, &query, first),
                Operation::Rebuild => rebuild(&db),
            }
            observations.extend(probes(&rows));
            for query in observations { compare(&db, &rows, &query, first); }
        }
    }

    #[test]
    fn populated_migration_is_transactional(
        initial in prop::collection::vec(row_strategy(), 1..12), query in query_strategy(),
    ) {
        let rows = unique_rows(initial);
        let mut db = Connection::open_in_memory().unwrap();
        crate::filter::search::register(&db).unwrap();
        schema(&db);
        for item in &rows { upsert(&db, item); }
        let transaction = db.transaction().unwrap();
        crate::filter::search::migrate(&transaction).unwrap();
        compare(&transaction, &rows, &query, 3);
        transaction.rollback().unwrap();
        // A rolled-back migration must leave neither conflicting schema objects
        // nor partially indexed content when the real migration is retried.
        migrate(&mut db);
        compare(&db, &rows, &query, 3);
        for query in probes(&rows) { compare(&db, &rows, &query, 2); }
        rebuild(&db);
        compare(&db, &rows, &query, 3);
    }
}

#[test]
fn transposed_radiohead_is_fuzzy_but_quoted_typo_is_literal() {
    let db = fixture(&[row(1, "Radiohead", "Radiohead")]);
    assert_eq!(sql_ids(&db, "radiohaed", 1), vec![1]);
    assert_eq!(sql_ids(&db, "title:radiohaed", 3), vec![1]);
    assert!(sql_ids(&db, "\"radiohaed\"", 1).is_empty());
    assert!(sql_ids(&db, "title:\"radiohaed\"", 3).is_empty());
    assert!(sql_ids(&db, "NOT radiohaed", 1).is_empty());
    assert_eq!(sql_ids(&db, "NOT \"radiohaed\"", 1), vec![1]);
}

#[test]
fn typo_eligibility_is_bounded_and_literals_do_not_become_patterns() {
    for (needle, haystack, matches) in [
        ("abc".to_owned(), "axc".to_owned(), false),
        ("abcd".to_owned(), "abxd".to_owned(), true),
        ("a".repeat(64), format!("{}b", "a".repeat(63)), true),
        ("a".repeat(65), format!("{}b", "a".repeat(64)), false),
        ("1234".to_owned(), "1235".to_owned(), false),
        ("ab-cd".to_owned(), "ab-xd".to_owned(), false),
        ("%".to_owned(), "anything".to_owned(), false),
        ("_".to_owned(), "anything".to_owned(), false),
        ("東京楽音".to_owned(), "東京音楽".to_owned(), true),
        ("écolf".to_owned(), "école".to_owned(), true),
        ("radiohaed".to_owned(), "preradioheadpost".to_owned(), false),
        ("radiohaed".to_owned(), "preradiohaedpost".to_owned(), true),
        ("radiozzad".to_owned(), "radiohead".to_owned(), false),
        ("radiohaed".to_owned(), "radiohead\u{301}".to_owned(), true),
        ("radiohaed".to_owned(), "radiohead123".to_owned(), false),
        ("radiohaed".to_owned(), "radiohead\u{e000}".to_owned(), true),
        ("radiohaed".to_owned(), "before\0radiohead".to_owned(), true),
        ("abcdef".to_owned(), "abdcef".to_owned(), true),
        ("abcdefg".to_owned(), "abdcefg".to_owned(), true),
        ("abcdefg".to_owned(), "abcedfg".to_owned(), true),
    ] {
        let db = fixture(&[row(1, &haystack, &haystack)]);
        assert_eq!(
            sql_ids(&db, &needle, 2),
            if matches { vec![1] } else { vec![] },
            "needle={needle:?}, haystack={haystack:?}"
        );
    }
    let literal = "x'); DROP TABLE library_tracks;-- %_ \"quoted\" \\path";
    let db = fixture(&[row(1, literal, literal), row(2, "unrelated", "unrelated")]);
    assert_eq!(sql_ids(&db, &quote(literal), 4), vec![1]);
    assert_eq!(sql_ids(&db, "NOT title:\"unrelated\"", 1), vec![1]);
}

#[test]
fn field_lists_and_implicit_and_preserve_boolean_scope() {
    let mut first = row(1, "Radiohead night", "night");
    first.artist = 0;
    first.album = 0; // album artist is Björk, not Radiohead.
    let mut second = row(2, "Björk light", "light");
    second.artist = 1;
    second.album = 1;
    second.genre = None;
    second.year = None;
    let db = fixture(&[first, second]);
    assert_eq!(
        sql_ids(&db, "artist:[radiohaed,björk] NOT title:nigth", 2),
        vec![2]
    );
    assert_eq!(
        sql_ids(&db, "albumartist:björk AND NOT artist:björk", 3),
        vec![1]
    );
    assert_eq!(sql_ids(&db, "NOT genre:alternatvie", 1), vec![2]);
    assert_eq!(sql_ids(&db, "NOT year:2007", 1), vec![2]);
    assert!(sql_ids(&db, "title:radiohaed", 1).is_empty());
}

#[test]
fn persisted_indexes_survive_reopen_delete_reinsert_and_rebuild() {
    struct DatabasePath(std::path::PathBuf);
    impl Drop for DatabasePath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let path = DatabasePath(
        std::env::temp_dir().join(format!("tyranny-search-{}.sqlite", uuid::Uuid::new_v4())),
    );
    let mut rows = vec![row(1, "Radiohead", "night"), row(2, "Björk", "light")];
    {
        let mut db = Connection::open(&path.0).unwrap();
        crate::filter::search::register(&db).unwrap();
        schema(&db);
        for item in &rows {
            upsert(&db, item);
        }
        migrate(&mut db);
        for query in probes(&rows) {
            compare(&db, &rows, &query, 2);
        }
        delete(&db, 1);
        rows[0] = row(1, "Massive Attack", "Radiohead");
        upsert(&db, &rows[0]);
        let transaction = db.transaction().unwrap();
        delete(&transaction, 2);
        upsert(&transaction, &row(3, "radiohead", "radiohead"));
        transaction.rollback().unwrap();
    }
    let db = Connection::open(&path.0).unwrap();
    crate::filter::search::register(&db).unwrap();
    let observations = probes(&rows);
    for query in &observations {
        compare(&db, &rows, query, 4);
    }
    assert!(sql_ids(&db, "radiohaed", 1).is_empty());
    assert_eq!(sql_ids(&db, "title:radiohaed", 1), vec![1]);
    rebuild(&db);
    for query in observations {
        compare(&db, &rows, &query, 4);
    }
}
