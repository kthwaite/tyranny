//! Station lookup against radio-browser.info, independent of the playback owner.
mod mirrors;
mod provider;
#[cfg(test)]
mod tests;

use crate::protocol::{ErrorCode, Reply, Response};
use anyhow::{Result, ensure};
use provider::Provider;
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use uuid::Uuid;

const QUEUE_CAPACITY: usize = 4;
const CLICK_QUEUE_CAPACITY: usize = 4;
const MAX_QUERY_BYTES: usize = 100;

#[derive(Clone)]
pub(crate) struct Cancellation {
    stop: Arc<AtomicBool>,
    abandoned: Arc<AtomicBool>,
}
impl Cancellation {
    pub(crate) fn new(stop: Arc<AtomicBool>, abandoned: Arc<AtomicBool>) -> Self {
        Self { stop, abandoned }
    }
    #[cfg(test)]
    fn idle() -> Self {
        Self::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
    }
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Acquire) || self.abandoned.load(Ordering::Acquire)
    }
    fn check(&self, deadline: Instant) -> Result<()> {
        ensure!(!self.cancelled(), "directory request cancelled");
        ensure!(
            Instant::now() < deadline,
            "directory search deadline exceeded"
        );
        Ok(())
    }
}

struct Search {
    query: String,
    reply: oneshot::Sender<Reply>,
    cancel: Cancellation,
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
#[derive(Clone)]
pub(crate) struct Handle {
    searches: mpsc::SyncSender<Search>,
    // Clicks have their own queue and worker, so they never make a search wait or fail.
    clicks: mpsc::SyncSender<String>,
    stop: Arc<AtomicBool>,
}
pub(crate) struct DirectoryWorkers {
    handle: Handle,
    threads: Vec<thread::JoinHandle<()>>,
}
type SharedProvider = Arc<OnceLock<std::result::Result<Provider, String>>>;

/// Builds the provider on the first worker that needs it, on a worker thread: the blocking
/// client must not be created or dropped inside the async runtime.
fn provider(shared: &SharedProvider) -> &std::result::Result<Provider, String> {
    shared.get_or_init(|| Provider::new().map_err(|e| format!("{e:#}")))
}
/// Runs `handle` for each item until `stop` is set or every sender is gone.
fn spawn_worker<T: Send + 'static>(
    name: &str,
    receiver: mpsc::Receiver<T>,
    stop: Arc<AtomicBool>,
    mut handle: impl FnMut(T) + Send + 'static,
) -> Option<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(item) => handle(item),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .ok()
}
impl DirectoryWorkers {
    pub(crate) fn start() -> Self {
        let (searches, search_receiver) = mpsc::sync_channel::<Search>(QUEUE_CAPACITY);
        let (clicks, click_receiver) = mpsc::sync_channel::<String>(CLICK_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let shared: SharedProvider = Arc::default();
        let search_provider = Arc::clone(&shared);
        let click_provider = shared;
        let click_cancel = Cancellation::new(Arc::clone(&stop), Arc::default());
        let threads = [
            spawn_worker(
                "directory-0",
                search_receiver,
                Arc::clone(&stop),
                move |Search {
                          query,
                          reply,
                          cancel,
                      }| {
                    if reply.is_closed() || cancel.cancelled() {
                        return;
                    }
                    let _ = reply.send(match provider(&search_provider) {
                        Ok(provider) => match provider.search(&query, &cancel) {
                            Ok(stations) => Reply::Ok {
                                result: Response::DirectoryResults { stations },
                            },
                            Err(error) => unavailable(format!("{error:#}")),
                        },
                        Err(error) => unavailable(error.clone()),
                    });
                },
            ),
            spawn_worker(
                "directory-clicks",
                click_receiver,
                Arc::clone(&stop),
                move |uuid: String| {
                    if let Ok(provider) = provider(&click_provider) {
                        provider.count_click(&uuid, &click_cancel);
                    }
                },
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        Self {
            handle: Handle {
                searches,
                clicks,
                stop,
            },
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
impl Drop for DirectoryWorkers {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Handle {
    /// Error replies carry `queue_revision` 0; the controller fills in the real one.
    pub(crate) async fn search(&self, query: &str) -> Reply {
        let query = match validate_query(query) {
            Ok(query) => query,
            Err(message) => return error(ErrorCode::InvalidCommand, message),
        };
        if self.stop.load(Ordering::Acquire) {
            return unavailable("radio-browser.info lookup stopped".into());
        }
        let (reply, receive) = oneshot::channel();
        let abandoned = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOnDrop(Arc::clone(&abandoned));
        let cancel = Cancellation::new(Arc::clone(&self.stop), abandoned);
        let work = Search {
            query: query.to_owned(),
            reply,
            cancel,
        };
        if self.searches.try_send(work).is_err() {
            return unavailable("radio-browser.info lookup queue is full or unavailable".into());
        }
        receive
            .await
            .unwrap_or_else(|_| unavailable("radio-browser.info lookup cancelled".into()))
    }
    /// Replies once the click is queued or dropped; never waits for the HTTP request.
    pub(crate) fn count_click(&self, station_uuid: &str) -> Reply {
        let Some(uuid) = normalize_uuid(station_uuid) else {
            return error(ErrorCode::InvalidCommand, "invalid station uuid".into());
        };
        let _ = self.clicks.try_send(uuid);
        Reply::Ok {
            result: Response::DirectoryClickCounted,
        }
    }
}
/// The hyphenated form is the only one the provider will request.
fn normalize_uuid(uuid: &str) -> Option<String> {
    Uuid::parse_str(uuid)
        .ok()
        .map(|uuid| uuid.hyphenated().to_string())
}
fn validate_query(query: &str) -> std::result::Result<&str, String> {
    let query = query.trim();
    if query.is_empty() || query.len() > MAX_QUERY_BYTES || query.chars().any(char::is_control) {
        return Err(format!(
            "query must be 1 to {MAX_QUERY_BYTES} bytes with no control characters"
        ));
    }
    Ok(query)
}
fn error(code: ErrorCode, message: String) -> Reply {
    Reply::Error {
        code,
        message,
        queue_revision: 0,
    }
}
fn unavailable(mut message: String) -> Reply {
    if !message.contains("radio-browser.info") {
        message = format!("radio-browser.info lookup failed: {message}");
    }
    let mut end = message.len().min(1024);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    error(ErrorCode::Unavailable, message)
}
