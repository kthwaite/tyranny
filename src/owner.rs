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
    playback::Player,
    protocol::{
        Command, ControlState, ErrorCode, MAX_PAGE_SIZE, Message, Reply, Request, Response,
        Settings, Transport,
    },
    store::{Checkpoint, Edit, PlaylistChange, PlaylistEdit, PlaylistFailure, Store},
};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::mpsc as startup,
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, mpsc, oneshot};

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
}
pub struct Subscription {
    pub snapshot: ControlState,
    pub events: broadcast::Receiver<ControlState>,
}
enum Work {
    Subscribe(oneshot::Sender<Subscription>),
    Request(Request, oneshot::Sender<Message>),
    Shutdown(oneshot::Sender<Result<()>>),
}

impl Owner {
    pub fn start(config: OwnerConfig) -> Result<Self> {
        let artwork_database = config.database_path.clone();
        let (sender, receiver) = mpsc::channel(64);
        let (ready_tx, ready_rx) = startup::sync_channel(1);
        let thread = thread::Builder::new()
            .name("playback-owner".into())
            .spawn(move || {
                let actor = Actor::open(config);
                match actor {
                    Ok(actor) => {
                        if ready_tx.send(Ok(actor.library.handle())).is_err() {
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
            Ok(library) => {
                let artwork = ArtworkWorkers::start(artwork_database);
                Ok(Self {
                    controller: Controller {
                        sender,
                        library,
                        artwork: artwork.handle(),
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
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Work::Request(request, reply))
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
        let code = if error.is::<StaleLibraryRevision>() {
            ErrorCode::StaleLibraryRevision
        } else if error.is::<crate::filter::InvalidFilter>()
            || error
                .downcast_ref::<LibraryError>()
                .is_some_and(|error| error.kind == LibraryErrorKind::InvalidFilter)
        {
            ErrorCode::InvalidFilter
        } else if error.downcast_ref::<LibraryError>().is_some_and(|error| {
            matches!(
                error.kind,
                LibraryErrorKind::Invalid | LibraryErrorKind::Busy
            )
        }) {
            ErrorCode::InvalidCommand
        } else {
            ErrorCode::Persistence
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

fn capture_destination(
    config: &OwnerConfig,
    entries: &[(u64, PathBuf)],
) -> Result<Option<PathBuf>> {
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
        !entries.iter().any(|(_, path)| same_file(&capture, path)),
        "capture destination overlaps a queued source"
    );
    Ok(Some(capture))
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
}
impl Actor {
    fn open(config: OwnerConfig) -> Result<Self> {
        let store = Store::open(&config.database_path)?;
        let loaded = store.load()?;
        let capture_path = capture_destination(&config, &loaded.entries)?;
        let mut player = Player::from_queue(
            loaded.entries,
            loaded.revision,
            loaded.next_id,
            config.output,
            loaded.settings.gain_mode,
        )?;
        let mut fault = None;
        if let Some(id) = loaded.checkpoint.current_id {
            match player.prepare_selection(
                id,
                loaded.checkpoint.position_frames,
                loaded.settings.gain_mode,
            ) {
                Ok(prepared) => {
                    player.commit_selection(prepared);
                    if loaded.settings.resume_on_start && loaded.checkpoint.playing {
                        player.play();
                    }
                }
                Err(error) => {
                    fault = Some(format!("restoring selected entry {id}: {error:#}"));
                }
            }
        }
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
        ControlState {
            sequence: self.last.sequence,
            queue_revision: state.queue_revision,
            playlists_revision: self.playlists_revision,
            queue_length: self.player.queue_entries().len(),
            current_id: state.current_id,
            can_next: state.next_id.is_some(),
            can_previous: state.previous_id.is_some(),
            now_playing: state.now_playing,
            position_frames: state.position_frames,
            sample_rate: SAMPLE_RATE,
            transport,
            settings: self.settings.clone(),
            error,
            underruns: state.output.underruns,
            library: self.library.state(),
        }
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
            position_frames: state.position_frames,
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
    fn select(
        &mut self,
        entry_id: u64,
        position_frames: u64,
        playing: bool,
    ) -> CommandResult<Response> {
        let prepared = self
            .player
            .prepare_selection(entry_id, position_frames, self.settings.gain_mode)
            .map_err(Failure::playback)?;
        self.store
            .checkpoint(
                Checkpoint {
                    current_id: Some(entry_id),
                    position_frames,
                    playing,
                },
                &self.settings,
            )
            .map_err(Failure::persistence)?;
        self.player.commit_selection(prepared);
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

    fn source_path(&self, source: PathBuf) -> CommandResult<PathBuf> {
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
        if self
            .capture_path
            .as_ref()
            .is_some_and(|capture| same_file(&source, capture))
        {
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

    fn enqueue_playlist(
        &mut self,
        playlist_id: u64,
        playlist_revision: u64,
        library_revision: u64,
        expected_revision: u64,
        play: bool,
    ) -> CommandResult<Response> {
        self.check_revision(expected_revision)?;
        self.next_revision()?;
        let mut sources = self
            .store
            .playlist_sources(playlist_id, playlist_revision, library_revision)
            .map_err(Failure::playlist)?;
        for (_, source) in &mut sources {
            *source = self.source_path(std::mem::take(source))?;
        }
        let mut track_ids = Vec::new();
        track_ids
            .try_reserve_exact(sources.len())
            .map_err(|error| Failure::playback(error.into()))?;
        let paths = sources.into_iter().map(|(track_id, path)| {
            track_ids.push(track_id);
            path
        });
        let prepared = self
            .player
            .prepare_append(paths, expected_revision, play)
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

    fn append(
        &mut self,
        expected_revision: u64,
        path: String,
        track: Option<(u64, u64)>,
    ) -> CommandResult<Response> {
        self.check_revision(expected_revision)?;
        let revision = self.next_revision()?;
        let id = self.player.next_id();
        if id >= i64::MAX as u64 {
            return Err(Failure::invalid("queue ID exhausted"));
        }
        let source = self.source_path(PathBuf::from(path))?;
        let path = source.to_str().expect("validated UTF-8 source");
        self.store
            .edit(Edit::Append { id, path, track }, revision, id + 1)
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
            Command::Append {
                expected_revision,
                path,
            } => self.append(expected_revision, path, None),
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
                self.append(expected_revision, path, Some((track_id, library_revision)))
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
            ),
            Command::InsertTracks {
                expected_revision,
                library_revision,
                track_ids,
                before,
            } => self.insert_tracks(expected_revision, library_revision, track_ids, before),
            Command::Artwork { .. }
            | Command::AddRoot { .. }
            | Command::Rescan { .. }
            | Command::LibraryRoots { .. }
            | Command::LibraryArtists { .. }
            | Command::LibraryAlbums { .. }
            | Command::LibraryTracks { .. } => {
                unreachable!("library and artwork commands are routed to their own workers")
            }
            Command::Remove {
                expected_revision,
                entry_id,
            } => {
                self.check_revision(expected_revision)?;
                self.contains(entry_id)?;
                let revision = self.next_revision()?;
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
                self.store
                    .edit(Edit::Clear, revision, self.player.next_id())
                    .map_err(Failure::persistence)?;
                self.player
                    .clear(expected_revision)
                    .map_err(Failure::committed)?;
                self.applied(None)
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
                let destination = match command {
                    Command::Next => state.next_id,
                    Command::Previous => state.previous_id,
                    _ => unreachable!(),
                }
                .ok_or_else(|| Failure::invalid("no available queue occurrence"))?;
                let active = state.playing
                    && !state.stopped
                    && !self.player.completed()
                    && self.player.error().is_none()
                    && self.fault.is_none();
                self.select(destination, 0, active)
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
                    return self.select(id, 0, true);
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
                self.select(entry_id, position_frames, active)
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

    fn run(mut self, mut receiver: mpsc::Receiver<Work>) -> Result<()> {
        let mut event_time = Instant::now();
        let mut checkpoint_time = Instant::now();
        loop {
            for _ in 0..8 {
                if let Err(error) = self.player.pump() {
                    self.player.pause();
                    self.fault = Some(format!("audio output: {error:#}"));
                }
                self.player.clear_diagnostics();
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
                        match result {
                            Ok(result) => {
                                let _ = reply.send(Message::Reply {
                                    id: request.id,
                                    reply: Reply::Ok { result },
                                });
                            }
                            Err(failure) => {
                                self.publish();
                                let message = format!("{:#}", failure.error);
                                let _ = reply.send(Message::Reply {
                                    id: request.id,
                                    reply: Reply::Error {
                                        code: failure.code,
                                        message,
                                        queue_revision: self.revision(),
                                    },
                                });
                                if failure.fatal {
                                    self.player.pause();
                                    let _ = self.library.shutdown();
                                    return Err(failure.error);
                                }
                            }
                        }
                    }
                    Ok(Work::Shutdown(reply)) => {
                        let checkpoint = self.checkpoint();
                        self.player.pause();
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
