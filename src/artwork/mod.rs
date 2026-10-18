//! Artwork owns discovery, image decoding, provider access and a disposable cache.
//! Two bounded workers read the durable queue/library independently of playback.
mod cache;
mod discovery;
mod provider;
#[cfg(test)]
mod tests;

use crate::protocol::{Artwork, ArtworkSize, ArtworkStatus, ErrorCode, Reply, Response};
use anyhow::{Result, ensure};
use provider::{FetchOutcome, Provider};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

const WORKERS: usize = 2;
const QUEUE_CAPACITY: usize = 16;
const MAX_ALBUM_TRACKS: usize = 1024;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(8);
const NEGATIVE_TTL: u64 = 24 * 60 * 60;
const FAILURE_TTL: u64 = 60;

#[derive(Clone, Copy)]
pub(crate) struct Request {
    pub album_id: Option<u64>,
    pub entry_id: Option<u64>,
    pub library_revision: u64,
    pub size: ArtworkSize,
}
struct Work {
    request: Request,
    reply: oneshot::Sender<Reply>,
}
#[derive(Clone)]
pub(crate) struct Handle {
    sender: mpsc::SyncSender<Work>,
    stop: Arc<AtomicBool>,
}
pub(crate) struct ArtworkWorkers {
    handle: Handle,
    threads: Vec<thread::JoinHandle<()>>,
}
impl ArtworkWorkers {
    pub(crate) fn start(database: PathBuf) -> Self {
        Self::with_provider(database, OnceLock::new())
    }
    fn with_provider(
        database: PathBuf,
        provider: OnceLock<std::result::Result<Provider, String>>,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Work>(QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let stop = Arc::new(AtomicBool::new(false));
        let provider = Arc::new(provider);
        let provider_gate = Arc::new(Mutex::new(()));
        let mut threads = Vec::with_capacity(WORKERS);
        for index in 0..WORKERS {
            let receiver = Arc::clone(&receiver);
            let stop = Arc::clone(&stop);
            let database = database.clone();
            let provider = Arc::clone(&provider);
            let provider_gate = Arc::clone(&provider_gate);
            let spawned = thread::Builder::new()
                .name(format!("artwork-{index}"))
                .spawn(move || {
                    let mut engine = None;
                    while !stop.load(Ordering::Acquire) {
                        let next = receiver
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .recv_timeout(Duration::from_millis(100));
                        let work = match next {
                            Ok(work) => work,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        };
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        if work.reply.is_closed() {
                            continue;
                        }
                        if engine.is_none() {
                            let provider = provider
                                .get_or_init(|| Provider::new().map_err(|e| format!("{e:#}")))
                                .clone();
                            match Engine::open(
                                &database,
                                provider,
                                Arc::clone(&stop),
                                Arc::clone(&provider_gate),
                            ) {
                                Ok(opened) => engine = Some(opened),
                                Err(error) => {
                                    let _ = work
                                        .reply
                                        .send(ok(Artwork::unavailable(format!("{error:#}"))));
                                    continue;
                                }
                            }
                        }
                        if let Some(engine) = &mut engine {
                            let _ = work.reply.send(engine.request(work.request));
                        }
                    }
                });
            if let Ok(thread) = spawned {
                threads.push(thread);
            }
        }
        Self {
            handle: Handle { sender, stop },
            threads,
        }
    }
    pub(crate) fn handle(&self) -> Handle {
        self.handle.clone()
    }
    pub(crate) fn stop(&self) {
        self.handle.stop.store(true, Ordering::Release);
    }
    pub(crate) fn shutdown(mut self) {
        self.stop();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
impl Drop for ArtworkWorkers {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Handle {
    pub(crate) async fn request(&self, request: Request) -> Reply {
        if self.stop.load(Ordering::Acquire) {
            return ok(Artwork::unavailable("artwork workers stopped"));
        }
        let (reply, receive) = oneshot::channel();
        if self.sender.try_send(Work { request, reply }).is_err() {
            return ok(Artwork::unavailable(
                "artwork request queue is full or unavailable",
            ));
        }
        receive
            .await
            .unwrap_or_else(|_| ok(Artwork::unavailable("artwork request cancelled")))
    }
}
fn ok(artwork: Artwork) -> Reply {
    Reply::Ok {
        result: Response::Artwork { artwork },
    }
}
impl Artwork {
    fn empty(status: ArtworkStatus, message: Option<String>) -> Self {
        Self {
            status,
            source: None,
            cache_key: None,
            width: 0,
            height: 0,
            mime_type: None,
            data: None,
            message,
        }
    }
    fn missing() -> Self {
        Self::empty(ArtworkStatus::Missing, None)
    }
    fn unavailable(message: impl Into<String>) -> Self {
        let mut message = message.into();
        let mut end = message.len().min(1024);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        Self::empty(ArtworkStatus::Unavailable, Some(message))
    }
}
#[derive(Serialize, Deserialize)]
struct Images {
    thumbnail: Artwork,
    display: Artwork,
}
impl Images {
    fn select(self, size: ArtworkSize) -> Artwork {
        match size {
            ArtworkSize::Thumbnail => self.thumbnail,
            ArtworkSize::Display => self.display,
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Local {
    images: Option<Images>,
    release_id: Option<String>,
    failure: Option<String>,
}
#[derive(Serialize, Deserialize)]
enum Online {
    Found(Images),
    Missing,
    Unavailable(String),
}
struct Target {
    paths: Vec<PathBuf>,
    release_id: Option<String>,
    key: String,
    direct: bool,
}
struct TargetError {
    code: ErrorCode,
    message: String,
}
impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::fmt::Debug for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for TargetError {}
fn invalid(message: &str) -> anyhow::Error {
    TargetError {
        code: ErrorCode::InvalidCommand,
        message: message.into(),
    }
    .into()
}
struct Engine {
    database: Connection,
    cache: Option<cache::Cache>,
    cache_path: PathBuf,
    provider: std::result::Result<Provider, String>,
    provider_gate: Arc<Mutex<()>>,
    stop: Arc<AtomicBool>,
}
impl Engine {
    fn open(
        path: &Path,
        provider: std::result::Result<Provider, String>,
        stop: Arc<AtomicBool>,
        provider_gate: Arc<Mutex<()>>,
    ) -> Result<Self> {
        let database = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        database.busy_timeout(Duration::from_millis(100))?;
        let cache_path = path.with_extension("artwork.sqlite");
        // A cache failure affects artwork performance only, never player startup.
        let cache = cache::Cache::open(&cache_path).ok();
        Ok(Self {
            database,
            cache,
            cache_path,
            provider,
            provider_gate,
            stop,
        })
    }
    fn request(&mut self, request: Request) -> Reply {
        if self.cache.is_none() {
            self.cache = cache::Cache::open(&self.cache_path).ok();
        }
        let result = self.resolve(request).and_then(|target| {
            let artwork = self.load(target, request.size);
            // Re-resolve after slow IO: stale revision/removed occurrence cannot be
            // mistaken for the current target, even if a client missed an event.
            self.revalidate(request)?;
            Ok(artwork)
        });
        match result {
            Ok(artwork) => ok(artwork),
            Err(error) => {
                if let Some(error) = error.downcast_ref::<TargetError>() {
                    let revision = self
                        .database
                        .query_row(
                            "SELECT revision FROM control WHERE singleton=1",
                            [],
                            |row| row.get(0),
                        )
                        .unwrap_or(0);
                    Reply::Error {
                        code: error.code,
                        message: error.message.clone(),
                        queue_revision: revision,
                    }
                } else {
                    ok(Artwork::unavailable(format!("{error:#}")))
                }
            }
        }
    }
    fn revalidate(&self, request: Request) -> Result<()> {
        let (revision, exists): (u64, bool) = self.database.query_row(
            "SELECT revision,CASE WHEN ?1 IS NOT NULL THEN EXISTS(SELECT 1 FROM queue WHERE id=?1)
             ELSE EXISTS(SELECT 1 FROM library_albums WHERE id=?2) END FROM library_state WHERE singleton=1",
            rusqlite::params![request.entry_id.and_then(|id| i64::try_from(id).ok()), request.album_id.and_then(|id| i64::try_from(id).ok())],
            |row| Ok((row.get(0)?, row.get(1)?)))?;
        if revision != request.library_revision {
            return Err(TargetError {
                code: ErrorCode::StaleLibraryRevision,
                message: format!("library revision is {revision}"),
            }
            .into());
        }
        ensure!(exists, invalid("artwork target was removed"));
        Ok(())
    }
    fn resolve(&mut self, request: Request) -> Result<Target> {
        let transaction = self.database.transaction()?;
        let revision: u64 = transaction.query_row(
            "SELECT revision FROM library_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        if revision != request.library_revision {
            return Err(TargetError {
                code: ErrorCode::StaleLibraryRevision,
                message: format!("library revision is {revision}"),
            }
            .into());
        }
        ensure!(
            request.album_id.is_some() != request.entry_id.is_some(),
            invalid("artwork requires exactly one album_id or entry_id")
        );
        let (album, direct_path) = match (request.album_id, request.entry_id) {
            (Some(id), None) => (
                Some(i64::try_from(id).map_err(|_| invalid("invalid album id"))?),
                None,
            ),
            (None, Some(id)) => {
                let id = i64::try_from(id).map_err(|_| invalid("invalid queue entry id"))?;
                let entry: Option<(String, Option<i64>)> = transaction.query_row(
                    "SELECT q.path,(SELECT t.album_id FROM library_tracks t JOIN library_assets a ON a.id=t.asset_id
                     JOIN library_roots r ON r.id=a.root_id WHERE a.path=q.path AND a.indexed=1 AND a.last_seen_scan>=r.completed_scan)
                     FROM queue q WHERE q.id=?1", [id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
                let (path, album) =
                    entry.ok_or_else(|| invalid("queue entry is unknown or removed"))?;
                (album, Some(PathBuf::from(path)))
            }
            _ => return Err(invalid("artwork requires exactly one album_id or entry_id")),
        };
        let mut hash = Sha256::new();
        hash.update(b"local-artwork-v1\0");
        let mut paths = Vec::new();
        let mut release_id = None;
        if let Some(album) = album {
            let identity: Option<(String, Option<String>)> = transaction
                .query_row(
                    "SELECT identity,musicbrainz_release_id FROM library_albums WHERE id=?1",
                    [album],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (identity, release) =
                identity.ok_or_else(|| invalid("album is unknown or unavailable"))?;
            hash.update(identity.as_bytes());
            release_id = release;
            let mut statement = transaction.prepare(
                "SELECT a.path,a.size,a.modified_ns,r.completed_scan FROM library_tracks t
                 JOIN library_assets a ON a.id=t.asset_id JOIN library_roots r ON r.id=a.root_id
                 WHERE t.album_id=?1 AND a.indexed=1 AND a.last_seen_scan>=r.completed_scan
                 ORDER BY t.disc_number,t.track_number,t.id LIMIT 1025",
            )?;
            let rows = statement.query_map([album], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            })?;
            for row in rows {
                let (path, size, modified, scan) = row?;
                hash.update(path.as_bytes());
                hash.update([0]);
                hash.update(size.unwrap_or(0).to_le_bytes());
                hash.update(modified.unwrap_or(0).to_le_bytes());
                hash.update(scan.to_le_bytes());
                paths.push(PathBuf::from(path));
            }
            ensure!(
                paths.len() <= MAX_ALBUM_TRACKS,
                "album exceeds artwork discovery limit of {MAX_ALBUM_TRACKS} tracks"
            );
            if paths.is_empty() {
                return Err(invalid("album is unknown or unavailable"));
            }
        } else if let Some(path) = direct_path {
            // Completed scans invalidate direct-path folder art too, including
            // paths not indexed as library tracks. No main-schema migration.
            let scan: u64 = transaction.query_row(
                "SELECT COALESCE(MAX(completed_scan),0) FROM library_roots",
                [],
                |row| row.get(0),
            )?;
            hash.update(path.as_os_str().as_encoded_bytes());
            hash.update(scan.to_le_bytes());
            if let Ok(metadata) = std::fs::metadata(&path) {
                hash.update(metadata.len().to_le_bytes());
                if let Ok(modified) = metadata.modified() {
                    hash.update(
                        modified
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                            .to_le_bytes(),
                    );
                }
            }
            paths.push(path);
        }
        transaction.commit()?;
        Ok(Target {
            paths,
            release_id,
            key: format!("local:{:x}", hash.finalize()),
            direct: album.is_none(),
        })
    }
    fn load(&mut self, target: Target, size: ArtworkSize) -> Artwork {
        let local: Local = if let Some(cached) =
            self.cache.as_ref().and_then(|cache| cache.get(&target.key))
        {
            cached
        } else {
            let deadline = Instant::now() + DISCOVERY_TIMEOUT;
            let (images, mut failure) = match discovery::local(&target.paths, &self.stop, deadline)
            {
                Ok(discovered) => discovered,
                // Incomplete discovery cannot authorize any lower-priority source.
                Err(error) => return Artwork::unavailable(format!("{error:#}")),
            };
            let release_id = if target.release_id.is_some() || !target.direct || images.is_some() {
                target.release_id
            } else {
                let probed = crate::metadata::probe_before(&target.paths[0], &self.stop, deadline);
                if self.stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return Artwork::unavailable(
                        "artwork discovery cancelled or deadline exceeded",
                    );
                }
                match probed {
                    Ok(metadata) => metadata.musicbrainz_release_id,
                    Err(error) => {
                        failure.get_or_insert_with(|| format!("{error:#}"));
                        None
                    }
                }
            };
            let local = Local {
                images,
                release_id,
                failure,
            };
            if let Some(cache) = &mut self.cache {
                let ttl = if local.images.is_none() && local.failure.is_some() {
                    Some(FAILURE_TTL)
                } else {
                    None
                };
                cache.put(&target.key, &local, ttl);
            }
            local
        };
        if let Some(images) = local.images {
            return images.select(size);
        }
        if self.stop.load(Ordering::Acquire) {
            return Artwork::unavailable("artwork request cancelled");
        }
        let Some(release_id) = local.release_id else {
            return local
                .failure
                .map(Artwork::unavailable)
                .unwrap_or_else(Artwork::missing);
        };
        let release_id = match uuid::Uuid::parse_str(&release_id) {
            Ok(id) => id.hyphenated().to_string(),
            Err(_) => return Artwork::unavailable("invalid MusicBrainz release ID"),
        };
        let key = format!("release:{release_id}");
        let online = if let Some(cached) = self
            .cache
            .as_ref()
            .and_then(|cache| cache.get::<Online>(&key))
        {
            cached
        } else {
            // Only cold provider requests serialize; local discovery and cached
            // online hits stay independent. Recheck persistence after the gate
            // so simultaneous thumbnail/display requests download only once.
            let _fetch = self.provider_gate.lock().unwrap_or_else(|p| p.into_inner());
            if self.stop.load(Ordering::Acquire) {
                return Artwork::unavailable("artwork request cancelled");
            }
            if self.cache.is_none() {
                self.cache = cache::Cache::open(&self.cache_path).ok();
            }
            if let Some(cached) = self
                .cache
                .as_ref()
                .and_then(|cache| cache.get::<Online>(&key))
            {
                match cached {
                    Online::Found(images) => return images.select(size),
                    Online::Missing => {
                        return local
                            .failure
                            .map(Artwork::unavailable)
                            .unwrap_or_else(Artwork::missing);
                    }
                    Online::Unavailable(message) => return Artwork::unavailable(message),
                }
            }
            let online = match &self.provider {
                Ok(provider) => match provider.fetch_front(&release_id) {
                    FetchOutcome::Found(bytes) => match discovery::normalize(
                        &bytes,
                        crate::protocol::ArtworkSource::CoverArtArchive,
                    ) {
                        Ok(images) => Online::Found(images),
                        Err(error) => {
                            Online::Unavailable(format!("invalid provider artwork: {error:#}"))
                        }
                    },
                    FetchOutcome::Missing => Online::Missing,
                    FetchOutcome::Unavailable(message) => Online::Unavailable(message),
                },
                Err(message) => Online::Unavailable(message.clone()),
            };
            let ttl = match &online {
                Online::Found(_) => None,
                Online::Missing => Some(NEGATIVE_TTL),
                Online::Unavailable(_) => Some(FAILURE_TTL),
            };
            if let Some(cache) = &mut self.cache {
                cache.put(&key, &online, ttl);
            }
            online
        };
        match online {
            Online::Found(images) => images.select(size),
            Online::Missing => local
                .failure
                .map(Artwork::unavailable)
                .unwrap_or_else(Artwork::missing),
            Online::Unavailable(message) => Artwork::unavailable(message),
        }
    }
}
fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
