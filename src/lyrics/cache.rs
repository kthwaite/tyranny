use super::unix_time;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::Duration,
};

const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECORDS: u64 = 2048;
const MAX_RECORD_BYTES: usize = 512 * 1024;
const APPLICATION_ID: i64 = 0x74796c72;

pub(super) struct Cache {
    connection: Connection,
}
impl Cache {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "lyrics cache must be a regular file"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        let application: i64 = connection.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            (application == 0 && version == 0) || (application == APPLICATION_ID && version == 1),
            "unsupported private lyrics cache"
        );
        connection.execute_batch("PRAGMA auto_vacuum=FULL; PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL; PRAGMA max_page_count=4608;
            CREATE TABLE IF NOT EXISTS lyrics_cache(key TEXT PRIMARY KEY,value TEXT NOT NULL,bytes INTEGER NOT NULL,expires INTEGER,touched INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS lyrics_cache_lru ON lyrics_cache(touched,key);")?;
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
        connection.pragma_update(None, "user_version", 1)?;
        Ok(Self { connection })
    }
    pub(super) fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let value: Option<String> = self.connection.query_row(
            "SELECT value FROM lyrics_cache WHERE key=?1 AND (expires IS NULL OR expires>?2) AND bytes<=?3 AND length(CAST(value AS BLOB))<=?3",
            params![key, unix_time(), MAX_RECORD_BYTES as u64], |r| r.get(0)).optional().ok()?;
        let decoded = serde_json::from_str(value.as_deref()?).ok()?;
        let _ = self.connection.execute(
            "UPDATE lyrics_cache SET touched=?1 WHERE key=?2",
            params![unix_time(), key],
        );
        Some(decoded)
    }
    pub(super) fn put<T: Serialize>(&mut self, key: &str, value: &T, ttl: Option<u64>) {
        // Cache failures affect performance only, never playback or usable lyrics.
        let _ = self.store(key, value, ttl);
    }
    fn store<T: Serialize>(&mut self, key: &str, value: &T, ttl: Option<u64>) -> Result<()> {
        let value = serde_json::to_string(value)?;
        ensure!(
            value.len() <= MAX_RECORD_BYTES,
            "lyrics cache record exceeds limit"
        );
        let now = unix_time();
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM lyrics_cache WHERE key=?1 OR expires<=?2",
            params![key, now],
        )?;
        let (mut bytes, mut count): (u64, u64) = tx.query_row(
            "SELECT COALESCE(SUM(bytes),0),COUNT(*) FROM lyrics_cache",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        while count >= MAX_RECORDS || bytes + value.len() as u64 > MAX_BYTES {
            let (oldest, size): (String, u64) = tx.query_row(
                "SELECT key,bytes FROM lyrics_cache ORDER BY touched,key LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            tx.execute("DELETE FROM lyrics_cache WHERE key=?1", [oldest])?;
            bytes = bytes.saturating_sub(size);
            count -= 1;
        }
        tx.execute(
            "INSERT INTO lyrics_cache(key,value,bytes,expires,touched) VALUES(?1,?2,?3,?4,?5)",
            params![
                key,
                value,
                value.len() as u64,
                ttl.map(|ttl| now.saturating_add(ttl)),
                now
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_persistent_bounded_and_expiring() {
        let dir = std::env::temp_dir().join(format!(
            "tyranny-likes-smoke-lyrics-cache-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("cache.sqlite");
        let mut cache = Cache::open(&path).unwrap();
        cache.put("ready", &"offline lyrics", None);
        cache.put("expired", &"missing", Some(0));
        cache.put("oversized", &"x".repeat(MAX_RECORD_BYTES), None);
        assert!(cache.get::<String>("expired").is_none());
        assert!(cache.get::<String>("oversized").is_none());
        drop(cache);
        let mut cache = Cache::open(&path).unwrap();
        assert_eq!(
            cache.get::<String>("ready").as_deref(),
            Some("offline lyrics")
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for n in 0..MAX_RECORDS + 1 {
            cache.put(&format!("entry:{n}"), &n, None);
        }
        let count: u64 = cache
            .connection
            .query_row("SELECT COUNT(*) FROM lyrics_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, MAX_RECORDS);
        assert!(fs::metadata(&path).unwrap().len() <= 4608 * 4096);
        drop(cache);
        fs::remove_dir_all(dir).unwrap();
    }
}
