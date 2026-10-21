//! Play rows and the library index, loaded from the store for aggregation.
#![allow(dead_code)] // Task 7 uses this
use crate::store::EndReason;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

/// One `play_history` row, reduced to what statistics read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlayRow {
    /// UTC seconds.
    pub played_at: i64,
    /// `None` for rows from before schema 8.
    pub listened_ms: Option<u64>,
    pub counted: bool,
    /// `None` while the play is open, and for rows from before schema 8.
    pub end_reason: Option<EndReason>,
    pub track_id: Option<u64>,
    pub artist: String,
    pub title: String,
    pub radio: bool,
    /// The station's name, for radio.
    pub station: Option<String>,
}

/// Library IDs by key. A key that matches more than one library ID is left out.
#[derive(Debug, Default)]
pub(crate) struct LibraryIndex {
    pub artists: HashMap<String, u64>,
    pub tracks: HashMap<(String, String), u64>,
    /// Library track count by library artist ID.
    pub artist_tracks: HashMap<u64, u64>,
}

/// The artist key of `name`: trimmed and lowercase. A track key is `(artist_key(artist), artist_key(title))`.
pub(crate) fn artist_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Every play, ordered by `played_at`, and the library index.
pub(crate) fn load(connection: &Connection) -> Result<(Vec<PlayRow>, LibraryIndex)> {
    Ok((plays(connection)?, library(connection)?))
}

fn plays(connection: &Connection) -> Result<Vec<PlayRow>> {
    let mut query = connection.prepare(
        "SELECT played_at,listened_ms,counted,end_reason,track_id,artist,title,source_kind,source_name
         FROM play_history ORDER BY played_at,id",
    )?;
    let rows = query.query_map([], |row| {
        let radio = row.get::<_, Option<String>>(7)?.as_deref() == Some("stream");
        Ok(PlayRow {
            played_at: row.get(0)?,
            listened_ms: row.get(1)?,
            counted: row.get(2)?,
            end_reason: row
                .get::<_, Option<String>>(3)?
                .and_then(|reason| EndReason::parse(&reason)),
            track_id: row.get(4)?,
            artist: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            title: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
            radio,
            station: if radio { row.get(8)? } else { None },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn library(connection: &Connection) -> Result<LibraryIndex> {
    let mut artists = Unique::default();
    let mut query = connection.prepare("SELECT id,name FROM library_artists")?;
    for row in query.query_map([], |row| {
        Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, name) = row?;
        artists.add(artist_key(&name), id);
    }
    let mut tracks = Unique::default();
    let mut artist_tracks = HashMap::new();
    let mut query = connection.prepare(
        "SELECT id,artist_id,json_extract(metadata,'$.artist'),json_extract(metadata,'$.title')
         FROM library_tracks",
    )?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, u64>(1)?,
            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        ))
    })?;
    for row in rows {
        let (id, artist_id, artist, title) = row?;
        *artist_tracks.entry(artist_id).or_insert(0) += 1;
        let title = artist_key(&title);
        if !title.is_empty() {
            tracks.add((artist_key(&artist), title), id);
        }
    }
    Ok(LibraryIndex {
        artists: artists.into_unique(),
        tracks: tracks.into_unique(),
        artist_tracks,
    })
}

/// IDs by key, remembering which keys more than one ID claimed.
struct Unique<K> {
    ids: HashMap<K, Option<u64>>,
}
impl<K> Default for Unique<K> {
    fn default() -> Self {
        Self {
            ids: HashMap::new(),
        }
    }
}
impl<K: std::hash::Hash + Eq> Unique<K> {
    fn add(&mut self, key: K, id: u64) {
        self.ids
            .entry(key)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(id));
    }
    fn into_unique(self) -> HashMap<K, u64> {
        self.ids
            .into_iter()
            .filter_map(|(key, id)| id.map(|id| (key, id)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use std::path::PathBuf;

    struct Database(PathBuf);
    impl Drop for Database {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn artist_key_trims_and_lowercases() {
        assert_eq!(artist_key("  Burial\t"), "burial");
        assert_eq!(artist_key("   "), "");
    }

    #[test]
    fn load_reads_plays_in_order_and_indexes_unique_library_keys() {
        let path =
            std::env::temp_dir().join(format!("tyranny-stats-{}.sqlite", uuid::Uuid::new_v4()));
        let _store = Store::open(&path).unwrap();
        let _database = Database(path.clone());
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "INSERT INTO library_roots(id,path,search) VALUES(1,'/m','/m');
                INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan) VALUES(1,1,'/m/1',1,1),(2,1,'/m/2',1,1),(3,1,'/m/3',1,1);
                INSERT INTO library_artists(id,identity,name,search) VALUES(1,'a','Burial','b'),(2,'b','Solo','s'),(3,'c','SOLO','s');
                INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation) VALUES(1,'x',1,'A','Burial','a','a',0);
                INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number) VALUES
                    (10,1,1,1,'{\"title\":\"Archangel\",\"artist\":\"Burial\"}','t','t',1,1,1),
                    (11,2,1,1,'{\"title\":\"Untrue\",\"artist\":\"Burial\"}','t','t',1,1,2),
                    (12,3,1,1,'{\"title\":\"untrue\",\"artist\":\"burial\"}','t','t',1,1,3);
                INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,duration_ms,source_kind,source_name,listened_ms,ended_at,end_reason,counted) VALUES
                    (200,1,10,'/m/1','Archangel','Burial','A',200000,NULL,NULL,150000,350,'skipped',1),
                    (100,2,NULL,'http://s','Song','Band','',NULL,'stream','FM',70000,170,'stopped',1),
                    (300,3,NULL,'/m/9','Old','Row','',NULL,NULL,NULL,NULL,NULL,NULL,1),
                    (400,4,11,'/m/2','Untrue','Burial','A',200000,NULL,NULL,5000,NULL,NULL,0);",
            )
            .unwrap();
        let (rows, library) = load(&connection).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.played_at).collect::<Vec<_>>(),
            [100, 200, 300, 400]
        );
        assert_eq!(
            rows[0],
            PlayRow {
                played_at: 100,
                listened_ms: Some(70_000),
                counted: true,
                end_reason: Some(EndReason::Stopped),
                track_id: None,
                artist: "Band".into(),
                title: "Song".into(),
                radio: true,
                station: Some("FM".into()),
            }
        );
        assert_eq!(rows[1].end_reason, Some(EndReason::Skipped));
        assert_eq!(
            (rows[1].radio, rows[1].station.clone(), rows[1].track_id),
            (false, None, Some(10))
        );
        assert_eq!((rows[2].listened_ms, rows[2].end_reason), (None, None));
        // An open row keeps its checkpointed time and has no end reason.
        assert_eq!(
            (rows[3].listened_ms, rows[3].end_reason, rows[3].counted),
            (Some(5_000), None, false)
        );
        // "Solo" and "SOLO" share a key, so neither is indexed. The two "untrue" tracks clash too.
        assert_eq!(library.artists.get("burial"), Some(&1));
        assert_eq!(library.artists.get("solo"), None);
        assert_eq!(
            library.tracks.get(&("burial".into(), "archangel".into())),
            Some(&10)
        );
        assert_eq!(
            library.tracks.get(&("burial".into(), "untrue".into())),
            None
        );
        assert_eq!(library.artist_tracks.get(&1), Some(&3));
        assert_eq!(library.artist_tracks.get(&2), None);
    }
}
