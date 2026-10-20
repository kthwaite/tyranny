//! One thread owns playback and SQLite. Network tasks submit bounded work; no
//! controller, serializer, database call, or socket runs on the audio callback.
use crate::{
    SAMPLE_RATE,
    artwork::{ArtworkWorkers, Handle as ArtworkHandle, Request as ArtworkRequest},
    library::{Library, LibraryHandle},
    library_types::{
        LibraryCommand, LibraryError, LibraryErrorKind, PageRequest, StaleLibraryRevision,
    },
    output::OutputConfig,
    play_order::{RepeatMode, RepeatScope, ShuffleStrategy, UniformShuffle},
    playback::{CountedPlay, Player, PreparedRemove, Start},
    protocol::{
        Command, ControlState, ErrorCode, MAX_PAGE_SIZE, MAX_SOURCE_NAME_BYTES, Message,
        NowPlaying, Placement, QueueSource, Reply, Request, Response, Settings, SourceKind,
        StreamSpec, TrackSelection, Transport,
    },
    store::{
        Checkpoint, Edit, EntryTrack, LoadedEntry, PendingPlays, PlayRecord, PlaylistChange,
        PlaylistEdit, PlaylistFailure, Store, UpcomingEdit, played_overflow,
    },
};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    cell::RefCell,
    collections::HashSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, mpsc as startup},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, mpsc, oneshot};

mod resolve;
#[cfg(test)]
mod tests;
use resolve::{Resolution, Resolver};

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);
const EVENT_INTERVAL: Duration = Duration::from_millis(250);

pub struct OwnerConfig {
    pub database_path: PathBuf,
    pub output: OutputConfig,
}
pub struct Owner {
    controller: Controller,
    artwork: ArtworkWorkers,
    thread: thread::JoinHandle<Result<()>>,
}
#[derive(Clone)]
pub struct Controller {
    sender: mpsc::Sender<Work>,
    library: LibraryHandle,
    artwork: ArtworkHandle,
    resolver: Arc<Resolver>,
}
pub struct Subscription {
    pub snapshot: ControlState,
    pub events: broadcast::Receiver<ControlState>,
}
enum Work {
    Subscribe(oneshot::Sender<Subscription>),
    Request(Box<Request>, oneshot::Sender<Message>),
    /// A run selection command with its selection already resolved.
    Resolved(Box<Request>, Resolution, oneshot::Sender<Message>),
    Shutdown(oneshot::Sender<Result<()>>),
}

impl Owner {
    pub fn start(config: OwnerConfig) -> Result<Self> {
        let database = config.database_path.clone();
        let (sender, receiver) = mpsc::channel(64);
        let (ready_tx, ready_rx) = startup::sync_channel(1);
        let thread = thread::Builder::new()
            .name("playback-owner".into())
            .spawn(move || {
                let actor = Actor::open(config);
                match actor {
                    Ok(actor) => {
                        let ready = (actor.library.handle(), actor.capture_path.clone());
                        if ready_tx.send(Ok(ready)).is_err() {
                            return Ok(());
                        }
                        actor.run(receiver)
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("{error:#}")));
                        Err(error)
                    }
                }
            })
            .context("starting playback owner")?;
        let initialized = match ready_rx.recv() {
            Ok(result) => result.map_err(anyhow::Error::msg),
            Err(error) => Err(error.into()),
        };
        match initialized {
            Ok((library, capture)) => {
                let resolver = Arc::new(Resolver::new(database.clone(), capture));
                let artwork = ArtworkWorkers::start(database);
                Ok(Self {
                    controller: Controller {
                        sender,
                        library,
                        artwork: artwork.handle(),
                        resolver,
                    },
                    artwork,
                    thread,
                })
            }
            Err(error) => {
                let _ = thread.join();
                Err(error.context("playback owner initialization failed"))
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn start_with_artwork(config: OwnerConfig, artwork: ArtworkWorkers) -> Result<Self> {
        let mut owner = Self::start(config)?;
        owner.controller.artwork = artwork.handle();
        std::mem::replace(&mut owner.artwork, artwork).shutdown();
        Ok(owner)
    }
    pub fn controller(&self) -> Controller {
        self.controller.clone()
    }
    pub async fn shutdown(self) -> Result<()> {
        self.artwork.stop();
        let (reply_tx, reply_rx) = oneshot::channel();
        let sent = self.controller.sender.send(Work::Shutdown(reply_tx)).await;
        let result = if sent.is_ok() {
            reply_rx
                .await
                .unwrap_or_else(|_| Err(anyhow!("owner closed before shutdown checkpoint")))
        } else {
            Err(anyhow!("playback owner already closed"))
        };
        let joined = tokio::task::spawn_blocking(move || {
            self.artwork.shutdown();
            self.thread.join()
        })
        .await
        .context("joining owner task")?
        .map_err(|_| anyhow!("playback owner panicked"))?;
        result?;
        joined
    }
}
impl Controller {
    pub async fn subscribe(&self) -> Result<Subscription> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Work::Subscribe(reply))
            .context("playback owner busy or unavailable")?;
        receiver.await.context("playback owner closed")
    }
    pub async fn request(&self, request: Request) -> Result<Message> {
        let id = request.id;
        let command = match request.command {
            Command::Artwork {
                album_id,
                entry_id,
                library_revision,
                size,
            } => {
                let reply = self
                    .artwork
                    .request(ArtworkRequest {
                        album_id,
                        entry_id,
                        library_revision,
                        size,
                    })
                    .await;
                return Ok(Message::Reply { id, reply });
            }
            Command::AddRoot { path } => LibraryCommand::AddRoot { path },
            Command::Rescan { root_id } => LibraryCommand::Rescan { root_id },
            Command::LibraryRoots {
                revision,
                offset,
                limit,
                query,
            } => LibraryCommand::Roots(PageRequest {
                revision,
                offset,
                limit,
                query,
            }),
            Command::LibraryArtists {
                revision,
                offset,
                limit,
                query,
            } => LibraryCommand::Artists(PageRequest {
                revision,
                offset,
                limit,
                query,
            }),
            Command::LibraryAlbums {
                revision,
                offset,
                limit,
                query,
                artist_id,
            } => LibraryCommand::Albums {
                artist_id,
                page: PageRequest {
                    revision,
                    offset,
                    limit,
                    query,
                },
            },
            Command::LibraryTracks {
                revision,
                offset,
                limit,
                query,
                album_id,
            } => LibraryCommand::Tracks {
                album_id,
                page: PageRequest {
                    revision,
                    offset,
                    limit,
                    query,
                },
            },
            Command::ListTracks {
                list,
                revision,
                offset,
                limit,
            } => LibraryCommand::ListTracks {
                list,
                revision,
                offset,
                limit,
            },
            Command::ListAlbums {
                list,
                sort,
                descending,
                revision,
                offset,
                limit,
            } => LibraryCommand::ListAlbums {
                list,
                sort,
                descending,
                revision,
                offset,
                limit,
            },
            Command::ListArtists {
                query,
                list,
                revision,
                offset,
                limit,
            } => LibraryCommand::ListArtists {
                page: PageRequest {
                    revision,
                    offset,
                    limit,
                    query,
                },
                list,
            },
            Command::ListGenres {
                query,
                revision,
                offset,
                limit,
            } => LibraryCommand::ListGenres(PageRequest {
                revision,
                offset,
                limit,
                query,
            }),
            Command::AlbumSummaries {
                album_ids,
                revision,
            } => LibraryCommand::AlbumSummaries {
                album_ids,
                revision,
            },
            Command::ListGroups { list, revision } => LibraryCommand::ListGroups { list, revision },
            command @ (Command::EnqueueTracks { .. }
            | Command::PlaylistAddSelection { .. }
            | Command::EnqueuePlaylist { .. }) => {
                return self.run_request(id, command).await;
            }
            command => return self.playback_request(Request { id, command }).await,
        };
        let reply = match self.library.request(command).await {
            Ok(reply) => Reply::Ok {
                result: reply.into(),
            },
            Err(error) => {
                let snapshot = self
                    .playback_request(Request {
                        id,
                        command: Command::Snapshot,
                    })
                    .await?;
                let Message::Reply {
                    reply:
                        Reply::Ok {
                            result: Response::Snapshot { snapshot },
                        },
                    ..
                } = snapshot
                else {
                    return Err(anyhow!("owner did not return its control snapshot"));
                };
                let code = match error.kind {
                    LibraryErrorKind::Invalid | LibraryErrorKind::Busy => ErrorCode::InvalidCommand,
                    LibraryErrorKind::InvalidFilter => ErrorCode::InvalidFilter,
                    LibraryErrorKind::StaleRevision => ErrorCode::StaleLibraryRevision,
                    LibraryErrorKind::StalePlaylistRevision => ErrorCode::StalePlaylistRevision,
                    LibraryErrorKind::Storage => ErrorCode::Persistence,
                };
                Reply::Error {
                    code,
                    message: error.message,
                    queue_revision: snapshot.queue_revision,
                }
            }
        };
        Ok(Message::Reply { id, reply })
    }

    async fn playback_request(&self, request: Request) -> Result<Message> {
        self.submit(|reply| Work::Request(Box::new(request), reply))
            .await
    }

    /// Resolves the run selection off the playback owner. The owner only
    /// rechecks revisions and commits.
    async fn run_request(&self, id: u64, command: Command) -> Result<Message> {
        let (command, resolution) = Arc::clone(&self.resolver).resolve(command).await?;
        self.submit(|reply| Work::Resolved(Box::new(Request { id, command }), resolution, reply))
            .await
    }

    async fn submit(&self, work: impl FnOnce(oneshot::Sender<Message>) -> Work) -> Result<Message> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(work(reply))
            .context("playback owner busy or unavailable")?;
        receiver
            .await
            .context("playback owner closed before command result; do not replay automatically")
    }
}

struct Failure {
    code: ErrorCode,
    error: anyhow::Error,
    fatal: bool,
}
impl Failure {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidCommand,
            error: anyhow!(message.into()),
            fatal: false,
        }
    }
    fn playback(error: anyhow::Error) -> Self {
        Self {
            code: ErrorCode::Playback,
            error,
            fatal: false,
        }
    }
    fn persistence(error: anyhow::Error) -> Self {
        Self {
            code: ErrorCode::Persistence,
            error,
            fatal: false,
        }
    }
    fn playlist(error: anyhow::Error) -> Self {
        if let Some(failure) = error.downcast_ref::<PlaylistFailure>() {
            return Self {
                code: failure.code,
                error,
                fatal: false,
            };
        }
        Self::library(error)
    }
    fn library(error: anyhow::Error) -> Self {
        let kind = error.downcast_ref::<LibraryError>().map(|error| error.kind);
        let code = if error.is::<StaleLibraryRevision>() {
            ErrorCode::StaleLibraryRevision
        } else if error.is::<crate::filter::InvalidFilter>() {
            ErrorCode::InvalidFilter
        } else {
            match kind {
                Some(LibraryErrorKind::StaleRevision) => ErrorCode::StaleLibraryRevision,
                Some(LibraryErrorKind::StalePlaylistRevision) => ErrorCode::StalePlaylistRevision,
                Some(LibraryErrorKind::InvalidFilter) => ErrorCode::InvalidFilter,
                Some(LibraryErrorKind::Invalid | LibraryErrorKind::Busy) => {
                    ErrorCode::InvalidCommand
                }
                Some(LibraryErrorKind::Storage) | None => ErrorCode::Persistence,
            }
        };
        Self {
            code,
            error,
            fatal: false,
        }
    }
    fn committed(error: anyhow::Error) -> Self {
        Self {
            code: ErrorCode::Playback,
            error: error
                .context("durable edit committed but engine application failed; owner stopping"),
            fatal: true,
        }
    }
}
type CommandResult<T> = std::result::Result<T, Failure>;

fn same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

fn capture_destination(config: &OwnerConfig, entries: &[LoadedEntry]) -> Result<Option<PathBuf>> {
    let OutputConfig::Capture { path, .. } = &config.output else {
        return Ok(None);
    };
    let capture = match path.canonicalize() {
        Ok(path) => {
            ensure!(path.is_file(), "capture destination is not a regular file");
            path
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            parent
                .canonicalize()?
                .join(path.file_name().context("capture needs a filename")?)
        }
        Err(error) => return Err(error).context("resolving capture destination"),
    };
    let database = config.database_path.canonicalize()?;
    let mut protected = vec![
        database.clone(),
        database.with_file_name("token"),
        database.with_file_name("server.lock"),
    ];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = database.as_os_str().to_os_string();
        sidecar.push(suffix);
        protected.push(PathBuf::from(sidecar));
    }
    ensure!(
        !protected.iter().any(|path| same_file(&capture, path)),
        "capture destination overlaps private server state"
    );
    ensure!(
        !entries
            .iter()
            .any(|entry| entry.stream.is_none() && same_file(&capture, &entry.path)),
        "capture destination overlaps a queued source"
    );
    Ok(Some(capture))
}

/// The canonical form of a queued source. `capture` is the capture output,
/// which cannot also be a source.
fn source_path(source: PathBuf, capture: Option<&Path>) -> CommandResult<PathBuf> {
    if source.as_os_str().len() > 4096 || !source.is_absolute() {
        return Err(Failure::invalid(
            "source must be an absolute local path of at most 4096 bytes",
        ));
    }
    let source = source
        .canonicalize()
        .map_err(|error| Failure::invalid(format!("opening source: {error}")))?;
    if !source.is_file() {
        return Err(Failure::invalid("source is not a regular file"));
    }
    if capture.is_some_and(|capture| same_file(&source, capture)) {
        return Err(Failure::invalid(
            "capture output cannot also be a queued source",
        ));
    }
    let path = source
        .to_str()
        .ok_or_else(|| Failure::invalid("source path must be UTF-8"))?;
    if path.len() > 4096 {
        return Err(Failure::invalid("resolved source path exceeds 4096 bytes"));
    }
    Ok(source)
}

fn source_name(source: &QueueSource) -> CommandResult<()> {
    if source.name.len() > MAX_SOURCE_NAME_BYTES {
        return Err(Failure::invalid(format!(
            "source name exceeds {MAX_SOURCE_NAME_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

/// `(entry id, library revision)` and what the library said about that entry.
type NowTrack = ((u64, u64), Option<EntryTrack>);

/// The order shuffle uses on the server.
fn shuffle_strategy() -> Box<dyn ShuffleStrategy> {
    Box::new(UniformShuffle::from_entropy())
}

/// Why an entry is selected.
enum Jump {
    /// A new position in the current entry; nothing is marked.
    Seek,
    /// Next or Play: the entry becomes the newest played entry. A fresh
    /// Repeat All pass first clears the played state of `fresh`, its context.
    Forward { fresh: Vec<u64> },
    /// Previous: the entry becomes the newest played entry, and the play
    /// order's history steps back.
    Back,
}

struct Actor {
    player: Player,
    store: Store,
    settings: Settings,
    playlists_revision: u64,
    fault: Option<String>,
    last: ControlState,
    events: broadcast::Sender<ControlState>,
    capture_path: Option<PathBuf>,
    library: Library,
    /// Last library lookup for the playing entry, keyed by entry and library revision.
    now_track: RefCell<Option<NowTrack>>,
    /// The entry last made the newest played entry. Output starting it again
    /// (after a selection, or a seek) does not mark it twice.
    marked: Option<u64>,
    /// The newest play history ID, or 0.
    history_revision: u64,
    /// Counted plays whose history write has not succeeded yet.
    pending_plays: PendingPlays,
    /// The last history write failed: pending plays wait for the retry.
    retrying_plays: bool,
}
impl Actor {
    fn open(config: OwnerConfig) -> Result<Self> {
        let store = Store::open(&config.database_path)?;
        let loaded = store.load()?;
        let capture_path = capture_destination(&config, &loaded.entries)?;
        let mut player = Player::from_loaded(
            loaded.entries,
            loaded.revision,
            loaded.next_id,
            config.output,
            loaded.settings.gain_mode,
        )?;
        player.set_volume(loaded.settings.volume);
        player.set_repeat(loaded.settings.repeat);
        if loaded.settings.shuffle {
            player.set_shuffle(Some(shuffle_strategy()));
        }
        let mut fault = None;
        if let Some(id) = loaded.checkpoint.current_id {
            match player.prepare_selection(
                id,
                if player.is_stream(id) {
                    0
                } else {
                    loaded.checkpoint.position_frames
                },
                loaded.settings.gain_mode,
            ) {
                Ok(prepared) => {
                    player.commit_selection(prepared);
                    player.restored_selection();
                    if loaded.settings.resume_on_start && loaded.checkpoint.playing {
                        player.play();
                    }
                }
                Err(error) => {
                    fault = Some(format!("restoring selected entry {id}: {error:#}"));
                }
            }
        }
        let history_revision = store.history_revision()?;
        let library = Library::start(config.database_path)?;
        let (events, _) = broadcast::channel(32);
        let last = ControlState {
            sequence: 0,
            queue_revision: loaded.revision,
            playlists_revision: loaded.playlists_revision,
            queue_length: player.queue_entries().len(),
            current_id: None,
            can_next: false,
            can_previous: false,
            now_playing: None,
            position_frames: 0,
            sample_rate: SAMPLE_RATE,
            transport: Transport::Stopped,
            settings: loaded.settings.clone(),
            error: None,
            underruns: 0,
            library: library.state(),
            history_revision,
            current_index: None,
            upcoming_count: 0,
        };
        let mut actor = Self {
            player,
            store,
            settings: loaded.settings,
            playlists_revision: loaded.playlists_revision,
            fault,
            last,
            events,
            capture_path,
            library,
            now_track: RefCell::new(None),
            marked: loaded.checkpoint.current_id,
            history_revision,
            pending_plays: PendingPlays::default(),
            retrying_plays: false,
        };
        actor.publish();
        Ok(actor)
    }

    fn state(&self) -> ControlState {
        let state = self.player.snapshot();
        let error = self
            .player
            .error()
            .map(str::to_owned)
            .or_else(|| self.fault.clone());
        let transport = if error.is_some() {
            Transport::Error
        } else if state.stopped {
            Transport::Stopped
        } else if self.player.completed() {
            Transport::Ended
        } else if state.playing && !state.output.paused {
            Transport::Playing
        } else if state.playing {
            Transport::Buffering
        } else if state.current_id.is_some() {
            Transport::Paused
        } else {
            Transport::Stopped
        };
        let library = self.library.state();
        let now_playing = state
            .now_playing
            .map(|now| self.describe(now, library.revision));
        let (current_index, upcoming_count) = self.player.upcoming_summary(state.current_id);
        ControlState {
            sequence: self.last.sequence,
            queue_revision: state.queue_revision,
            playlists_revision: self.playlists_revision,
            queue_length: self.player.queue_entries().len(),
            current_id: state.current_id,
            can_next: state.next_id.is_some(),
            can_previous: state.previous_id.is_some(),
            now_playing,
            position_frames: state.position_frames,
            sample_rate: SAMPLE_RATE,
            transport,
            settings: self.settings.clone(),
            error,
            underruns: state.output.underruns,
            library,
            history_revision: self.history_revision,
            current_index,
            upcoming_count,
        }
    }
    /// Adds the indexed track, album, album artist, compilation flag and format to the
    /// player's metadata. The lookup is display-only, so a failed one leaves those fields blank.
    fn describe(&self, now: Arc<NowPlaying>, library_revision: u64) -> Arc<NowPlaying> {
        if self.player.is_stream(now.entry_id) {
            return now;
        }
        let key = (now.entry_id, library_revision);
        let mut cache = self.now_track.borrow_mut();
        if cache.as_ref().is_none_or(|(cached, _)| *cached != key) {
            *cache = Some((key, self.store.entry_track(now.entry_id).ok().flatten()));
        }
        let Some((_, Some(track))) = cache.as_ref() else {
            return now;
        };
        Arc::new(NowPlaying {
            track_id: Some(track.track_id),
            album_id: Some(track.album_id),
            album_artist_id: Some(track.album_artist_id),
            compilation: Some(track.compilation),
            format: track.format.clone(),
            ..(*now).clone()
        })
    }
    fn publish(&mut self) {
        let mut state = self.state();
        if state != self.last {
            state.sequence = self
                .last
                .sequence
                .checked_add(1)
                .expect("event sequence exhausted");
            self.last = state.clone();
            let _ = self.events.send(state);
        }
    }
    fn checkpoint(&self) -> Checkpoint {
        let state = self.player.snapshot();
        if state.stopped || self.player.completed() {
            return Checkpoint::default();
        }
        Checkpoint {
            current_id: state.current_id,
            position_frames: if state.current_id.is_some_and(|id| self.player.is_stream(id)) {
                0
            } else {
                state.position_frames
            },
            playing: state.playing && self.player.error().is_none() && self.fault.is_none(),
        }
    }
    fn save(&self) -> Result<()> {
        self.store.checkpoint(self.checkpoint(), &self.settings)
    }
    fn revision(&self) -> u64 {
        self.player.snapshot().queue_revision
    }
    fn check_revision(&self, expected: u64) -> CommandResult<()> {
        if expected != self.revision() {
            return Err(Failure {
                code: ErrorCode::StaleRevision,
                error: anyhow!(
                    "queue revision {expected} is stale; current {}",
                    self.revision()
                ),
                fatal: false,
            });
        }
        Ok(())
    }
    fn contains(&self, id: u64) -> CommandResult<()> {
        if !self.player.queue_entries().any(|entry| entry.0 == id) {
            return Err(Failure::invalid("queue occurrence not found"));
        }
        Ok(())
    }
    fn next_revision(&self) -> CommandResult<u64> {
        if self.revision() >= i64::MAX as u64 {
            return Err(Failure::invalid("queue revision exhausted"));
        }
        Ok(self.revision() + 1)
    }
    fn applied(&mut self, entry_id: Option<u64>) -> CommandResult<Response> {
        self.save().map_err(Failure::persistence)?;
        self.published(entry_id)
    }

    fn published(&mut self, entry_id: Option<u64>) -> CommandResult<Response> {
        self.publish();
        Ok(Response::Applied {
            snapshot: Box::new(self.last.clone()),
            entry_id,
        })
    }
    /// Unless `jump` is a seek, the selected entry becomes the newest played
    /// entry and the played prefix is trimmed, in the checkpoint's commit.
    fn select(
        &mut self,
        entry_id: u64,
        position_frames: u64,
        playing: bool,
        jump: Jump,
    ) -> CommandResult<Response> {
        let prepared = self
            .player
            .prepare_selection(entry_id, position_frames, self.settings.gain_mode)
            .map_err(Failure::playback)?;
        let checkpoint = Checkpoint {
            current_id: Some(entry_id),
            position_frames,
            playing,
        };
        let (seek, back) = (matches!(jump, Jump::Seek), matches!(jump, Jump::Back));
        let fresh = match jump {
            Jump::Forward { fresh } => fresh,
            _ => Vec::new(),
        };
        // `Some` once the entry is marked; holds the trim's player removal.
        let mut marked = None;
        let renews = fresh.iter().any(|entry| *entry != entry_id);
        if !seek && (self.marked != Some(entry_id) || renews) {
            // The trim can delete the entry that was current.
            self.collect_plays();
            let (trimmed, removal) = self
                .prepare_trim(entry_id, Some(entry_id))
                .map_err(Failure::persistence)?;
            self.store
                .mark_current(
                    self.revision(),
                    entry_id,
                    &trimmed,
                    &fresh,
                    Some(checkpoint),
                )
                .map_err(Failure::persistence)?;
            marked = Some(removal);
        } else {
            self.store
                .checkpoint(checkpoint, &self.settings)
                .map_err(Failure::persistence)?;
        }
        if marked.is_some() {
            // Before the selection decides what follows, in the new pass.
            self.player.clear_played(&fresh);
        }
        self.player.commit_selection(prepared);
        if let Some(removal) = marked {
            self.commit_trim(entry_id, removal);
        }
        if back {
            self.player.retreat_to(entry_id);
        }
        if playing {
            self.player.play();
        } else {
            self.player.pause();
        }
        self.fault = None;
        // Preparation and the single durable checkpoint are complete. A second
        // fallible save here could report rejection after the audible commit.
        self.published(None)
    }

    /// What making `id` current trims, and the player's removal of it.
    /// `anchor` is the entry the output treats as current when the trim
    /// commits. It can still be the entry before `id`, so it counts towards the
    /// retention, but it is never trimmed.
    fn prepare_trim(
        &self,
        id: u64,
        anchor: Option<u64>,
    ) -> Result<(Vec<u64>, Option<PreparedRemove>)> {
        let played = self.store.played_newest_first()?;
        let mut trimmed = self.player.trimmed(&played, id);
        trimmed.retain(|entry| Some(*entry) != anchor);
        if trimmed.is_empty() {
            return Ok((trimmed, None));
        }
        let removal = self
            .player
            .prepare_remove_batch(&trimmed, anchor, self.revision())?;
        Ok((trimmed, Some(removal)))
    }

    /// Applies a committed `mark_current` to the player.
    fn commit_trim(&mut self, id: u64, removal: Option<PreparedRemove>) {
        if let Some(removal) = removal {
            self.player.commit_remove_batch(removal);
        }
        self.player.mark_played(id);
        self.marked = Some(id);
    }

    /// Output started a pass over entry `id`: it becomes the newest played
    /// entry, and the played prefix is trimmed, in one small commit. A fresh
    /// Repeat All pass first clears the played state of `fresh`, its context.
    fn advance(&mut self, id: u64, fresh: &[u64]) -> Result<()> {
        if !self.player.queue_entries().any(|entry| entry.0 == id) {
            return Ok(());
        }
        // A selection, a seek or Repeat One starts the marked entry again.
        if self.marked == Some(id) && fresh.iter().all(|entry| *entry == id) {
            return Ok(());
        }
        let anchor = self.player.snapshot().current_id;
        let (trimmed, removal) = self.prepare_trim(id, anchor)?;
        self.store
            .mark_current(self.revision(), id, &trimmed, fresh, None)?;
        self.player.clear_played(fresh);
        self.commit_trim(id, removal);
        Ok(())
    }

    /// A pass counted as a play. It waits in `pending_plays` with what queue
    /// pages show for its entry. An entry already gone from the queue has
    /// nothing to describe it, and is not recorded.
    fn count_play(&mut self, play: CountedPlay) {
        match self.store.queue_entry(play.entry_id) {
            Ok(Some(item)) => {
                if let Some(dropped) = self
                    .pending_plays
                    .push(PlayRecord::of(item, play.started_at))
                {
                    eprintln!(
                        "tyranny: play history writes keep failing; dropped the play of entry {} at {}",
                        dropped.entry_id, dropped.played_at
                    );
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!(
                "tyranny: describing the play of entry {} failed; not recorded: {error:#}",
                play.entry_id
            ),
        }
    }

    /// Describes the passes that counted since the last pump, while their
    /// entries are still in the store. Call before a store edit that can
    /// delete the current entry: output may have reached its threshold since
    /// the pump, and a deleted entry has nothing to describe its play.
    fn collect_plays(&mut self) {
        self.player.count_heard();
        for play in self.player.take_counted_plays() {
            self.count_play(play);
        }
    }

    /// Writes the pending plays oldest first, one small commit each. The first
    /// failure stops this attempt; it and the later plays wait for the retry.
    /// Playback never pauses for history. Returns whether any play was written.
    fn write_plays(&mut self) -> bool {
        let mut written = false;
        while let Some(play) = self.pending_plays.front() {
            if let Err(error) = self.store.record_play(play) {
                eprintln!(
                    "tyranny: recording a play failed; {} waiting to retry: {error:#}",
                    self.pending_plays.len()
                );
                break;
            }
            self.pending_plays.pop_front();
            written = true;
        }
        self.retrying_plays = !self.pending_plays.is_empty();
        if written {
            match self.store.history_revision() {
                Ok(revision) => self.history_revision = revision,
                Err(error) => eprintln!("tyranny: reading the history revision failed: {error:#}"),
            }
        }
        written
    }

    fn playlist_applied(&mut self, change: PlaylistChange) -> CommandResult<Response> {
        self.playlists_revision = change.revision;
        self.publish();
        Ok(Response::PlaylistApplied {
            snapshot: Box::new(self.last.clone()),
            playlist: change.playlist,
        })
    }

    fn edit_playlist(
        &mut self,
        id: Option<u64>,
        expected: u64,
        edit: PlaylistEdit,
    ) -> CommandResult<Response> {
        if let PlaylistEdit::Add {
            library_revision,
            track_ids,
            ..
        } = &edit
        {
            if track_ids.is_empty() || track_ids.len() > MAX_PAGE_SIZE {
                return Err(Failure::invalid("insert requires 1..=500 tracks"));
            }
            for &track in track_ids {
                let path = self
                    .store
                    .track_source(track, *library_revision)
                    .map_err(Failure::library)?;
                self.source_path(PathBuf::from(path))?;
            }
        }
        let change = self
            .store
            .edit_playlist(id, expected, edit)
            .map_err(Failure::playlist)?;
        self.playlist_applied(change)
    }

    /// Adds every track of the resolved selection in one playlist edit that
    /// rechecks the selection's revisions.
    fn playlist_add_selection(
        &mut self,
        playlist_id: u64,
        expected_revision: u64,
        selection: TrackSelection,
        before: Option<u64>,
        sources: Resolution,
    ) -> CommandResult<Response> {
        let track_ids = sources?.into_iter().map(|(track, _)| track).collect();
        let change = self
            .store
            .edit_playlist(
                Some(playlist_id),
                expected_revision,
                PlaylistEdit::AddSelection {
                    selection: Box::new(selection),
                    track_ids,
                    before,
                },
            )
            .map_err(Failure::playlist)?;
        self.playlist_applied(change)
    }

    fn source_path(&self, source: PathBuf) -> CommandResult<PathBuf> {
        source_path(source, self.capture_path.as_deref())
    }
    fn insert_tracks(
        &mut self,
        expected_revision: u64,
        library_revision: u64,
        track_ids: Vec<u64>,
        before: Option<u64>,
    ) -> CommandResult<Response> {
        self.check_revision(expected_revision)?;
        if track_ids.is_empty() || track_ids.len() > MAX_PAGE_SIZE {
            return Err(Failure::invalid("insert requires 1..=500 tracks"));
        }
        if let Some(before) = before {
            self.contains(before)?;
        }
        let mut paths = Vec::with_capacity(track_ids.len());
        for &track in &track_ids {
            let path = self
                .store
                .track_source(track, library_revision)
                .map_err(Failure::library)?;
            paths.push(self.source_path(PathBuf::from(path))?);
        }
        let prepared = self
            .player
            .prepare_insert(paths.into_iter(), before, expected_revision)
            .map_err(Failure::playback)?;
        let first_id = prepared.first_id();
        self.store
            .insert_tracks(
                expected_revision,
                library_revision,
                prepared
                    .entries()
                    .zip(track_ids)
                    .map(|((id, path), track)| (id, track, path)),
                before,
            )
            .map_err(Failure::playlist)?;
        self.player.commit_insert(prepared);
        self.published(Some(first_id))
    }

    /// Appends the playlist's resolved sources in one store transaction, which
    /// rechecks the playlist and library revisions and every source, and one
    /// infallible player commit.
    fn enqueue_playlist(
        &mut self,
        playlist_id: u64,
        playlist_revision: u64,
        library_revision: u64,
        expected_revision: u64,
        play: bool,
        sources: Resolution,
    ) -> CommandResult<Response> {
        self.check_revision(expected_revision)?;
        self.next_revision()?;
        let sources = sources?;
        let mut track_ids = Vec::new();
        track_ids
            .try_reserve_exact(sources.len())
            .map_err(|error| Failure::playback(error.into()))?;
        let paths = sources.into_iter().map(|(track_id, path)| {
            track_ids.push(track_id);
            path
        });
        let scope = self
            .store
            .playlist_source(playlist_id)
            .map_err(Failure::playlist)?
            .kind
            .repeat_scope();
        let prepared = self
            .player
            .prepare_append(paths, expected_revision, play, scope)
            .map_err(Failure::playback)?;
        let first_id = prepared.first_id();
        self.store
            .enqueue_playlist(
                playlist_id,
                playlist_revision,
                library_revision,
                expected_revision,
                prepared
                    .entries()
                    .zip(track_ids)
                    .map(|((id, path), track_id)| (id, track_id, path)),
                play.then_some(Checkpoint {
                    current_id: Some(first_id),
                    position_frames: 0,
                    playing: true,
                }),
            )
            .map_err(Failure::playlist)?;
        self.player.commit_append(prepared);
        if play {
            self.fault = None;
        }
        // Queue rows and the new selection were committed together; no second
        // fallible checkpoint may reject an already audible playlist load.
        self.published(Some(first_id))
    }

    /// Edits the queue with the resolved selection in one store transaction,
    /// which rechecks the selection's revisions, and one infallible player
    /// commit. With `play` the first new entry is selected as `Play` would,
    /// and the checkpoint commits with the rows.
    fn enqueue_tracks(
        &mut self,
        selection: TrackSelection,
        expected_revision: u64,
        placement: Placement,
        play: bool,
        source: QueueSource,
        sources: Resolution,
    ) -> CommandResult<Response> {
        self.check_revision(expected_revision)?;
        self.next_revision()?;
        source_name(&source)?;
        let sources = sources?;
        let mut track_ids = Vec::new();
        track_ids
            .try_reserve_exact(sources.len())
            .map_err(|error| Failure::playback(error.into()))?;
        // With play the first new entry becomes current: trim the played
        // entries this edit keeps. Repeat All over the whole queue keeps them.
        let trim = if play
            && !(self.settings.repeat == RepeatMode::All
                && source.kind.repeat_scope() == RepeatScope::Queue)
        {
            let removed: HashSet<u64> = if placement == Placement::ReplaceUpcoming {
                self.player
                    .upcoming_ids()
                    .map_err(Failure::playback)?
                    .into_iter()
                    .collect()
            } else {
                HashSet::new()
            };
            let played = self
                .store
                .played_newest_first()
                .map_err(Failure::persistence)?;
            played_overflow(&played, |id| removed.contains(&id))
        } else {
            Vec::new()
        };
        let paths = sources.into_iter().map(|(track_id, path)| {
            track_ids.push(track_id);
            path
        });
        let prepared = self
            .player
            .prepare_replace_upcoming(
                paths,
                placement,
                expected_revision,
                // A list played without a clicked row starts at a shuffle pick.
                play.then_some(match selection {
                    TrackSelection::List {
                        first_row_id: None, ..
                    } => Start::Pick,
                    _ => Start::First,
                }),
                source.kind.repeat_scope(),
                &trim,
            )
            .map_err(Failure::playback)?;
        let first_id = prepared.first_id();
        let start_id = prepared.start_id();
        self.store
            .replace_upcoming(
                expected_revision,
                &selection,
                UpcomingEdit {
                    removed: prepared.removed(),
                    before: prepared.before(),
                    source: &source,
                    checkpoint: play.then_some(Checkpoint {
                        current_id: Some(start_id),
                        position_frames: 0,
                        playing: true,
                    }),
                },
                prepared
                    .entries()
                    .zip(track_ids)
                    .map(|((id, path), track_id)| (id, track_id, path)),
            )
            .map_err(Failure::playlist)?;
        self.player.commit_replace_upcoming(prepared);
        if play {
            self.fault = None;
            self.marked = Some(start_id);
        }
        // Queue rows and any new selection were committed together; no second
        // fallible checkpoint may reject an already audible run.
        self.published(Some(first_id))
    }

    fn append(
        &mut self,
        expected_revision: u64,
        path: String,
        track: Option<(u64, u64)>,
        kind: SourceKind,
    ) -> CommandResult<Response> {
        self.check_revision(expected_revision)?;
        let revision = self.next_revision()?;
        let id = self.player.next_id();
        if id >= i64::MAX as u64 {
            return Err(Failure::invalid("queue ID exhausted"));
        }
        let source = self.source_path(PathBuf::from(path))?;
        let path = source.to_str().expect("validated UTF-8 source");
        let stamp = QueueSource::stamped(kind, "");
        self.store
            .edit(
                Edit::Append {
                    id,
                    path,
                    track,
                    source: Some(&stamp),
                    run_id: Some(id),
                },
                revision,
                id + 1,
            )
            .map_err(Failure::library)?;
        let actual = self
            .player
            .append(source, expected_revision)
            .map_err(Failure::committed)?;
        if actual != id {
            return Err(Failure::committed(anyhow!("queue ID diverged")));
        }
        self.applied(Some(id))
    }

    fn append_stream(
        &mut self,
        expected_revision: u64,
        url: String,
        title: String,
    ) -> CommandResult<Response> {
        self.check_revision(expected_revision)?;
        let revision = self.next_revision()?;
        let stream =
            StreamSpec::new(url, title).map_err(|error| Failure::invalid(error.to_string()))?;
        let prepared = self
            .player
            .prepare_stream(stream.clone(), expected_revision)
            .map_err(Failure::playback)?;
        let id = prepared.first_id();
        if id >= i64::MAX as u64 {
            return Err(Failure::invalid("queue ID exhausted"));
        }
        self.store
            .edit(
                Edit::AppendStream {
                    id,
                    stream: &stream,
                },
                revision,
                id + 1,
            )
            .map_err(Failure::persistence)?;
        self.player.commit_append(prepared);
        self.published(Some(id))
    }

    fn command(&mut self, command: Command) -> CommandResult<Response> {
        match command {
            Command::Snapshot => {
                self.publish();
                Ok(Response::Snapshot {
                    snapshot: Box::new(self.last.clone()),
                })
            }
            Command::QueuePage {
                revision,
                offset,
                limit,
            } => {
                if let Some(revision) = revision {
                    self.check_revision(revision)?;
                }
                if limit == 0 || limit > MAX_PAGE_SIZE || offset > i64::MAX as usize {
                    return Err(Failure::invalid(format!(
                        "page limit must be 1..={MAX_PAGE_SIZE}"
                    )));
                }
                let entries = self
                    .store
                    .queue_page(offset, limit)
                    .map_err(Failure::persistence)?;
                Ok(Response::QueuePage {
                    revision: self.revision(),
                    total: self.player.queue_entries().len(),
                    offset,
                    entries,
                })
            }
            Command::History {
                revision: _,
                offset,
                limit,
            } => {
                if limit == 0 || limit > MAX_PAGE_SIZE || offset > i64::MAX as usize {
                    return Err(Failure::invalid(format!(
                        "page limit must be 1..={MAX_PAGE_SIZE}"
                    )));
                }
                let page = self
                    .store
                    .history_page(offset, limit)
                    .map_err(Failure::persistence)?;
                Ok(Response::History { page })
            }
            Command::Append {
                expected_revision,
                path,
            } => self.append(expected_revision, path, None, SourceKind::Direct),
            Command::AppendStream {
                expected_revision,
                url,
                title,
            } => self.append_stream(expected_revision, url, title),
            Command::AppendTrack {
                expected_revision,
                library_revision,
                track_id,
            } => {
                self.check_revision(expected_revision)?;
                let path = self
                    .store
                    .track_source(track_id, library_revision)
                    .map_err(Failure::library)?;
                self.append(
                    expected_revision,
                    path,
                    Some((track_id, library_revision)),
                    SourceKind::Drag,
                )
            }
            Command::SavedPlaylists {
                revision,
                offset,
                limit,
                query,
                kind,
            } => {
                let page = self
                    .store
                    .saved_playlists(
                        PageRequest {
                            revision,
                            offset,
                            limit,
                            query,
                        },
                        kind,
                    )
                    .map_err(Failure::playlist)?;
                Ok(Response::SavedPlaylists { page })
            }
            Command::PlaylistEntries {
                playlist_id,
                revision,
                library_revision,
                offset,
                limit,
                query,
            } => {
                let (library_revision, page) = self
                    .store
                    .playlist_entries(
                        playlist_id,
                        revision,
                        library_revision,
                        offset,
                        limit,
                        &query,
                    )
                    .map_err(Failure::playlist)?;
                Ok(Response::PlaylistEntries {
                    playlist_id,
                    library_revision,
                    page,
                })
            }
            Command::CreatePlaylist {
                expected_revision,
                name,
            } => self.edit_playlist(None, expected_revision, PlaylistEdit::Create { name }),
            Command::CreateSmartPlaylist {
                expected_revision,
                name,
                query,
            } => self.edit_playlist(
                None,
                expected_revision,
                PlaylistEdit::CreateSmart { name, query },
            ),
            Command::UpdateSmartPlaylist {
                playlist_id,
                expected_revision,
                query,
            } => self.edit_playlist(
                Some(playlist_id),
                expected_revision,
                PlaylistEdit::UpdateSmart { query },
            ),
            Command::SnapshotSmartPlaylist {
                playlist_id,
                playlist_revision,
                library_revision,
                expected_revision,
                name,
            } => {
                let change = self
                    .store
                    .snapshot_smart_playlist(
                        playlist_id,
                        playlist_revision,
                        library_revision,
                        expected_revision,
                        &name,
                    )
                    .map_err(Failure::playlist)?;
                self.playlist_applied(change)
            }
            Command::RenamePlaylist {
                playlist_id,
                expected_revision,
                name,
            } => self.edit_playlist(
                Some(playlist_id),
                expected_revision,
                PlaylistEdit::Rename { name },
            ),
            Command::DeletePlaylist {
                playlist_id,
                expected_revision,
            } => self.edit_playlist(Some(playlist_id), expected_revision, PlaylistEdit::Delete),
            Command::PlaylistAdd {
                playlist_id,
                expected_revision,
                library_revision,
                track_ids,
                before,
            } => self.edit_playlist(
                Some(playlist_id),
                expected_revision,
                PlaylistEdit::Add {
                    library_revision,
                    track_ids,
                    before,
                },
            ),
            Command::PlaylistRemove {
                playlist_id,
                expected_revision,
                entry_ids,
            } => self.edit_playlist(
                Some(playlist_id),
                expected_revision,
                PlaylistEdit::Remove { entry_ids },
            ),
            Command::PlaylistMove {
                playlist_id,
                expected_revision,
                entry_ids,
                before,
            } => self.edit_playlist(
                Some(playlist_id),
                expected_revision,
                PlaylistEdit::Move { entry_ids, before },
            ),
            Command::InsertTracks {
                expected_revision,
                library_revision,
                track_ids,
                before,
            } => self.insert_tracks(expected_revision, library_revision, track_ids, before),
            Command::EnqueueTracks { .. }
            | Command::PlaylistAddSelection { .. }
            | Command::EnqueuePlaylist { .. } => {
                unreachable!("run selections are resolved before the owner (`Work::Resolved`)")
            }
            Command::Artwork { .. }
            | Command::AddRoot { .. }
            | Command::Rescan { .. }
            | Command::LibraryRoots { .. }
            | Command::LibraryArtists { .. }
            | Command::LibraryAlbums { .. }
            | Command::LibraryTracks { .. }
            | Command::ListTracks { .. }
            | Command::ListAlbums { .. }
            | Command::ListArtists { .. }
            | Command::ListGenres { .. }
            | Command::AlbumSummaries { .. }
            | Command::ListGroups { .. } => {
                unreachable!("library and artwork commands are routed to their own workers")
            }
            Command::Remove {
                expected_revision,
                entry_id,
            } => {
                self.check_revision(expected_revision)?;
                self.contains(entry_id)?;
                let revision = self.next_revision()?;
                self.collect_plays();
                self.store
                    .edit(
                        Edit::Remove { id: entry_id },
                        revision,
                        self.player.next_id(),
                    )
                    .map_err(Failure::persistence)?;
                self.player
                    .remove(entry_id, expected_revision)
                    .map_err(Failure::committed)?;
                self.applied(None)
            }
            Command::Move {
                expected_revision,
                entry_ids,
                before,
            } => {
                self.check_revision(expected_revision)?;
                let prepared = self
                    .player
                    .prepare_move(&entry_ids, before, expected_revision)
                    .map_err(|error| Failure::invalid(error.to_string()))?;
                if let Some(prepared) = prepared {
                    self.store
                        .move_queue(expected_revision, &entry_ids, before)
                        .map_err(Failure::playlist)?;
                    self.player.commit_move(prepared);
                }
                self.published(None)
            }
            Command::Clear { expected_revision } => {
                self.check_revision(expected_revision)?;
                let revision = self.next_revision()?;
                self.collect_plays();
                self.store
                    .edit(Edit::Clear, revision, self.player.next_id())
                    .map_err(Failure::persistence)?;
                self.player
                    .clear(expected_revision)
                    .map_err(Failure::committed)?;
                self.applied(None)
            }
            Command::ClearUpcoming { expected_revision } => {
                self.check_revision(expected_revision)?;
                let prepared = self
                    .player
                    .prepare_clear_upcoming(expected_revision)
                    .map_err(Failure::playback)?;
                if let Some(prepared) = prepared {
                    self.store
                        .remove_entries(expected_revision, prepared.removed())
                        .map_err(Failure::playlist)?;
                    self.player.commit_remove_batch(prepared);
                }
                self.published(None)
            }
            Command::ShuffleUpcoming { expected_revision } => {
                self.check_revision(expected_revision)?;
                let prepared = self
                    .player
                    .prepare_shuffle_upcoming(
                        &mut UniformShuffle::from_entropy(),
                        expected_revision,
                    )
                    .map_err(Failure::playback)?;
                if let Some(prepared) = prepared {
                    self.store
                        .reorder_queue(expected_revision, prepared.order())
                        .map_err(Failure::playlist)?;
                    self.player.commit_reorder(prepared);
                }
                self.published(None)
            }
            Command::SaveUpcoming {
                expected_revision,
                expected_playlists_revision,
                name,
            } => {
                self.check_revision(expected_revision)?;
                let ids = self.player.upcoming_ids().map_err(Failure::playback)?;
                let change = self
                    .store
                    .save_upcoming(expected_revision, expected_playlists_revision, &name, &ids)
                    .map_err(Failure::playlist)?;
                self.playlist_applied(change)
            }
            Command::TogglePlayPause => {
                let active = matches!(
                    self.state().transport,
                    Transport::Playing | Transport::Buffering
                );
                self.command(if active {
                    Command::Pause
                } else {
                    Command::Play { entry_id: None }
                })
            }
            Command::Next | Command::Previous => {
                let state = self.player.snapshot();
                let (destination, jump) = match command {
                    Command::Next => self
                        .player
                        .next_jump()
                        .map(|(id, fresh)| (id, Jump::Forward { fresh })),
                    Command::Previous => state.previous_id.map(|id| (id, Jump::Back)),
                    _ => unreachable!(),
                }
                .ok_or_else(|| Failure::invalid("no available queue occurrence"))?;
                let active = state.playing
                    && !state.stopped
                    && !self.player.completed()
                    && self.player.error().is_none()
                    && self.fault.is_none();
                self.select(destination, 0, active, jump)
            }
            Command::Play { entry_id } => {
                let state = self.player.snapshot();
                let restart =
                    state.stopped || self.player.completed() || self.player.error().is_some();
                let selection = entry_id.or_else(|| {
                    if restart || state.current_id.is_none() {
                        self.player.queue_entries().next().map(|entry| entry.0)
                    } else {
                        None
                    }
                });
                if let Some(id) = selection {
                    let fresh = self.player.jump_fresh(id);
                    return self.select(id, 0, true, Jump::Forward { fresh });
                }
                if self.player.queue_entries().len() == 0 {
                    return Err(Failure::invalid("queue is empty"));
                }
                let mut checkpoint = self.checkpoint();
                checkpoint.playing = true;
                self.store
                    .checkpoint(checkpoint, &self.settings)
                    .map_err(Failure::persistence)?;
                self.fault = None;
                self.player.play();
                self.published(None)
            }
            Command::Pause => {
                let mut checkpoint = self.checkpoint();
                checkpoint.playing = false;
                self.store
                    .checkpoint(checkpoint, &self.settings)
                    .map_err(Failure::persistence)?;
                self.player.pause();
                self.published(None)
            }
            Command::Stop => {
                self.store
                    .checkpoint(Checkpoint::default(), &self.settings)
                    .map_err(Failure::persistence)?;
                self.player.stop();
                self.fault = None;
                self.published(None)
            }
            Command::Seek {
                entry_id,
                position_frames,
            } => {
                let state = self.player.snapshot();
                if state.current_id != Some(entry_id) {
                    return Err(Failure::invalid("seek occurrence is no longer current"));
                }
                if self.player.is_stream(entry_id) {
                    return Err(Failure::invalid("live streams cannot seek"));
                }
                if state
                    .now_playing
                    .as_ref()
                    .and_then(|metadata| metadata.duration_frames)
                    .is_some_and(|duration| position_frames >= duration)
                {
                    return Err(Failure::playback(anyhow!(
                        "seek target exceeds track duration"
                    )));
                }
                let active = state.playing
                    && !state.stopped
                    && !self.player.completed()
                    && self.player.error().is_none()
                    && self.fault.is_none();
                self.select(entry_id, position_frames, active, Jump::Seek)
            }
            Command::SetResume { enabled } => {
                let mut settings = self.settings.clone();
                settings.resume_on_start = enabled;
                self.store
                    .checkpoint(self.checkpoint(), &settings)
                    .map_err(Failure::persistence)?;
                self.settings = settings;
                self.published(None)
            }
            Command::SetRepeat { mode } => {
                let mut settings = self.settings.clone();
                settings.repeat = mode;
                self.store
                    .checkpoint(self.checkpoint(), &settings)
                    .map_err(Failure::persistence)?;
                self.settings = settings;
                if self.player.repeat() != mode {
                    self.player.set_repeat(mode);
                }
                self.published(None)
            }
            Command::SetShuffle { enabled } => {
                let mut settings = self.settings.clone();
                settings.shuffle = enabled;
                self.store
                    .checkpoint(self.checkpoint(), &settings)
                    .map_err(Failure::persistence)?;
                let changed = self.settings.shuffle != enabled;
                self.settings = settings;
                if changed {
                    self.player.set_shuffle(enabled.then(shuffle_strategy));
                }
                self.published(None)
            }
            Command::SetVolume { percent } => {
                if percent > 100 {
                    return Err(Failure::invalid("volume must be 0..=100"));
                }
                let mut settings = self.settings.clone();
                settings.volume = percent;
                self.store
                    .checkpoint(self.checkpoint(), &settings)
                    .map_err(Failure::persistence)?;
                self.settings = settings;
                self.player.set_volume(percent);
                self.published(None)
            }
            Command::SetGain { mode } => {
                let checkpoint = self.checkpoint();
                let prepared = checkpoint
                    .current_id
                    .map(|id| {
                        self.player
                            .prepare_selection(id, checkpoint.position_frames, mode)
                    })
                    .transpose()
                    .map_err(Failure::playback)?;
                let mut settings = self.settings.clone();
                settings.gain_mode = mode;
                self.store
                    .checkpoint(checkpoint, &settings)
                    .map_err(Failure::persistence)?;
                self.settings = settings;
                if let Some(prepared) = prepared {
                    self.player.commit_selection(prepared);
                    if !checkpoint.playing {
                        self.player.pause();
                    }
                } else {
                    self.player.stop();
                    self.player.set_idle_gain(mode);
                }
                self.fault = None;
                self.published(None)
            }
        }
    }

    /// A run selection command whose selection resolved off the owner.
    fn resolved(&mut self, command: Command, sources: Resolution) -> CommandResult<Response> {
        match command {
            Command::EnqueueTracks {
                selection,
                expected_revision,
                placement,
                play,
                source,
            } => self.enqueue_tracks(
                selection,
                expected_revision,
                placement,
                play,
                source,
                sources,
            ),
            Command::PlaylistAddSelection {
                playlist_id,
                expected_revision,
                selection,
                before,
            } => self.playlist_add_selection(
                playlist_id,
                expected_revision,
                selection,
                before,
                sources,
            ),
            Command::EnqueuePlaylist {
                playlist_id,
                playlist_revision,
                library_revision,
                expected_revision,
                play,
            } => self.enqueue_playlist(
                playlist_id,
                playlist_revision,
                library_revision,
                expected_revision,
                play,
                sources,
            ),
            _ => unreachable!("only run selections are resolved before the owner"),
        }
    }

    /// Sends the command's reply. Returns the error of a fatal failure, after
    /// which the owner stops.
    fn reply(
        &mut self,
        id: u64,
        result: CommandResult<Response>,
        reply: oneshot::Sender<Message>,
    ) -> Option<anyhow::Error> {
        match result {
            Ok(result) => {
                let _ = reply.send(Message::Reply {
                    id,
                    reply: Reply::Ok { result },
                });
                None
            }
            Err(failure) => {
                self.publish();
                let message = format!("{:#}", failure.error);
                let _ = reply.send(Message::Reply {
                    id,
                    reply: Reply::Error {
                        code: failure.code,
                        message,
                        queue_revision: self.revision(),
                    },
                });
                failure.fatal.then_some(failure.error)
            }
        }
    }

    /// Stops the owner after a fatal failure.
    fn stop(mut self, error: anyhow::Error) -> Result<()> {
        self.player.pause();
        let _ = self.library.shutdown();
        Err(error)
    }

    fn run(mut self, mut receiver: mpsc::Receiver<Work>) -> Result<()> {
        let mut event_time = Instant::now();
        let mut checkpoint_time = Instant::now();
        loop {
            for _ in 0..8 {
                if let Err(error) = self.player.pump() {
                    self.player.pause();
                    self.fault = Some(format!("audio output: {error:#}"));
                }
                // Before the advances, whose trims may remove a counted entry.
                // While earlier writes fail, new plays wait for the retry.
                for play in self.player.take_counted_plays() {
                    self.count_play(play);
                }
                let recorded =
                    !self.retrying_plays && !self.pending_plays.is_empty() && self.write_plays();
                // Natural advance: each pass output starts marks its entry played.
                // The mark is bookkeeping: a failed one leaves the player and
                // the store as they were, and playback goes on.
                for (id, fresh) in self.player.take_advances() {
                    if let Err(error) = self.advance(id, &fresh) {
                        eprintln!(
                            "tyranny: recording entry {id} as played failed; still playing: {error:#}"
                        );
                    }
                }
                self.player.clear_diagnostics();
                if recorded {
                    self.publish();
                }
                match receiver.try_recv() {
                    Ok(Work::Subscribe(reply)) => {
                        // No command or publication can intervene between these.
                        self.publish();
                        let subscription = Subscription {
                            snapshot: self.last.clone(),
                            events: self.events.subscribe(),
                        };
                        let _ = reply.send(subscription);
                    }
                    Ok(Work::Request(request, reply)) => {
                        let result = self.command(request.command);
                        if let Some(error) = self.reply(request.id, result, reply) {
                            return self.stop(error);
                        }
                    }
                    Ok(Work::Resolved(request, sources, reply)) => {
                        let result = self.resolved(request.command, sources);
                        if let Some(error) = self.reply(request.id, result, reply) {
                            return self.stop(error);
                        }
                    }
                    Ok(Work::Shutdown(reply)) => {
                        let checkpoint = self.checkpoint();
                        self.player.pause();
                        self.write_plays();
                        let library = self.library.shutdown();
                        let saved = self.store.checkpoint(checkpoint, &self.settings);
                        let result = library.and(saved);
                        let response = result
                            .as_ref()
                            .map(|_| ())
                            .map_err(|error| anyhow!("{error:#}"));
                        let _ = reply.send(response);
                        return result;
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        let checkpoint = self.checkpoint();
                        self.player.pause();
                        self.write_plays();
                        let library = self.library.shutdown();
                        let saved = self.store.checkpoint(checkpoint, &self.settings);
                        return library.and(saved);
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {}
                }
            }
            if event_time.elapsed() >= EVENT_INTERVAL {
                self.publish();
                event_time = Instant::now();
            }
            if checkpoint_time.elapsed() >= CHECKPOINT_INTERVAL {
                if !self.pending_plays.is_empty() && self.write_plays() {
                    self.publish();
                }
                if let Err(error) = self.save() {
                    self.player.pause();
                    self.fault = Some(format!("checkpoint failed; playback paused: {error:#}"));
                    self.publish();
                }
                checkpoint_time = Instant::now();
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}
