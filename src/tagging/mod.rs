mod acoustid;
mod engine;
mod fingerprint;
mod http;
mod journal;
mod matching;
mod musicbrainz;
mod proposal;
mod safe_write;
pub mod tags;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use crate::cancel::{CancelOnDrop, Cancellation, Stop};
use crate::library::LibraryHandle;
use crate::library_types::LibraryCommand;
use crate::protocol::{
    ErrorCode, Reply, Response, TagApplyResult, TagEditSummary, TagField, TagProposal,
};
use std::{
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

pub(crate) const MAX_ALBUM_TRACKS: usize = 500;
pub(crate) const LOOKUP_DEADLINE: Duration = Duration::from_secs(50);
const QUEUE_CAPACITY: usize = 4;

pub(crate) struct LookupRequest {
    pub album_id: u64,
    pub library_revision: u64,
    pub release_id: Option<String>,
    pub fingerprint: bool,
    pub acoustid_key: Option<String>,
}
/// Writes a stored proposal's values. Only the proposal's values are written.
pub(crate) struct ApplyRequest {
    pub proposal_id: String,
    pub track_ids: Vec<u64>,
    pub omit_fields: Vec<TagField>,
}
pub(crate) enum TagRequest {
    Lookup(LookupRequest),
    Apply(ApplyRequest),
    Edits,
    Revert { edit_id: u64 },
}
pub(crate) enum TagOutcome {
    Proposal(TagProposal),
    /// `written` holds the paths of the files that were written, to re-index.
    Applied {
        result: TagApplyResult,
        written: Vec<PathBuf>,
    },
    Edits(Vec<TagEditSummary>),
}
#[derive(Debug)]
pub(crate) struct Failure {
    pub code: ErrorCode,
    pub message: String,
    pub queue_revision: u64,
}
impl Failure {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            queue_revision: 0,
        }
    }
}

struct Work {
    request: TagRequest,
    reply: oneshot::Sender<Result<TagOutcome, Failure>>,
    cancel: Cancellation,
    /// A lookup's deadline, from the moment it was queued.
    deadline: Instant,
}
impl Work {
    /// Only a lookup is dropped when its requester has left (F2). Other work
    /// runs to the end once it is queued, and its reply may go nowhere.
    fn abandoned(&self) -> bool {
        match self.request {
            TagRequest::Lookup(_) => self.reply.is_closed() || self.cancel.cancelled(),
            TagRequest::Apply(_) | TagRequest::Edits | TagRequest::Revert { .. } => false,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Handle {
    sender: mpsc::SyncSender<Work>,
    stop: Arc<AtomicBool>,
    /// Re-indexes the files that an apply or a revert wrote.
    library: Option<LibraryHandle>,
}
pub(crate) struct TaggingWorkers {
    handle: Handle,
    thread: Option<thread::JoinHandle<()>>,
}
impl TaggingWorkers {
    pub(crate) fn start(database: PathBuf, library: LibraryHandle) -> Self {
        Self::with_providers(database, None, Some(library))
    }
    /// `None` builds the live providers in the worker, on first use.
    fn with_providers(
        database: PathBuf,
        providers: Option<engine::Providers>,
        library: Option<LibraryHandle>,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Work>(QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("tagging-0".into())
                .spawn(move || run(&database, providers, &receiver, &stop))
                .ok()
        };
        Self {
            handle: Handle {
                sender,
                stop,
                library,
            },
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
impl Drop for TaggingWorkers {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The worker loop. The engine, and the live providers when none are given,
/// are made on the first request.
fn run(
    database: &std::path::Path,
    providers: Option<engine::Providers>,
    receiver: &mpsc::Receiver<Work>,
    stop: &AtomicBool,
) {
    let mut providers = Some(providers);
    let mut engine = None;
    while !stop.load(Ordering::Acquire) {
        let work = match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(work) => work,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if work.abandoned() {
            continue;
        }
        let engine = engine.get_or_insert_with(|| match providers.take() {
            Some(providers) => {
                let providers = providers.unwrap_or_else(engine::Providers::live);
                engine::Engine::open(database, providers)
                    .map_err(|error| format!("tag lookups are unavailable: {error:#}"))
            }
            None => Err("tag lookups are unavailable".into()),
        });
        let result = match engine {
            Ok(engine) => match &work.request {
                TagRequest::Lookup(request) => engine
                    .lookup(request, &work.cancel, work.deadline)
                    .map(TagOutcome::Proposal),
                // Writes check only `stop`, and only between files.
                TagRequest::Apply(request) => engine
                    .apply(request, stop)
                    .map(|(result, written)| TagOutcome::Applied { result, written }),
                TagRequest::Revert { edit_id } => engine
                    .revert(*edit_id, stop)
                    .map(|(result, written)| TagOutcome::Applied { result, written }),
                TagRequest::Edits => engine.edits().map(TagOutcome::Edits),
            },
            Err(message) => Err(Failure::new(ErrorCode::Persistence, message.clone())),
        };
        let _ = work.reply.send(result);
    }
}

impl Handle {
    /// An apply or a revert re-indexes the files it wrote, in a detached task.
    /// So a dropped future or a gone client never loses the re-index (F3).
    pub(crate) async fn request(&self, request: TagRequest) -> Result<TagOutcome, Failure> {
        if self.stop.load(Ordering::Acquire) {
            return Err(stopped());
        }
        let library = match request {
            TagRequest::Apply(_) | TagRequest::Revert { .. } => self.library.clone(),
            TagRequest::Lookup(_) | TagRequest::Edits => None,
        };
        let (reply, receive) = oneshot::channel();
        let abandoned = Arc::new(AtomicBool::new(false));
        // Dropping a lookup's future stops it; other work is never abandoned.
        let _cancel =
            matches!(request, TagRequest::Lookup(_)).then(|| CancelOnDrop(Arc::clone(&abandoned)));
        let cancel = Cancellation::with_flags(Arc::clone(&self.stop), abandoned);
        let work = Work {
            request,
            reply,
            cancel,
            deadline: Instant::now() + LOOKUP_DEADLINE,
        };
        match self.sender.try_send(work) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                return Err(Failure::new(
                    ErrorCode::InvalidCommand,
                    "tag lookups are busy, try again shortly",
                ));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return Err(stopped()),
        }
        let Some(library) = library else {
            return receive.await.unwrap_or_else(|_| Err(stopped()));
        };
        let (forward, forwarded) = oneshot::channel();
        tokio::spawn(async move {
            let outcome = receive.await.unwrap_or_else(|_| Err(stopped()));
            if let Ok(TagOutcome::Applied { written, .. }) = &outcome
                && !written.is_empty()
            {
                let paths = written.clone();
                if let Err(error) = library.request(LibraryCommand::Reindex { paths }).await {
                    eprintln!("tyranny: re-index after tag write failed: {error}");
                }
            }
            // The requester may be gone; its outcome then goes nowhere.
            let _ = forward.send(outcome);
        });
        forwarded.await.unwrap_or_else(|_| Err(stopped()))
    }
}

fn stopped() -> Failure {
    Failure::new(ErrorCode::InvalidCommand, "tagging workers stopped")
}

/// The wire reply for a tagging outcome. A failure keeps its `queue_revision`.
pub(crate) fn reply(outcome: Result<TagOutcome, Failure>) -> Reply {
    match outcome {
        Ok(outcome) => Reply::Ok {
            result: match outcome {
                TagOutcome::Proposal(proposal) => Response::TagProposal { proposal },
                TagOutcome::Applied { result, .. } => Response::TagApply { result },
                TagOutcome::Edits(edits) => Response::TagEdits { edits },
            },
        },
        Err(Failure {
            code,
            message,
            queue_revision,
        }) => Reply::Error {
            code,
            message,
            queue_revision,
        },
    }
}

/// Why a provider lookup stopped. `Display` is the user-visible reason.
#[derive(Debug)]
pub(super) enum LookupError {
    Busy(&'static str),
    Cancelled,
    TimedOut,
    Failed(String),
}
impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(service) => write!(f, "{service} is busy, try again shortly"),
            Self::Cancelled => f.write_str("lookup cancelled"),
            Self::TimedOut => f.write_str("lookup took too long"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for LookupError {}

impl Cancellation {
    pub(super) fn check_lookup(&self, deadline: Instant) -> Result<(), LookupError> {
        match self.stopped(deadline) {
            Some(Stop::Cancelled) => Err(LookupError::Cancelled),
            Some(Stop::Expired) => Err(LookupError::TimedOut),
            None => Ok(()),
        }
    }
}
