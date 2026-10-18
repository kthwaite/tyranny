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

const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RECORDS: u64 = 4096;
const MAX_RECORD_BYTES: usize = 6 * 1024 * 1024;
const APPLICATION_ID: i64 = 0x74796172;

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
            "artwork cache must be a regular file"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        let application: i64 =
            connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(
            (application == 0 && version == 0) || (application == APPLICATION_ID && version == 1),
            "unsupported private artwork cache"
        );
        // FULL auto-vacuum returns evicted pages to the filesystem. A hard page
        // ceiling bounds payload plus index/SQLite overhead, even on write failure.
        connection.execute_batch("PRAGMA auto_vacuum=FULL; PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL; PRAGMA max_page_count=34816;
            CREATE TABLE IF NOT EXISTS artwork_cache (
                key TEXT PRIMARY KEY, value TEXT NOT NULL, bytes INTEGER NOT NULL,
                expires INTEGER, touched INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS artwork_cache_lru ON artwork_cache(touched,key);")?;
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
        connection.pragma_update(None, "user_version", 1)?;
        Ok(Self { connection })
    }
    pub(super) fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let value: Option<String> = self.connection.query_row(
            "SELECT value FROM artwork_cache WHERE key=?1 AND (expires IS NULL OR expires>?2) AND bytes<=?3 AND length(value)<=?3",
            params![key, unix_time(), MAX_RECORD_BYTES as u64], |row| row.get(0)).optional().ok()?;
        let decoded = serde_json::from_str(value.as_deref()?).ok()?;
        let _ = self.connection.execute(
            "UPDATE artwork_cache SET touched=?1 WHERE key=?2",
            params![unix_time(), key],
        );
        Some(decoded)
    }
    pub(super) fn put<T: Serialize>(&mut self, key: &str, value: &T, ttl: Option<u64>) {
        // Persistence is an optimization. Failure must not discard a usable image
        // or turn an unrelated playback command into a persistence error.
        let _ = self.store(key, value, ttl);
    }
    fn store<T: Serialize>(&mut self, key: &str, value: &T, ttl: Option<u64>) -> Result<()> {
        let value = serde_json::to_string(value)?;
        ensure!(
            value.len() <= MAX_RECORD_BYTES,
            "artwork cache record exceeds limit"
        );
        let now = unix_time();
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM artwork_cache WHERE key=?1 OR expires<=?2",
            params![key, now],
        )?;
        let (mut bytes, mut count): (u64, u64) = transaction.query_row(
            "SELECT COALESCE(SUM(bytes),0),COUNT(*) FROM artwork_cache",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        while count >= MAX_RECORDS || bytes + value.len() as u64 > MAX_BYTES {
            let (oldest, size): (String, u64) = transaction.query_row(
                "SELECT key,bytes FROM artwork_cache ORDER BY touched,key LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            transaction.execute("DELETE FROM artwork_cache WHERE key=?1", [oldest])?;
            bytes = bytes.saturating_sub(size);
            count -= 1;
        }
        transaction.execute(
            "INSERT INTO artwork_cache(key,value,bytes,expires,touched) VALUES(?1,?2,?3,?4,?5)",
            params![
                key,
                value,
                value.len() as u64,
                ttl.map(|ttl| now.saturating_add(ttl)),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }
}
