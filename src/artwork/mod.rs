//! Artwork owns discovery, image decoding, provider access and a disposable cache.
//! Two bounded workers read the durable queue/library independently of playback.
mod cache;
mod discovery;
mod provider;
#[cfg(test)]
mod tests;

use crate::protocol::{
    Artwork, ArtworkCachePolicy, ArtworkProvider, ArtworkSize, ArtworkSource, ArtworkStatus,
    DiscogsToken, ErrorCode, Reply, Response,
};
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

#[derive(Clone)]
pub(crate) struct Request {
    pub album_id: Option<u64>,
    pub entry_id: Option<u64>,
    pub library_revision: u64,
    pub size: ArtworkSize,
    pub providers: Vec<ArtworkProvider>,
    pub discogs_token: Option<DiscogsToken>,
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
            source_url: None,
            expires_at: None,
            cache_policy: ArtworkCachePolicy::Persistent,
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
    album: String,
    artist: String,
    failure: Option<String>,
}
type Online = std::result::Result<Option<Images>, String>;
struct Target {
    paths: Vec<PathBuf>,
    release_id: Option<String>,
    album: String,
    artist: String,
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
        let result = self.resolve(&request).and_then(|target| {
            let artwork = self.load(target, &request);
            // Re-resolve after slow IO: stale revision/removed occurrence cannot be
            // mistaken for the current target, even if a client missed an event.
            self.revalidate(&request)?;
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
    fn revalidate(&self, request: &Request) -> Result<()> {
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
    fn resolve(&mut self, request: &Request) -> Result<Target> {
        ensure!(
            request.providers.len() <= 3
                && request
                    .providers
                    .iter()
                    .enumerate()
                    .all(|(index, provider)| !request.providers[..index].contains(provider)),
            invalid("artwork providers must be unique and contain at most three entries")
        );
        ensure!(
            !request.providers.contains(&ArtworkProvider::Discogs)
                || request.discogs_token.is_some(),
            invalid("Discogs requires a saved token")
        );
        ensure!(
            request.providers.contains(&ArtworkProvider::Discogs)
                || request.discogs_token.is_none(),
            invalid("Discogs token must be null when Discogs is disabled")
        );
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
                let entry: Option<(String, Option<i64>, bool)> = transaction.query_row(
                    "SELECT q.path,CASE WHEN q.stream_title IS NULL THEN
                     (SELECT t.album_id FROM library_tracks t JOIN library_assets a ON a.id=t.asset_id
                     JOIN library_roots r ON r.id=a.root_id WHERE a.path=q.path AND a.indexed=1 AND a.last_seen_scan>=r.completed_scan)
                     END,q.stream_title IS NOT NULL
                     FROM queue q WHERE q.id=?1", [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
                let (path, album, stream) =
                    entry.ok_or_else(|| invalid("queue entry is unknown or removed"))?;
                if stream {
                    transaction.commit()?;
                    return Ok(Target {
                        paths: Vec::new(),
                        release_id: None,
                        album: String::new(),
                        artist: String::new(),
                        key: String::new(),
                        direct: false,
                    });
                }
                (album, Some(PathBuf::from(path)))
            }
            _ => return Err(invalid("artwork requires exactly one album_id or entry_id")),
        };
        let mut hash = Sha256::new();
        hash.update(b"local-artwork-v2\0");
        let mut paths = Vec::new();
        let mut release_id = None;
        let mut album_title = String::new();
        let mut artist = String::new();
        if let Some(album) = album {
            let identity: Option<(String, Option<String>, String, String)> = transaction
                .query_row(
                    "SELECT identity,musicbrainz_release_id,title,album_artist FROM library_albums WHERE id=?1",
                    [album],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let (identity, release, title, album_artist) =
                identity.ok_or_else(|| invalid("album is unknown or unavailable"))?;
            hash.update(identity.as_bytes());
            hash.update(title.as_bytes());
            hash.update([0]);
            hash.update(album_artist.as_bytes());
            release_id = release;
            album_title = title;
            artist = album_artist;
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
            album: album_title,
            artist,
            key: format!("local:{:x}", hash.finalize()),
            direct: album.is_none(),
        })
    }
    fn load(&mut self, target: Target, request: &Request) -> Artwork {
        let size = request.size;
        let deadline = Instant::now() + Duration::from_secs(26);
        if target.paths.is_empty() {
            return Artwork::missing();
        }
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
            let mut release_id = target.release_id;
            let mut album = target.album;
            let mut artist = target.artist;
            if target.direct && images.is_none() {
                let probed = crate::metadata::probe_before(&target.paths[0], &self.stop, deadline);
                if self.stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return Artwork::unavailable(
                        "artwork discovery cancelled or deadline exceeded",
                    );
                }
                match probed {
                    Ok(metadata) => {
                        release_id = metadata.musicbrainz_release_id;
                        album = metadata.album;
                        artist = metadata.album_artist;
                    }
                    Err(error) => {
                        failure.get_or_insert_with(|| format!("{error:#}"));
                    }
                }
            }
            let local = Local {
                images,
                release_id,
                album,
                artist,
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
        let mut failure = local.failure;
        // The order and credential identity are part of the remote namespace. A
        // disabled provider cannot be selected, even when old cache rows survive.
        let mut hash = Sha256::new();
        hash.update(b"remote-artwork-v2\0");
        for identity in [
            local.release_id.as_deref().unwrap_or(""),
            &local.album,
            &local.artist,
        ] {
            hash.update((identity.len() as u64).to_le_bytes());
            hash.update(identity.as_bytes());
        }
        for provider in &request.providers {
            hash.update([match provider {
                ArtworkProvider::CoverArtArchive => 0,
                ArtworkProvider::Deezer => 1,
                ArtworkProvider::Discogs => 2,
            }]);
        }
        if let Some(token) = &request.discogs_token {
            hash.update(token.expose().as_bytes());
        }
        let namespace = format!("{:x}", hash.finalize());
        for selected in &request.providers {
            if self.stop.load(Ordering::Acquire) {
                return Artwork::unavailable("artwork request cancelled");
            }
            let key = format!("remote:{namespace}:{selected:?}");
            let cached = self
                .cache
                .as_ref()
                .and_then(|cache| cache.get::<Online>(&key));
            let online = if let Some(cached) = cached {
                cached
            } else {
                let gate = Arc::clone(&self.provider_gate);
                let _fetch = loop {
                    if self.stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                        return Artwork::unavailable(
                            "artwork request cancelled or deadline exceeded",
                        );
                    }
                    match gate.try_lock() {
                        Ok(guard) => break guard,
                        Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                        Err(std::sync::TryLockError::WouldBlock) => {
                            thread::sleep(Duration::from_millis(10))
                        }
                    }
                };
                // Another worker may have filled persistence while we waited.
                if let Some(cached) = self
                    .cache
                    .as_ref()
                    .and_then(|cache| cache.get::<Online>(&key))
                {
                    cached
                } else {
                    let online = self.fetch_remote(
                        *selected,
                        &local.release_id,
                        &local.album,
                        &local.artist,
                        request.discogs_token.as_ref(),
                        deadline,
                    );
                    let ttl = match &online {
                        Ok(Some(images)) => images
                            .display
                            .expires_at
                            .map(|expires| expires.saturating_sub(unix_time())),
                        Ok(None) => Some(NEGATIVE_TTL),
                        Err(_) => Some(FAILURE_TTL),
                    };
                    // Deezer permits active display only, never a reusable image cache.
                    if !matches!(&online, Ok(Some(images)) if images.display.cache_policy == ArtworkCachePolicy::DisplayOnly)
                        && let Some(cache) = &mut self.cache
                    {
                        cache.put(&key, &online, ttl);
                    }
                    online
                }
            };
            match online {
                Ok(Some(images)) => {
                    let mut artwork = images.select(size);
                    if artwork
                        .expires_at
                        .is_some_and(|expires| expires <= unix_time())
                    {
                        failure = Some("Artwork provider image expired".into());
                        continue;
                    }
                    artwork.message = failure;
                    return artwork;
                }
                Ok(None) => {}
                Err(message) => {
                    // Preserve actionable authentication failures through fallback.
                    if !failure
                        .as_deref()
                        .is_some_and(|message| message.contains("Discogs"))
                    {
                        failure = Some(message);
                    }
                }
            }
        }
        failure
            .map(Artwork::unavailable)
            .unwrap_or_else(Artwork::missing)
    }

    fn fetch_remote(
        &self,
        selected: ArtworkProvider,
        release: &Option<String>,
        album: &str,
        artist: &str,
        token: Option<&DiscogsToken>,
        deadline: Instant,
    ) -> Online {
        let provider = match &self.provider {
            Ok(provider) => provider.before(deadline),
            Err(message) => return Err(message.clone()),
        };
        let (bytes, source, source_url) = match selected {
            ArtworkProvider::CoverArtArchive => {
                let Some(release) = release else {
                    return Ok(None);
                };
                let release = match uuid::Uuid::parse_str(release) {
                    Ok(id) => id.hyphenated().to_string(),
                    Err(_) => return Err("invalid MusicBrainz release ID".into()),
                };
                match provider.fetch_front(&release) {
                    FetchOutcome::Found(bytes) => (
                        bytes,
                        ArtworkSource::CoverArtArchive,
                        format!("https://musicbrainz.org/release/{release}"),
                    ),
                    FetchOutcome::Missing => return Ok(None),
                    FetchOutcome::Unavailable(message) => return Err(message),
                }
            }
            ArtworkProvider::Deezer | ArtworkProvider::Discogs => {
                if album.trim().is_empty()
                    || artist.trim().is_empty()
                    || album == "Unknown Album"
                    || artist == "Unknown Artist"
                    || album.len() > 4096
                    || artist.len() > 4096
                {
                    return Ok(None);
                }
                let outcome = if selected == ArtworkProvider::Deezer {
                    provider.fetch_deezer(album, artist)
                } else {
                    let Some(token) = token else {
                        return Err("Discogs requires a saved token".into());
                    };
                    provider.fetch_discogs(album, artist, token.expose())
                };
                match outcome {
                    Ok(Some(image)) => (
                        image.bytes,
                        if selected == ArtworkProvider::Deezer {
                            ArtworkSource::Deezer
                        } else {
                            ArtworkSource::Discogs
                        },
                        image.source_url,
                    ),
                    Ok(None) => return Ok(None),
                    Err(message) => return Err(message.into()),
                }
            }
        };
        let expires_at = (selected == ArtworkProvider::Discogs).then(|| unix_time() + 6 * 60 * 60);
        match discovery::normalize(&bytes, source) {
            Ok(mut images) => {
                for artwork in [&mut images.thumbnail, &mut images.display] {
                    artwork.source_url = Some(source_url.clone());
                    artwork.expires_at = expires_at;
                    artwork.cache_policy = if selected == ArtworkProvider::Deezer {
                        ArtworkCachePolicy::DisplayOnly
                    } else {
                        ArtworkCachePolicy::Persistent
                    };
                    // Identical pixels from different providers have different policies.
                    artwork.cache_key = artwork
                        .cache_key
                        .take()
                        .map(|key| format!("{selected:?}:{key}"));
                }
                Ok(Some(images))
            }
            Err(error) => Err(format!("invalid provider artwork: {error:#}")),
        }
    }
}
fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
