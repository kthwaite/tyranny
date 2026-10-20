//! Bounded, cancellable lyrics discovery runs independently of the playback owner.
mod cache;
mod discovery;
mod provider;
#[cfg(test)]
mod tests;

use crate::protocol::{ErrorCode, Lyrics, LyricsSource, LyricsStatus, Reply, Response};
use anyhow::{Result, ensure};
use provider::Provider;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::MetadataExt,
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
const QUEUE_CAPACITY: usize = 8;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(8);
const NEGATIVE_TTL: u64 = 24 * 60 * 60;
const FAILURE_TTL: u64 = 60;

#[derive(Clone, Copy)]
pub(crate) struct Request {
    pub entry_id: u64,
    pub library_revision: u64,
}
#[derive(Clone)]
struct Cancellation {
    stop: Arc<AtomicBool>,
    abandoned: Arc<AtomicBool>,
}
impl Cancellation {
    #[cfg(test)]
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            abandoned: Arc::new(AtomicBool::new(false)),
        }
    }
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Acquire) || self.abandoned.load(Ordering::Acquire)
    }
    fn check(&self, deadline: Instant) -> Result<()> {
        ensure!(!self.cancelled(), "lyrics request cancelled");
        ensure!(
            Instant::now() < deadline,
            "lyrics discovery deadline exceeded"
        );
        Ok(())
    }
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct Work {
    request: Request,
    reply: oneshot::Sender<Reply>,
    cancel: Cancellation,
}
#[derive(Clone)]
pub(crate) struct Handle {
    sender: mpsc::SyncSender<Work>,
    stop: Arc<AtomicBool>,
}
pub(crate) struct LyricsWorkers {
    handle: Handle,
    threads: Vec<thread::JoinHandle<()>>,
}
impl LyricsWorkers {
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
            let (receiver, stop, provider, database, provider_gate) = (
                Arc::clone(&receiver),
                Arc::clone(&stop),
                Arc::clone(&provider),
                database.clone(),
                Arc::clone(&provider_gate),
            );
            if let Ok(thread) = thread::Builder::new()
                .name(format!("lyrics-{index}"))
                .spawn(move || {
                    let mut engine = None;
                    while !stop.load(Ordering::Acquire) {
                        let work = match receiver
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .recv_timeout(Duration::from_millis(100))
                        {
                            Ok(work) => work,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        };
                        if work.reply.is_closed() || work.cancel.cancelled() {
                            continue;
                        }
                        if engine.is_none() {
                            let provider = provider
                                .get_or_init(|| Provider::new().map_err(|e| format!("{e:#}")))
                                .clone();
                            match Engine::open(&database, provider, Arc::clone(&provider_gate)) {
                                Ok(opened) => engine = Some(opened),
                                Err(error) => {
                                    let _ = work
                                        .reply
                                        .send(ok(Lyrics::unavailable(format!("{error:#}"))));
                                    continue;
                                }
                            }
                        }
                        if let Some(engine) = &mut engine {
                            let result = engine.request(work.request, &work.cancel);
                            let _ = work.reply.send(result);
                        }
                    }
                })
            {
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
impl Drop for LyricsWorkers {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Handle {
    pub(crate) async fn request(&self, request: Request) -> Reply {
        if self.stop.load(Ordering::Acquire) {
            return ok(Lyrics::unavailable("lyrics workers stopped"));
        }
        let (reply, receive) = oneshot::channel();
        let abandoned = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOnDrop(Arc::clone(&abandoned));
        let cancel = Cancellation {
            stop: Arc::clone(&self.stop),
            abandoned,
        };
        if self
            .sender
            .try_send(Work {
                request,
                reply,
                cancel,
            })
            .is_err()
        {
            return ok(Lyrics::unavailable(
                "lyrics request queue is full or unavailable",
            ));
        }
        receive
            .await
            .unwrap_or_else(|_| ok(Lyrics::unavailable("lyrics request cancelled")))
    }
}
fn ok(lyrics: Lyrics) -> Reply {
    Reply::Ok {
        result: Response::Lyrics { lyrics },
    }
}
impl Lyrics {
    fn missing() -> Self {
        Self {
            status: LyricsStatus::Missing,
            source: None,
            plain_text: None,
            synced_lrc: None,
            attribution_url: None,
            message: None,
        }
    }
    fn unavailable(message: impl Into<String>) -> Self {
        let mut message = message.into();
        let mut end = message.len().min(1024);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        Self {
            status: LyricsStatus::Unavailable,
            message: Some(message),
            ..Self::missing()
        }
    }
}
#[derive(Debug)]
struct TargetError {
    code: ErrorCode,
    message: String,
}
impl std::fmt::Display for TargetError {
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
struct Target {
    path: Option<PathBuf>,
    key: String,
}
struct Engine {
    database: Connection,
    cache: Option<cache::Cache>,
    cache_path: PathBuf,
    provider: std::result::Result<Provider, String>,
    provider_gate: Arc<Mutex<()>>,
}
impl Engine {
    fn open(
        path: &Path,
        provider: std::result::Result<Provider, String>,
        provider_gate: Arc<Mutex<()>>,
    ) -> Result<Self> {
        let database = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        database.busy_timeout(Duration::from_millis(100))?;
        let cache_path = path.with_extension("lyrics.sqlite");
        Ok(Self {
            database,
            cache: cache::Cache::open(&cache_path).ok(),
            cache_path,
            provider,
            provider_gate,
        })
    }
    fn request(&mut self, request: Request, cancel: &Cancellation) -> Reply {
        if self.cache.is_none() {
            self.cache = cache::Cache::open(&self.cache_path).ok();
        }
        let result = self.resolve(request).and_then(|target| {
            let lyrics = self.load(&target, cancel);
            // Revalidate queue occurrence, library revision and real file identity
            // after slow IO; the client independently follows the current entry.
            let current = self.resolve(request)?;
            ensure!(
                current.key == target.key && current.path == target.path,
                invalid("lyrics source changed during discovery")
            );
            Ok(lyrics)
        });
        match result {
            Ok(lyrics) => ok(lyrics),
            Err(error) => {
                if let Some(error) = error.downcast_ref::<TargetError>() {
                    let queue_revision = self
                        .database
                        .query_row("SELECT revision FROM control WHERE singleton=1", [], |r| {
                            r.get(0)
                        })
                        .unwrap_or(0);
                    Reply::Error {
                        code: error.code,
                        message: error.message.clone(),
                        queue_revision,
                    }
                } else {
                    ok(Lyrics::unavailable(format!("{error:#}")))
                }
            }
        }
    }
    fn resolve(&mut self, request: Request) -> Result<Target> {
        let id = i64::try_from(request.entry_id).map_err(|_| invalid("invalid queue entry id"))?;
        let tx = self.database.transaction()?;
        let revision: u64 = tx.query_row(
            "SELECT revision FROM library_state WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if revision != request.library_revision {
            return Err(TargetError {
                code: ErrorCode::StaleLibraryRevision,
                message: format!("library revision is {revision}"),
            }
            .into());
        }
        let entry: Option<(String, bool)> = tx
            .query_row(
                "SELECT path,stream_title IS NOT NULL FROM queue WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (path, stream) =
            entry.ok_or_else(|| invalid("lyrics queue occurrence is unknown or removed"))?;
        if stream {
            tx.commit()?;
            return Ok(Target {
                path: None,
                key: String::new(),
            });
        }
        let scan: u64 = tx.query_row(
            "SELECT COALESCE(MAX(completed_scan),0) FROM library_roots",
            [],
            |r| r.get(0),
        )?;
        tx.commit()?;
        let path = PathBuf::from(path);
        ensure!(path.is_absolute(), "lyrics source must be an absolute path");
        let metadata = std::fs::metadata(&path)?;
        ensure!(metadata.is_file(), "lyrics source must be a regular file");
        let mut hash = Sha256::new();
        hash.update(b"local-lyrics-v1\0");
        hash.update(path.as_os_str().as_encoded_bytes());
        for value in [
            revision,
            scan,
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ] {
            hash.update(value.to_le_bytes());
        }
        Ok(Target {
            path: Some(path),
            key: format!("local:{:x}", hash.finalize()),
        })
    }
    fn load(&mut self, target: &Target, cancel: &Cancellation) -> Lyrics {
        let Some(path) = &target.path else {
            return Lyrics::missing();
        };
        let local: discovery::Local = if let Some(local) =
            self.cache.as_ref().and_then(|cache| cache.get(&target.key))
        {
            local
        } else {
            let local = match discovery::probe(path, cancel, Instant::now() + DISCOVERY_TIMEOUT) {
                Ok(local) => local,
                // Failed embedded discovery must not authorize lower-priority lyrics.
                Err(error) => return Lyrics::unavailable(format!("{error:#}")),
            };
            if let Some(cache) = &mut self.cache {
                cache.put(&target.key, &local, None);
            }
            local
        };
        if let Some(lyrics) = local.lyrics {
            return lyrics;
        }
        if !local.identity.eligible() {
            return Lyrics::missing();
        }
        if cancel.cancelled() {
            return Lyrics::unavailable("lyrics request cancelled");
        }
        let mut hash = Sha256::new();
        hash.update(b"lrclib-v2\0");
        // Remote results belong to the metadata identity, not a file or scan.
        // Local probing above still invalidates independently and always wins.
        // JSON preserves field boundaries and exact tag identity in the key.
        hash.update(
            serde_json::to_vec(&local.identity).expect("finite eligible metadata serializes"),
        );
        let key = format!("online:{:x}", hash.finalize());
        if let Some(lyrics) = self.cache.as_ref().and_then(|cache| cache.get(&key)) {
            return lyrics;
        }
        let deadline = Instant::now() + Duration::from_secs(7);
        let gate = Arc::clone(&self.provider_gate);
        let _fetch = loop {
            if let Err(error) = cancel.check(deadline) {
                return Lyrics::unavailable(error.to_string());
            }
            match gate.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::WouldBlock) => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(_) => return Lyrics::unavailable("lyrics provider lock unavailable"),
            }
        };
        if let Some(lyrics) = self.cache.as_ref().and_then(|cache| cache.get(&key)) {
            return lyrics;
        }
        let retry = self
            .cache
            .as_ref()
            .and_then(|cache| cache.get::<u64>("provider-retry"))
            .unwrap_or(0);
        let (lyrics, retry) = match &self.provider {
            Ok(provider) => provider.fetch(&local.identity, cancel, retry),
            Err(message) => (Lyrics::unavailable(message.clone()), retry),
        };
        if let Some(cache) = &mut self.cache {
            let remaining = retry.saturating_sub(unix_time());
            if remaining > 0 {
                cache.put("provider-retry", &retry, Some(remaining));
            }
        }
        if !cancel.cancelled() {
            let ttl = match lyrics.status {
                LyricsStatus::Ready | LyricsStatus::Instrumental => None,
                LyricsStatus::Missing => Some(NEGATIVE_TTL),
                LyricsStatus::Unavailable => Some(FAILURE_TTL),
            };
            if let Some(cache) = &mut self.cache {
                cache.put(&key, &lyrics, ttl);
            }
        }
        lyrics
    }
}
fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
