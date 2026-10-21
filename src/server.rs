//! Private, bounded Unix transport. The owner, not any socket, owns playback.
use crate::{
    output::OutputConfig,
    owner::{Controller, Owner, OwnerConfig},
    protocol::{Command, Message, Reply, Request, Response, VERSION},
    transport::{
        FrameReader, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, prepare_socket, socket_metadata,
        validate_ancestors, validate_directory, validate_socket_path, write_frame,
    },
};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use std::{
    fs::{self, DirBuilder, File, Metadata, OpenOptions},
    future::Future,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::watch,
    task::JoinSet,
    time::timeout,
};
use uuid::Uuid;

const MAX_CONNECTIONS: usize = 32;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ServerConfig {
    pub state_dir: PathBuf,
    pub output: OutputConfig,
}

pub struct Server {
    listener: UnixListener,
    owner: Owner,
    socket: SocketGuard,
    state: StateDirectory,
    instance_id: String,
}

impl Server {
    /// Acquires the lifetime lock before starting the single playback owner.
    pub async fn start(config: ServerConfig) -> Result<Self> {
        let state = StateDirectory::open(&config.state_dir)?;
        let path = state.root.join("control.sock");
        validate_socket_path(&path)?;
        remove_stale_socket(&path).await?;
        let listener = UnixListener::bind(&path).context("bind local control socket")?;
        let metadata = fs::symlink_metadata(&path)?;
        let socket = SocketGuard {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        fs::set_permissions(&socket.path, fs::Permissions::from_mode(0o600))?;
        let owner = Owner::start(OwnerConfig {
            database_path: state.root.join("state.sqlite3"),
            output: config.output,
        })?;
        Ok(Self {
            listener,
            socket,
            owner,
            state,
            instance_id: Uuid::new_v4().to_string(),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket.path
    }
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Stops accepting, closes controllers, checkpoints and joins the owner before
    /// releasing the state-directory lock. Disconnecting a controller never stops audio.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let Self {
            listener,
            owner,
            socket,
            state,
            instance_id,
            ..
        } = self;
        let controller = owner.controller();
        let instance_id: Arc<str> = instance_id.into();
        let (stop, stopped) = watch::channel(false);
        let mut connections = JoinSet::new();
        tokio::pin!(shutdown);
        let network_result: Result<()> = loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => break Ok(()),
                joined = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = joined {
                        break Err(anyhow::anyhow!("controller task failed: {error}"));
                    }
                }
                accepted = listener.accept() => {
                    let (socket, _) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => break Err(error).context("accept local controller"),
                    };
                    if connections.len() >= MAX_CONNECTIONS || prepare_socket(&socket).is_err() {
                        drop(socket);
                        continue;
                    }
                    connections.spawn(serve_connection(
                        socket, controller.clone(), instance_id.clone(), stopped.clone(),
                    ));
                }
            }
        };
        drop(listener);
        let _ = stop.send(true);
        while connections.join_next().await.is_some() {}
        drop(controller);
        let shutdown_result = owner.shutdown().await;
        drop(socket);
        drop(state);
        match (network_result, shutdown_result) {
            (Ok(()), result) => result,
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(shutdown)) => {
                bail!("{error:#}; owner shutdown also failed: {shutdown:#}")
            }
        }
    }
}

pub fn default_state_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    let suffix = "Library/Application Support/tyranny";
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(root) = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            return Ok(root.join("tyranny"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    let suffix = ".local/state/tyranny";
    Ok(PathBuf::from(
        std::env::var_os("HOME")
            .context("HOME is not set; supply an explicit state/socket path")?,
    )
    .join(suffix))
}

struct StateDirectory {
    root: PathBuf,
    _lock: File,
    _directory: File,
}

impl StateDirectory {
    fn open(path: &Path) -> Result<Self> {
        ensure!(
            !path.as_os_str().is_empty(),
            "state directory must not be empty"
        );
        // Normalize trailing separators before the no-follow check.
        let normalized: PathBuf = path.components().collect();
        let path = normalized.as_path();
        match fs::symlink_metadata(path) {
            Ok(metadata) => validate_directory(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(path)
                    .context("create private state directory")?;
            }
            Err(error) => return Err(error).context("inspect state directory"),
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .context("open private state directory without following symlinks")?;
        validate_directory(&directory.metadata()?)?;
        let root = fs::canonicalize(path).context("resolve state directory")?;
        validate_ancestors(&root)?;
        // All children stay under the private directory. Reject insecure existing
        // files, including SQLite sidecars, rather than silently repairing them.
        let lock = open_private_file(&root.join("server.lock"), true)?;
        lock.try_lock_exclusive()
            .context("state directory is already locked by another server")?;
        drop(open_private_file(&root.join("state.sqlite3"), true)?);
        for name in [
            "state.sqlite3-wal",
            "state.sqlite3-shm",
            "state.sqlite3-journal",
        ] {
            let path = root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    drop(open_private_file(&path, false)?);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("inspect SQLite sidecar"),
            }
        }
        directory.sync_all().context("sync state directory")?;
        Ok(Self {
            root,
            _lock: lock,
            _directory: directory,
        })
    }
}

fn validate_file(metadata: &Metadata) -> Result<()> {
    ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "state file must be a regular file with one link"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "state file must be owned by the current user"
    );
    ensure!(
        metadata.mode() & 0o7777 == 0o600,
        "state file permissions must be 0600"
    );
    Ok(())
}

fn open_private_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open private state file {}", path.display()))?;
    validate_file(&file.metadata()?)?;
    Ok(file)
}

async fn serve_connection(
    mut socket: UnixStream,
    controller: Controller,
    instance: Arc<str>,
    mut shutdown: watch::Receiver<bool>,
) {
    let subscription = tokio::select! {
        biased;
        _ = shutdown.changed() => None,
        result = controller.subscribe() => result.ok(),
    };
    let Some(mut subscription) = subscription else {
        return;
    };
    let mut sequence = subscription.snapshot.sequence;
    let hello = Message::Hello {
        protocol_version: VERSION,
        instance_id: instance.to_string(),
        snapshot: subscription.snapshot,
    };
    if !send_message(&mut socket, &hello, &mut shutdown).await {
        return;
    }
    let mut reader = FrameReader::default();
    let mut deferred = None;
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            received = async {
                if let Some(request) = deferred.take() { Ok(Some(request)) }
                else { read_request(&mut reader, &mut socket).await }
            } => {
                let request = match received { Ok(Some(request)) => request, _ => return };
                // Keep watching abandoned artwork requests without losing a
                // fragmented next frame when the pending reply wins the select.
                let artwork = matches!(&request.command, Command::Artwork { .. });
                let pending = controller.request(request);
                tokio::pin!(pending);
                let reply = loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.changed() => return,
                        incoming = read_request(&mut reader, &mut socket), if artwork && deferred.is_none() => match incoming {
                            Ok(Some(request)) => deferred = Some(request),
                            _ => return,
                        },
                        result = &mut pending => break match result {
                            Ok(reply) => reply,
                            Err(_) => return,
                        },
                    }
                };
                if let Some(new_sequence) = reply_sequence(&reply) {
                    sequence = sequence.max(new_sequence);
                }
                if !send_message(&mut socket, &reply, &mut shutdown).await { return; }
            }
            event = subscription.events.recv() => {
                let snapshot = match event { Ok(snapshot) => snapshot, Err(_) => return };
                // Replies may advance past queued events. Never send state backwards.
                if snapshot.sequence > sequence {
                    sequence = snapshot.sequence;
                    if !send_message(&mut socket, &Message::State { snapshot }, &mut shutdown).await { return; }
                }
            }
        }
    }
}

async fn read_request(
    reader: &mut FrameReader,
    socket: &mut UnixStream,
) -> Result<Option<Request>> {
    reader
        .read(socket, MAX_REQUEST_BYTES)
        .await?
        .map(serde_json::from_slice)
        .transpose()
        .context("invalid request JSON")
}

fn reply_sequence(message: &Message) -> Option<u64> {
    match message {
        Message::Reply {
            reply:
                Reply::Ok {
                    result:
                        Response::Snapshot { snapshot }
                        | Response::Applied { snapshot, .. }
                        | Response::PlaylistApplied { snapshot, .. },
                },
            ..
        } => Some(snapshot.sequence),
        _ => None,
    }
}

async fn send_message(
    socket: &mut UnixStream,
    message: &Message,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    let serialized = match serde_json::to_vec(message) {
        Ok(serialized) if serialized.len() <= MAX_RESPONSE_BYTES => serialized,
        _ => return false,
    };
    if *shutdown.borrow() {
        return false;
    }
    tokio::select! {
        biased;
        _ = shutdown.changed() => false,
        result = timeout(IO_TIMEOUT, write_frame(socket, &serialized, MAX_RESPONSE_BYTES)) => matches!(result, Ok(Ok(()))),
    }
}

struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Called only while holding the directory's lifetime lock. A live socket is
/// never removed, including a listener which does not participate in our lock.
async fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect existing control socket"),
        Ok(_) => {}
    }
    let before = socket_metadata(path)?;
    match timeout(IO_TIMEOUT, UnixStream::connect(path)).await {
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Ok(Ok(_)) => bail!("control socket already has a live listener"),
        Ok(Err(error)) => return Err(error).context("probe existing control socket"),
        Err(_) => bail!("existing control socket probe timed out; refusing to remove it"),
    }
    let after = socket_metadata(path)?;
    ensure!(
        before.dev() == after.dev() && before.ino() == after.ino(),
        "control socket changed during stale check"
    );
    fs::remove_file(path).context("remove stale control socket")
}
