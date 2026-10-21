//! Authenticated, bounded loopback transport. The owner, not any socket, owns playback.
use crate::{
    output::OutputConfig,
    owner::{Controller, Owner, OwnerConfig},
    protocol::{Command, Message, Reply, Request, Response, VERSION},
};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use futures_util::{SinkExt, StreamExt};
use std::{
    fs::{self, DirBuilder, File, Metadata, OpenOptions},
    future::Future,
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async_with_config,
    tungstenite::{
        Message as Frame,
        client::IntoClientRequest,
        handshake::{
            client::Request as ClientRequest,
            server::{ErrorResponse, Request as UpgradeRequest, Response as UpgradeResponse},
        },
        http::{
            HeaderValue, StatusCode, Uri,
            header::{AUTHORIZATION, ORIGIN},
        },
        protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
    },
};
use uuid::Uuid;

pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
/// Queue pages can contain 500 paths, so replies have a separate finite ceiling.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 32;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const TOKEN_BYTES: usize = 64;

pub struct ServerConfig {
    pub state_dir: PathBuf,
    pub bind: SocketAddr,
    pub output: OutputConfig,
}

pub struct Server {
    listener: TcpListener,
    address: SocketAddr,
    owner: Owner,
    state: StateDirectory,
    instance_id: String,
}

impl Server {
    /// Acquires the lifetime lock before starting the single playback owner.
    pub async fn start(config: ServerConfig) -> Result<Self> {
        ensure!(
            config.bind.ip().is_loopback(),
            "server bind address must be loopback"
        );
        let state = StateDirectory::open(&config.state_dir)?;
        let listener = TcpListener::bind(config.bind)
            .await
            .context("bind local control socket")?;
        let address = listener.local_addr()?;
        let owner = Owner::start(OwnerConfig {
            database_path: state.root.join("state.sqlite3"),
            output: config.output,
        })?;
        Ok(Self {
            listener,
            address,
            owner,
            state,
            instance_id: Uuid::new_v4().to_string(),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
    pub fn token_path(&self) -> &Path {
        &self.state.token_path
    }

    /// Stops accepting, closes controllers, checkpoints and joins the owner before
    /// releasing the state-directory lock. Disconnecting a controller never stops audio.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let Self {
            listener,
            owner,
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
                    let (socket, peer) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => break Err(error).context("accept local controller"),
                    };
                    if !peer.ip().is_loopback() || connections.len() >= MAX_CONNECTIONS {
                        drop(socket);
                        continue;
                    }
                    connections.spawn(serve_connection(
                        socket, controller.clone(), state.token.clone(), instance_id.clone(), stopped.clone(),
                    ));
                }
            }
        };
        drop(listener);
        let _ = stop.send(true);
        while connections.join_next().await.is_some() {}
        drop(controller);
        let shutdown_result = owner.shutdown().await;
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
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set; supply --state-dir")?)
            .join("Library/Application Support/tyranny"),
    )
}

struct StateDirectory {
    root: PathBuf,
    token_path: PathBuf,
    token: Arc<str>,
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
        // All children stay under the private directory. Reject insecure existing
        // files, including SQLite sidecars, rather than silently repairing them.
        let lock = open_private_file(&root.join("server.lock"), true)?;
        lock.try_lock_exclusive()
            .context("state directory is already locked by another server")?;
        let token_path = root.join("token");
        match fs::symlink_metadata(&token_path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_token(&root, &directory)?
            }
            Err(error) => return Err(error).context("inspect authentication token"),
        }
        let token = read_token(&token_path)?.into();
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
            token_path,
            token,
            _lock: lock,
            _directory: directory,
        })
    }
}

fn validate_directory(metadata: &Metadata) -> Result<()> {
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "state directory must be a real directory, not a symlink"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "state directory must be owned by the current user"
    );
    ensure!(
        metadata.mode() & 0o777 == 0o700,
        "state directory permissions must be 0700"
    );
    Ok(())
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
        metadata.mode() & 0o777 == 0o600,
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

fn create_token(root: &Path, directory: &File) -> Result<()> {
    let temporary = root.join(format!(".token-{}", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)
        .context("create private authentication token")?;
    let result = (|| -> Result<()> {
        let mut entropy = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut entropy)?;
        let mut encoded = [0_u8; TOKEN_BYTES];
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (byte, pair) in entropy.into_iter().zip(encoded.as_chunks_mut::<2>().0) {
            pair[0] = HEX[usize::from(byte >> 4)];
            pair[1] = HEX[usize::from(byte & 15)];
        }
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(&temporary, root.join("token"))?;
        directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.context("persist authentication token")
}

/// Opens without following a final symlink; the token never appears in diagnostics.
pub fn read_token(path: &Path) -> Result<String> {
    let mut file = open_private_file(path, false)?;
    ensure!(
        file.metadata()?.len() == TOKEN_BYTES as u64,
        "authentication token has invalid length"
    );
    let mut bytes = [0_u8; TOKEN_BYTES];
    file.read_exact(&mut bytes)
        .context("read authentication token")?;
    ensure!(
        bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)),
        "authentication token has invalid encoding"
    );
    Ok(String::from_utf8(bytes.to_vec()).expect("validated ASCII token"))
}

/// Validates the destination before adding the secret. Literal IPs avoid DNS
/// rebinding and no userinfo, query string or redirect can carry credentials.
pub fn client_request(url: &str, token: &str) -> Result<ClientRequest> {
    let uri: Uri = url.parse().context("invalid WebSocket URL")?;
    ensure!(uri.scheme_str() == Some("ws"), "control URL must use ws://");
    let authority = uri.authority().context("control URL needs an authority")?;
    ensure!(
        !authority.as_str().contains('@'),
        "control URL must not contain userinfo"
    );
    let host = uri.host().context("control URL needs a host")?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let ip: IpAddr = host
        .parse()
        .context("control URL must use a literal loopback IP address")?;
    ensure!(
        ip.is_loopback(),
        "control URL must be loopback; refusing to send credentials"
    );
    ensure!(
        uri.path() == "/ws" && uri.query().is_none(),
        "control URL path must be /ws without a query"
    );
    let mut request = url
        .into_client_request()
        .context("invalid WebSocket request")?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| anyhow::anyhow!("invalid authentication token"))?;
    authorization.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, authorization);
    Ok(request)
}

struct Authenticate(Arc<str>);

impl tokio_tungstenite::tungstenite::handshake::server::Callback for Authenticate {
    fn on_request(
        self,
        request: &UpgradeRequest,
        response: UpgradeResponse,
    ) -> std::result::Result<UpgradeResponse, ErrorResponse> {
        let rejection = if request.headers().contains_key(ORIGIN) {
            Some(StatusCode::FORBIDDEN)
        } else if request.uri().path() != "/ws" || request.uri().query().is_some() {
            Some(StatusCode::NOT_FOUND)
        } else {
            let mut authorization = request.headers().get_all(AUTHORIZATION).iter();
            let supplied = authorization
                .next()
                .and_then(|value| value.as_bytes().strip_prefix(b"Bearer "));
            let authenticated = authorization.next().is_none()
                && supplied.is_some_and(|supplied| bool::from(supplied.ct_eq(self.0.as_bytes())));
            (!authenticated).then_some(StatusCode::UNAUTHORIZED)
        };
        match rejection {
            Some(status) => {
                let mut response = ErrorResponse::new(None);
                *response.status_mut() = status;
                Err(response)
            }
            None => Ok(response),
        }
    }
}

async fn serve_connection(
    socket: TcpStream,
    controller: Controller,
    token: Arc<str>,
    instance: Arc<str>,
    mut shutdown: watch::Receiver<bool>,
) {
    let configuration = WebSocketConfig::default()
        .read_buffer_size(MAX_REQUEST_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_RESPONSE_BYTES + 1024)
        .max_message_size(Some(MAX_REQUEST_BYTES))
        .max_frame_size(Some(MAX_REQUEST_BYTES));
    let handshake = accept_hdr_async_with_config(socket, Authenticate(token), Some(configuration));
    let mut socket = tokio::select! {
        biased;
        _ = shutdown.changed() => return,
        result = timeout(IO_TIMEOUT, handshake) => match result {
            Ok(Ok(socket)) => socket,
            _ => return,
        },
    };
    let subscription = tokio::select! {
        biased;
        _ = shutdown.changed() => None,
        result = controller.subscribe() => result.ok(),
    };
    let Some(mut subscription) = subscription else {
        close_socket(&mut socket, CloseCode::Away, "server stopping").await;
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
    let mut deferred = None;
    let (code, reason) = 'connection: loop {
        tokio::select! {
            _ = shutdown.changed() => break (CloseCode::Away, "server stopping"),
            received = async {
                if let Some(frame) = deferred.take() { Some(Ok(frame)) } else { socket.next().await }
            } => {
                match received {
                    Some(Ok(Frame::Text(text))) => {
                        let request: Request = match serde_json::from_str(&text) {
                            Ok(request) => request,
                            Err(_) => break (CloseCode::Invalid, "invalid request JSON"),
                        };
                        // Artwork, lyrics, tag lookups and directory searches have no mutations to replay. Observe
                        // socket closure while waiting so abandoned discovery
                        // work is cancelled. At most one
                        // next frame is retained, preserving pipelined requests
                        // in order without running them before this reply.
                        // Invariant the client's launch prefetch relies on: no `state` message
                        // is written between reading a request and writing its reply. Events
                        // are polled only after the reply (below), so a reply's connection
                        // saw no state change in between.
                        let discovery = matches!(&request.command, Command::Artwork { .. }
                                | Command::Lyrics { .. }
                                | Command::MusicbrainzLookup { .. }
                                | Command::SearchDirectory { .. }
                        );
                        let pending = controller.request(request);
                        tokio::pin!(pending);
                        let reply = loop {
                            tokio::select! {
                                biased;
                                _ = shutdown.changed() => break 'connection (CloseCode::Away, "server stopping"),
                                incoming = socket.next(), if discovery && deferred.is_none() => match incoming {
                                    Some(Ok(Frame::Ping(_) | Frame::Pong(_))) => {},
                                    Some(Ok(Frame::Close(_))) | None => break 'connection (CloseCode::Normal, "controller disconnected"),
                                    Some(Ok(frame)) => deferred = Some(frame),
                                    Some(Err(_)) => break 'connection (CloseCode::Policy, "invalid or oversized WebSocket message"),
                                },
                                result = &mut pending => break match result {
                                    Ok(reply) => reply,
                                    Err(_) => break 'connection (CloseCode::Error, "playback owner unavailable"),
                                },
                            }
                        };
                        if let Some(new_sequence) = reply_sequence(&reply) {
                            sequence = sequence.max(new_sequence);
                        }
                        if !send_message(&mut socket, &reply, &mut shutdown).await { return; }
                    }
                    Some(Ok(Frame::Ping(_))) => {
                        let flushed = tokio::select! {
                            biased;
                            _ = shutdown.changed() => false,
                            result = timeout(IO_TIMEOUT, socket.flush()) => matches!(result, Ok(Ok(()))),
                        };
                        if !flushed { return; }
                    }
                    Some(Ok(Frame::Pong(_))) => {}
                    Some(Ok(Frame::Close(_))) | None => break (CloseCode::Normal, "controller disconnected"),
                    Some(Ok(_)) => break (CloseCode::Unsupported, "text requests required"),
                    Some(Err(_)) => break (CloseCode::Policy, "invalid or oversized WebSocket message"),
                }
            }
            event = subscription.events.recv() => {
                let snapshot = match event {
                    Ok(snapshot) => snapshot,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break (CloseCode::Policy, "controller lagged; reconnect for snapshot"),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break (CloseCode::Away, "playback owner stopped"),
                };
                // Replies may advance past queued events. Never send state backwards.
                if snapshot.sequence > sequence {
                    sequence = snapshot.sequence;
                    if !send_message(&mut socket, &Message::State { snapshot }, &mut shutdown).await { return; }
                }
            }
        }
    };
    close_socket(&mut socket, code, reason).await;
}

fn reply_sequence(message: &Message) -> Option<u64> {
    match message {
        Message::Reply {
            reply:
                Reply::Ok {
                    result:
                        Response::Snapshot { snapshot }
                        | Response::Applied { snapshot, .. }
                        | Response::PlaylistApplied { snapshot, .. }
                        | Response::StationsApplied { snapshot, .. },
                },
            ..
        } => Some(snapshot.sequence),
        _ => None,
    }
}

async fn send_message(
    socket: &mut WebSocketStream<TcpStream>,
    message: &Message,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    let serialized = match serde_json::to_string(message) {
        Ok(serialized) if serialized.len() <= MAX_RESPONSE_BYTES => serialized,
        _ => return false,
    };
    if *shutdown.borrow() {
        return false;
    }
    tokio::select! {
        biased;
        _ = shutdown.changed() => false,
        result = timeout(IO_TIMEOUT, socket.send(Frame::Text(serialized.into()))) => matches!(result, Ok(Ok(()))),
    }
}

async fn close_socket(
    socket: &mut WebSocketStream<TcpStream>,
    code: CloseCode,
    reason: &'static str,
) {
    let _ = timeout(
        CLOSE_TIMEOUT,
        socket.close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })),
    )
    .await;
}
