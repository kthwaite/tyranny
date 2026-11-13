//! Indexed substring candidates and bounded whole-word typo matching.
//! Both indexes follow the authoritative normalized track columns in the same
//! transaction, including SQL import, retagging, rollback and deletion.
use super::Field;
use rusqlite::{Connection, Transaction, functions::FunctionFlags};

pub(crate) fn migrate(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    transaction.execute_batch(
        "CREATE VIRTUAL TABLE track_substrings USING fts5(search,title_sort,
             content='library_tracks',content_rowid='id',tokenize='trigram case_sensitive 1');
         CREATE VIRTUAL TABLE track_words USING fts5(search,title_sort,
             content='library_tracks',content_rowid='id',tokenize='ascii');
         CREATE VIRTUAL TABLE track_vocabulary USING fts5vocab(track_words,'row');
         CREATE TRIGGER track_search_insert AFTER INSERT ON library_tracks BEGIN
             INSERT INTO track_substrings(rowid,search,title_sort) VALUES(new.id,new.search,new.title_sort);
             INSERT INTO track_words(rowid,search,title_sort) VALUES(new.id,new.search,new.title_sort);
         END;
         CREATE TRIGGER track_search_delete AFTER DELETE ON library_tracks BEGIN
             INSERT INTO track_substrings(track_substrings,rowid,search,title_sort)
                 VALUES('delete',old.id,old.search,old.title_sort);
             INSERT INTO track_words(track_words,rowid,search,title_sort)
                 VALUES('delete',old.id,old.search,old.title_sort);
         END;
         CREATE TRIGGER track_search_update AFTER UPDATE OF id,search,title_sort ON library_tracks
             WHEN old.id!=new.id OR old.search!=new.search OR old.title_sort!=new.title_sort BEGIN
             INSERT INTO track_substrings(track_substrings,rowid,search,title_sort)
                 VALUES('delete',old.id,old.search,old.title_sort);
             INSERT INTO track_words(track_words,rowid,search,title_sort)
                 VALUES('delete',old.id,old.search,old.title_sort);
             INSERT INTO track_substrings(rowid,search,title_sort) VALUES(new.id,new.search,new.title_sort);
             INSERT INTO track_words(rowid,search,title_sort) VALUES(new.id,new.search,new.title_sort);
         END;
         INSERT INTO track_substrings(track_substrings) VALUES('rebuild');
         INSERT INTO track_words(track_words) VALUES('rebuild');",
    )?;
    Ok(())
}

const MAX_WORD: usize = 64;

struct Word {
    chars: [char; MAX_WORD + 1],
    len: usize,
}
impl Word {
    fn new(text: &str) -> Self {
        let mut result = Self {
            chars: ['\0'; MAX_WORD + 1],
            len: 0,
        };
        for ch in text.chars() {
            if result.len == result.chars.len() {
                result.len += 1;
                break;
            }
            result.chars[result.len] = ch;
            result.len += 1;
        }
        result
    }

    fn near(&self, text: &str) -> bool {
        let other = Self::new(text);
        if self.len > MAX_WORD || other.len > MAX_WORD + 1 || self.len.abs_diff(other.len) > 1 {
            return false;
        }
        let a = &self.chars[..self.len];
        let b = &other.chars[..other.len];
        let prefix = a.iter().zip(b).take_while(|(a, b)| a == b).count();
        if prefix == a.len().min(b.len()) {
            return true;
        }
        match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Less => a[prefix..] == b[prefix + 1..],
            std::cmp::Ordering::Greater => a[prefix + 1..] == b[prefix..],
            std::cmp::Ordering::Equal => {
                a[prefix + 1..] == b[prefix + 1..]
                    || (prefix + 1 < a.len()
                        && a[prefix] == b[prefix + 1]
                        && a[prefix + 1] == b[prefix]
                        && a[prefix + 2..] == b[prefix + 2..])
            }
        }
    }
}

pub(crate) fn register(connection: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    connection.create_scalar_function("search_matches", 2, flags, |ctx| {
        let text = ctx.get_raw(0).as_str()?;
        let query = ctx.get_raw(1).as_str()?;
        if text.contains(query) {
            return Ok(true);
        }
        let word = ctx.get_or_create_aux(1, |value| value.as_str().map(Word::new))?;
        Ok(text
            .split(|ch: char| !ch.is_alphanumeric())
            .any(|token| word.near(token)))
    })
}

/// Quotes for the FTS grammar, independently of SQL parameter binding.
fn phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(super) fn predicate(field: Field, value: &str, fuzzy: bool, parameter: usize) -> String {
    let needle = format!("?{parameter}");
    let matches = |column: &str| {
        if fuzzy {
            format!("search_matches({column},{needle})")
        } else {
            format!("instr({column},{needle})>0")
        }
    };
    match field {
        Field::Any | Field::Title => {
            let column = if matches!(field, Field::Any) {
                "search"
            } else {
                "title_sort"
            };
            let length = value.chars().count();
            if length < 3 || value.contains('\0') {
                // No usable trigram: avoid building a second full-library ID set.
                return format!("instr(t.{column},{needle})>0");
            }
            if fuzzy && length >= 7 {
                // One edit, even a transposition, cannot damage both end trigrams:
                // at least one scalar separates them. Their union is a complete
                // candidate superset; the word matcher below rejects false positives.
                let prefix_end = value.char_indices().nth(3).unwrap().0;
                let suffix_start = value.char_indices().rev().nth(2).unwrap().0;
                let query = format!(
                    "{column} : ({} OR {})",
                    phrase(&value[..prefix_end]),
                    phrase(&value[suffix_start..])
                )
                .replace('\'', "''");
                return format!(
                    "t.id IN (SELECT rowid FROM track_substrings WHERE track_substrings MATCH '{query}' \
                     AND search_matches({column},{needle}))"
                );
            }
            // Keep instr as the semantic authority, even for tokenizer edge cases.
            let query = format!("{column} : {}", phrase(value)).replace('\'', "''");
            let literal = format!(
                "SELECT rowid FROM track_substrings WHERE track_substrings MATCH '{query}' \
                                   AND instr({column},{needle})>0"
            );
            let candidates = if fuzzy {
                // The uncorrelated vocabulary scan and MATCH run once per term, not per track.
                // No match is an empty quoted FTS phrase, not invalid empty MATCH syntax.
                // ASCII tokenization keeps Unicode bytes unchanged. A vocabulary token can
                // span Unicode separators; search_matches splits those into scalar words.
                // Recheck the source text so index token boundaries never define semantics.
                format!(
                    "{literal} UNION SELECT rowid FROM track_words WHERE track_words MATCH \
                    ('{column} : (' || COALESCE((SELECT group_concat('\"' || replace(term,'\"','\"\"') || '\"',' OR ') \
                    FROM track_vocabulary WHERE instr(term,{needle})=0 \
                    AND search_matches(term,{needle})), '\"\"') || ')') \
                    AND search_matches({column},{needle})"
                )
            } else {
                literal
            };
            format!("t.id IN ({candidates})")
        }
        Field::Artist => format!(
            "t.artist_id IN (SELECT id FROM library_artists WHERE {})",
            matches("search")
        ),
        Field::Album => format!(
            "t.album_id IN (SELECT id FROM library_albums WHERE {})",
            matches("title_sort")
        ),
        Field::AlbumArtist => format!(
            "t.album_id IN (SELECT id FROM library_albums WHERE artist_id IN \
            (SELECT id FROM library_artists WHERE {}))",
            matches("search")
        ),
        Field::Genre => format!(
            "t.id IN (SELECT track_id FROM library_track_genres WHERE genre_id IN \
            (SELECT id FROM library_genres WHERE {}))",
            matches("search")
        ),
        Field::Year | Field::Liked => unreachable!(),
    }
}

pub(super) fn fuzzy_word(value: &str) -> bool {
    (4..=MAX_WORD).contains(&value.chars().count()) && value.chars().all(char::is_alphabetic)
}
