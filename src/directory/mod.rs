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
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use uuid::Uuid;

const QUEUE_CAPACITY: usize = 4;
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

enum Work {
    Search {
        query: String,
        reply: oneshot::Sender<Reply>,
        cancel: Cancellation,
    },
    Click {
        uuid: String,
    },
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
#[derive(Clone)]
pub(crate) struct Handle {
    sender: mpsc::SyncSender<Work>,
    stop: Arc<AtomicBool>,
}
pub(crate) struct DirectoryWorkers {
    handle: Handle,
    thread: Option<thread::JoinHandle<()>>,
}
impl DirectoryWorkers {
    pub(crate) fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Work>(QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("directory-0".into())
            .spawn(move || {
                let mut provider = None;
                while !worker_stop.load(Ordering::Acquire) {
                    let work = match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(work) => work,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if provider.is_none() {
                        provider = Some(Provider::new().map_err(|e| format!("{e:#}")));
                    }
                    let Some(provider) = &provider else { continue };
                    match (work, provider) {
                        (Work::Search { reply, cancel, .. }, _)
                            if reply.is_closed() || cancel.cancelled() => {}
                        (
                            Work::Search {
                                query,
                                reply,
                                cancel,
                            },
                            Ok(provider),
                        ) => {
                            let _ = reply.send(match provider.search(&query, &cancel) {
                                Ok(stations) => Reply::Ok {
                                    result: Response::DirectoryResults { stations },
                                },
                                Err(error) => unavailable(format!("{error:#}")),
                            });
                        }
                        (Work::Search { reply, .. }, Err(error)) => {
                            let _ = reply.send(unavailable(error.clone()));
                        }
                        (Work::Click { uuid }, Ok(provider)) => provider.count_click(&uuid),
                        (Work::Click { .. }, Err(_)) => {}
                    }
                }
            })
            .ok();
        Self {
            handle: Handle { sender, stop },
            thread,
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
        if let Some(thread) = self.thread.take() {
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
        let work = Work::Search {
            query: query.to_owned(),
            reply,
            cancel,
        };
        if self.sender.try_send(work).is_err() {
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
        let _ = self.sender.try_send(Work::Click { uuid });
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
