use futures_util::{SinkExt, StreamExt};
use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{net::TcpStream, sync::oneshot, task::JoinHandle, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Error, Message as Frame,
        client::IntoClientRequest,
        http::{HeaderValue, StatusCode},
    },
};
use tyranny::{
    output::OutputConfig,
    protocol::{ControlState, ErrorCode, Message, Reply, Response, Transport},
    server::{MAX_REQUEST_BYTES, Server, ServerConfig, client_request, read_token},
};
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(5);

#[path = "support/http_streams.rs"]
mod http_streams;
#[path = "support/library_lists.rs"]
mod library_lists;
#[path = "support/saved_playlists.rs"]
mod saved_playlists;
#[path = "support/stations.rs"]
mod stations;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tyranny-server-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn config(state_dir: PathBuf, capture: PathBuf) -> ServerConfig {
    ServerConfig {
        state_dir,
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        output: OutputConfig::Capture {
            path: capture,
            clocked: true,
        },
    }
}

struct Running {
    url: String,
    token: String,
    instance_id: String,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<anyhow::Result<()>>,
}
impl Running {
    async fn start(scratch: &Scratch) -> Self {
        let server = Server::start(config(scratch.path("state"), scratch.path("capture.f32")))
            .await
            .unwrap();
        let url = format!("ws://{}/ws", server.address());
        let token = read_token(server.token_path()).unwrap();
        let instance_id = server.instance_id().to_owned();
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(server.run(async {
            let _ = stopped.await;
        }));
        Self {
            url,
            token,
            instance_id,
            shutdown,
            task,
        }
    }
    async fn connect(&self) -> Socket {
        timeout(
            DEADLINE,
            connect_async(client_request(&self.url, &self.token).unwrap()),
        )
        .await
        .unwrap()
        .unwrap()
        .0
    }
    async fn stop(self) {
        self.shutdown.send(()).unwrap();
        timeout(DEADLINE, self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

async fn receive(socket: &mut Socket) -> Message {
    timeout(DEADLINE, async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Frame::Text(text) => return serde_json::from_str(&text).unwrap(),
                Frame::Ping(_) => socket.flush().await.unwrap(),
                frame => panic!("unexpected WebSocket frame: {frame:?}"),
            }
        }
    })
    .await
    .unwrap()
}

async fn hello(socket: &mut Socket, instance: &str) -> ControlState {
    match receive(socket).await {
        Message::Hello {
            protocol_version,
            instance_id,
            snapshot,
        } => {
            assert_eq!(protocol_version, tyranny::protocol::VERSION);
            assert_eq!(instance_id, instance);
            snapshot
        }
        message => panic!("Hello was not first: {message:?}"),
    }
}

async fn reply(socket: &mut Socket, id: u64, sequence: &mut u64) -> ControlState {
    loop {
        match receive(socket).await {
            Message::State { snapshot } => {
                assert!(
                    snapshot.sequence > *sequence,
                    "stale state followed a newer reply"
                );
                *sequence = snapshot.sequence;
            }
            Message::Reply {
                id: reply_id,
                reply:
                    Reply::Ok {
                        result:
                            Response::Applied { snapshot, .. }
                            | Response::Snapshot { snapshot }
                            | Response::PlaylistApplied { snapshot, .. },
                    },
            } => {
                assert_eq!(reply_id, id);
                assert!(snapshot.sequence >= *sequence);
                *sequence = snapshot.sequence;
                return *snapshot;
            }
            message => panic!("unexpected reply: {message:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_requires_bearer_and_rejects_every_browser_origin() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let missing = running.url.as_str().into_client_request().unwrap();
    let wrong = client_request(&running.url, &"0".repeat(64)).unwrap();
    let mut origin = client_request(&running.url, &running.token).unwrap();
    origin
        .headers_mut()
        .insert("origin", HeaderValue::from_static("null"));
    let mut duplicate = client_request(&running.url, &running.token).unwrap();
    duplicate
        .headers_mut()
        .append("authorization", HeaderValue::from_static("Bearer wrong"));
    let mut wrong_path = client_request(&running.url, &running.token).unwrap();
    *wrong_path.uri_mut() = running.url.replace("/ws", "/elsewhere").parse().unwrap();
    for (request, status) in [
        (missing, StatusCode::UNAUTHORIZED),
        (wrong, StatusCode::UNAUTHORIZED),
        (origin, StatusCode::FORBIDDEN),
        (duplicate, StatusCode::UNAUTHORIZED),
        (wrong_path, StatusCode::NOT_FOUND),
    ] {
        match timeout(DEADLINE, connect_async(request)).await.unwrap() {
            Err(Error::Http(response)) => assert_eq!(response.status(), status),
            result => panic!("unsafe handshake was not rejected: {result:?}"),
        }
    }
    let mut allowed = running.connect().await;
    hello(&mut allowed, &running.instance_id).await;
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_controllers_receive_ordered_state_and_reconnect_to_current_authority() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut first = running.connect().await;
    let mut second = running.connect().await;
    let mut first_sequence = hello(&mut first, &running.instance_id).await.sequence;
    let mut second_sequence = hello(&mut second, &running.instance_id).await.sequence;
    for id in 1..=40_u64 {
        let enabled = id % 2 != 0;
        first
            .send(Frame::Text(
                serde_json::json!({
                    "id": id, "command": "set_resume", "enabled": enabled,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let applied = reply(&mut first, id, &mut first_sequence).await;
        assert_eq!(applied.settings.resume_on_start, enabled);
        loop {
            match receive(&mut second).await {
                Message::State { snapshot } => {
                    assert!(snapshot.sequence > second_sequence);
                    second_sequence = snapshot.sequence;
                    if snapshot.sequence >= applied.sequence {
                        assert_eq!(snapshot.settings.resume_on_start, enabled);
                        break;
                    }
                }
                message => panic!("unexpected controller event: {message:?}"),
            }
        }
    }
    first.close(None).await.unwrap();
    second
        .send(Frame::Text(
            r#"{"id":41,"command":"set_resume","enabled":true}"#.into(),
        ))
        .await
        .unwrap();
    reply(&mut second, 41, &mut second_sequence).await;
    let mut reconnected = running.connect().await;
    assert!(
        hello(&mut reconnected, &running.instance_id)
            .await
            .settings
            .resume_on_start
    );
    let token = running.token.clone();
    let old_instance = running.instance_id.clone();
    running.stop().await;
    let restarted = Running::start(&scratch).await;
    assert_eq!(restarted.token, token);
    assert_ne!(restarted.instance_id, old_instance);
    let mut reconnected = restarted.connect().await;
    assert!(
        hello(&mut reconnected, &restarted.instance_id)
            .await
            .settings
            .resume_on_start
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equalizer_is_durable_coherent_and_authoritative_after_reconnect() {
    use tyranny::equalizer::EqualizerSettings;

    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut first = running.connect().await;
    let mut second = running.connect().await;
    let initial = hello(&mut first, &running.instance_id).await;
    assert_eq!(initial.settings.equalizer, EqualizerSettings::default());
    let mut first_sequence = initial.sequence;
    let mut second_sequence = hello(&mut second, &running.instance_id).await.sequence;
    let mut equalizer = EqualizerSettings {
        enabled: true,
        preamp_db: -6.0,
        bands_db: [6.0, 4.0, 2.0, 0.0, -1.0, -2.0, 0.0, 1.0, 2.0, 3.0],
    };
    for (id, enabled) in [(1, true), (2, false)] {
        equalizer.enabled = enabled;
        first
            .send(Frame::Text(
                serde_json::json!({
                    "id": id, "command": "set_equalizer", "equalizer": equalizer,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let applied = reply(&mut first, id, &mut first_sequence).await;
        assert_eq!(applied.settings.equalizer, equalizer);
        assert_eq!(applied.queue_revision, initial.queue_revision);
        // The successful reply follows the durable write, not only an in-memory update.
        let database = rusqlite::Connection::open(scratch.path("state/state.sqlite3")).unwrap();
        let saved: String = database
            .query_row(
                "SELECT settings FROM control WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let saved: tyranny::protocol::Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved, applied.settings);
        loop {
            match receive(&mut second).await {
                Message::State { snapshot } => {
                    assert!(snapshot.sequence > second_sequence);
                    second_sequence = snapshot.sequence;
                    if snapshot.sequence >= applied.sequence {
                        assert_eq!(snapshot.settings, applied.settings);
                        assert_eq!(snapshot.queue_revision, applied.queue_revision);
                        break;
                    }
                }
                message => panic!("unexpected controller event: {message:?}"),
            }
        }
    }
    let mut invalid = equalizer.clone();
    invalid.bands_db[3] = 12.01;
    first
        .send(Frame::Text(
            serde_json::json!({
                "id": 3, "command": "set_equalizer", "equalizer": invalid,
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    match receive(&mut first).await {
        Message::Reply {
            id: 3,
            reply: Reply::Error { code, .. },
        } => {
            assert_eq!(code, ErrorCode::InvalidCommand);
        }
        message => panic!("invalid equalizer was not rejected: {message:?}"),
    }
    first
        .send(Frame::Text(r#"{"id":4,"command":"snapshot"}"#.into()))
        .await
        .unwrap();
    let unchanged = reply(&mut first, 4, &mut first_sequence).await;
    assert_eq!(unchanged.settings.equalizer, equalizer);
    assert_eq!(unchanged.sequence, second_sequence);
    first.close(None).await.unwrap();
    let mut reconnected = running.connect().await;
    assert_eq!(
        hello(&mut reconnected, &running.instance_id).await.settings,
        unchanged.settings
    );
    running.stop().await;
    let restarted = Running::start(&scratch).await;
    let mut reconnected = restarted.connect().await;
    assert_eq!(
        hello(&mut reconnected, &restarted.instance_id)
            .await
            .settings,
        unchanged.settings
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_binary_and_oversized_requests_disconnect_only_the_offending_controller() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut healthy = running.connect().await;
    let mut sequence = hello(&mut healthy, &running.instance_id).await.sequence;
    for frame in [
        Frame::Text("{".into()),
        Frame::Text(r#"{"id":1,"command":"seek","position_frames":0}"#.into()),
        Frame::Binary(vec![1, 2, 3].into()),
        Frame::Text("x".repeat(MAX_REQUEST_BYTES + 1).into()),
    ] {
        let mut offending = running.connect().await;
        hello(&mut offending, &running.instance_id).await;
        offending.send(frame).await.unwrap();
        timeout(DEADLINE, async {
            loop {
                match offending.next().await {
                    Some(Ok(Frame::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(Frame::Text(text))) => assert!(matches!(
                        serde_json::from_str::<Message>(&text).unwrap(),
                        Message::State { .. }
                    )),
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
        .unwrap();
    }
    healthy
        .send(Frame::Text(r#"{"id":1,"command":"snapshot"}"#.into()))
        .await
        .unwrap();
    reply(&mut healthy, 1, &mut sequence).await;
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_is_private_and_exclusively_locked_until_owner_shutdown() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    assert_eq!(
        fs::metadata(scratch.path("state")).unwrap().mode() & 0o777,
        0o700
    );
    for file in ["token", "server.lock", "state.sqlite3"] {
        assert_eq!(
            fs::metadata(scratch.path("state").join(file))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }
    let other_capture = scratch.path("other.f32");
    assert!(
        Server::start(config(scratch.path("state"), other_capture.clone()))
            .await
            .is_err()
    );
    assert!(
        !other_capture.exists(),
        "second instance reached the audio owner before taking the lock"
    );
    running.stop().await;
    let restarted = Running::start(&scratch).await;
    restarted.stop().await;
}

fn private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn insecure_state_paths_and_nonloopback_binds_fail_before_starting_audio() {
    let scratch = Scratch::new();
    let capture = scratch.path("capture.f32");
    let insecure = scratch.path("insecure");
    private_directory(&insecure);
    fs::set_permissions(&insecure, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        Server::start(config(insecure, capture.clone()))
            .await
            .is_err()
    );
    let target = scratch.path("target");
    private_directory(&target);
    let linked = scratch.path("linked");
    symlink(&target, &linked).unwrap();
    assert!(
        Server::start(config(linked, capture.clone()))
            .await
            .is_err()
    );
    symlink(scratch.path("missing-token"), target.join("token")).unwrap();
    assert!(
        Server::start(config(target.clone(), capture.clone()))
            .await
            .is_err()
    );
    assert!(
        fs::symlink_metadata(target.join("token"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let mut public = config(scratch.path("public"), capture.clone());
    public.bind = SocketAddr::from(([0, 0, 0, 0], 0));
    assert!(Server::start(public).await.is_err());
    assert!(!capture.exists());
}

#[test]
fn controller_never_attaches_credentials_to_nonloopback_or_query_urls() {
    for url in [
        "ws://192.0.2.1:9865/ws",
        "ws://example.com/ws",
        "ws://localhost:9865/ws",
        "ws://127.0.0.1:9865/ws?token=secret",
        "ws://user@127.0.0.1:9865/ws",
        "wss://127.0.0.1:9865/ws",
        "ws://[::]:9865/ws",
    ] {
        assert!(
            client_request(url, "secret").is_err(),
            "unsafe URL accepted: {url}"
        );
    }
    for url in ["ws://127.0.0.1:9865/ws", "ws://[::1]:9865/ws"] {
        let request = client_request(url, "secret").unwrap();
        assert_eq!(request.headers()["authorization"], "Bearer secret");
        assert!(request.headers()["authorization"].is_sensitive());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_cannot_overwrite_credentials_database_or_queued_media() {
    let scratch = Scratch::new();
    let source = scratch.path("queued.wav");
    fs::write(&source, b"queued source must survive capture startup").unwrap();
    let running = Running::start(&scratch).await;
    let mut socket = running.connect().await;
    let initial = hello(&mut socket, &running.instance_id).await;
    socket
        .send(Frame::Text(
            serde_json::json!({
                "id": 1, "command": "append",
                "expected_revision": initial.queue_revision,
                "path": source,
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let mut sequence = initial.sequence;
    reply(&mut socket, 1, &mut sequence).await;
    running.stop().await;
    let alias = scratch.path("source-alias.wav");
    fs::hard_link(&source, &alias).unwrap();
    for target in [
        scratch.path("state/token"),
        scratch.path("state/state.sqlite3"),
        source,
        alias,
    ] {
        let before = fs::read(&target).unwrap();
        let result = Server::start(config(scratch.path("state"), target.clone())).await;
        let rejected = result.is_err();
        if let Ok(server) = result {
            server.run(async {}).await.unwrap();
        }
        assert!(
            rejected,
            "capture accepted protected target {}",
            target.display()
        );
        assert_eq!(fs::read(&target).unwrap(), before);
    }
}

// Exercise transport through the same authenticated socket used by controllers;
// all expected positions below are server-timeline frames, not decoder state.
struct PlaybackClient {
    socket: Socket,
    id: u64,
    state: ControlState,
}

impl PlaybackClient {
    async fn connect(running: &Running) -> Self {
        let mut socket = running.connect().await;
        let state = hello(&mut socket, &running.instance_id).await;
        Self {
            socket,
            id: 0,
            state,
        }
    }

    async fn request(&mut self, mut command: serde_json::Value) -> Reply {
        self.id += 1;
        command["id"] = self.id.into();
        self.socket
            .send(Frame::Text(command.to_string().into()))
            .await
            .unwrap();
        loop {
            match receive(&mut self.socket).await {
                Message::State { snapshot } => {
                    assert!(snapshot.sequence > self.state.sequence);
                    self.state = snapshot;
                }
                Message::Reply { id, reply } => {
                    assert_eq!(id, self.id);
                    return reply;
                }
                message => panic!("unexpected message: {message:?}"),
            }
        }
    }

    async fn apply(&mut self, command: serde_json::Value) {
        match self.request(command).await {
            Reply::Ok {
                result:
                    Response::Applied { snapshot, .. }
                    | Response::Snapshot { snapshot }
                    | Response::PlaylistApplied { snapshot, .. },
            } => {
                assert!(snapshot.sequence >= self.state.sequence);
                self.state = *snapshot;
            }
            reply => panic!("command failed: {reply:?}"),
        }
    }

    async fn reject(&mut self, command: serde_json::Value, expected: ErrorCode) {
        match self.request(command).await {
            Reply::Error { code, .. } => assert_eq!(code, expected),
            reply => panic!("command unexpectedly succeeded: {reply:?}"),
        }
        self.apply(serde_json::json!({"command": "snapshot"})).await;
    }

    async fn append(&mut self, source: &Path) {
        self.apply(serde_json::json!({
            "command": "append", "expected_revision": self.state.queue_revision, "path": source,
        }))
        .await;
    }

    async fn seek(&mut self, entry_id: u64, frame: u64) {
        self.apply(serde_json::json!({
            "command": "seek", "entry_id": entry_id, "position_frames": frame,
        }))
        .await;
    }

    fn selected(&self, id: u64, title: &str) {
        assert_eq!(self.state.current_id, Some(id));
        let metadata = self.state.now_playing.as_ref().expect("selected metadata");
        assert_eq!(metadata.entry_id, id);
        assert_eq!(metadata.title, title);
        assert!(self.state.can_previous);
    }

    async fn until(&mut self, ready: impl Fn(&ControlState) -> bool) {
        timeout(DEADLINE, async {
            while !ready(&self.state) {
                match receive(&mut self.socket).await {
                    Message::State { snapshot } => {
                        assert!(snapshot.sequence > self.state.sequence);
                        self.state = snapshot;
                    }
                    message => panic!("unexpected event: {message:?}"),
                }
            }
        })
        .await
        .unwrap();
    }
}

fn tagged_wav(path: &Path, frames: u32, title: &[u8]) {
    fn chunk(destination: &mut Vec<u8>, kind: &[u8; 4], bytes: &[u8]) {
        destination.extend_from_slice(kind);
        destination.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        destination.extend_from_slice(bytes);
        if !bytes.len().is_multiple_of(2) {
            destination.push(0);
        }
    }
    let mut body = b"WAVE".to_vec();
    let mut format = Vec::new();
    format.extend_from_slice(&1_u16.to_le_bytes());
    format.extend_from_slice(&2_u16.to_le_bytes());
    format.extend_from_slice(&48_000_u32.to_le_bytes());
    format.extend_from_slice(&192_000_u32.to_le_bytes());
    format.extend_from_slice(&4_u16.to_le_bytes());
    format.extend_from_slice(&16_u16.to_le_bytes());
    chunk(&mut body, b"fmt ", &format);
    let mut tags = b"INFO".to_vec();
    for (key, value) in [
        (b"INAM", title),
        (b"IART", b"Fixture Artist".as_slice()),
        (b"IPRD", b"Fixture Album".as_slice()),
    ] {
        let mut terminated = value.to_vec();
        terminated.push(0);
        chunk(&mut tags, key, &terminated);
    }
    chunk(&mut body, b"LIST", &tags);
    chunk(&mut body, b"data", &vec![0; frames as usize * 4]);
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    fs::write(path, file).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_uses_current_order_and_three_second_boundary_without_resuming_paused_audio() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    assert!(!client.state.can_next && !client.state.can_previous);
    assert!(client.state.now_playing.is_none());
    for title in ["One", "Two", "Three"] {
        let path = scratch.path(&format!("{title}.wav"));
        tagged_wav(&path, 384_000, title.as_bytes());
        client.append(&path).await;
    }
    client
        .apply(serde_json::json!({
            "command": "move", "entry_ids": [3], "before": 2,
            "expected_revision": client.state.queue_revision,
        }))
        .await;
    client
        .apply(serde_json::json!({"command": "play", "entry_id": 1}))
        .await;
    client.apply(serde_json::json!({"command": "next"})).await;
    client.selected(3, "Three");
    assert!(matches!(
        client.state.transport,
        Transport::Playing | Transport::Buffering
    ));
    client.apply(serde_json::json!({"command": "pause"})).await;
    client.seek(3, 144_000).await;
    client
        .apply(serde_json::json!({"command": "previous"}))
        .await;
    client.selected(3, "Three");
    assert_eq!(client.state.position_frames, 0);
    assert_eq!(client.state.transport, Transport::Paused);
    client.seek(3, 143_999).await;
    client
        .apply(serde_json::json!({"command": "previous"}))
        .await;
    client.selected(1, "One");
    assert_eq!(client.state.position_frames, 0);
    client.seek(1, 48_000).await;
    client
        .apply(serde_json::json!({"command": "previous"}))
        .await;
    client.selected(1, "One");
    assert_eq!(client.state.position_frames, 0);
    client.apply(serde_json::json!({"command": "next"})).await;
    client.selected(3, "Three");
    assert_eq!(client.state.transport, Transport::Paused);
    client
        .reject(
            serde_json::json!({
                "command": "seek", "entry_id": 1, "position_frames": 48_000,
            }),
            ErrorCode::InvalidCommand,
        )
        .await;
    client.selected(3, "Three");
    assert_eq!(client.state.position_frames, 0);
    client
        .reject(
            serde_json::json!({
                "command": "seek", "entry_id": 3, "position_frames": 384_000,
            }),
            ErrorCode::Playback,
        )
        .await;
    client.apply(serde_json::json!({"command": "next"})).await;
    client.selected(2, "Two");
    assert!(!client.state.can_next);
    client
        .reject(
            serde_json::json!({"command": "next"}),
            ErrorCode::InvalidCommand,
        )
        .await;
    client.selected(2, "Two");
    client
        .apply(serde_json::json!({"command": "toggle_play_pause"}))
        .await;
    assert!(matches!(
        client.state.transport,
        Transport::Playing | Transport::Buffering
    ));
    client
        .apply(serde_json::json!({"command": "toggle_play_pause"}))
        .await;
    assert_eq!(client.state.transport, Transport::Paused);
    client.apply(serde_json::json!({"command": "pause"})).await;
    assert_eq!(client.state.transport, Transport::Paused);
    client.apply(serde_json::json!({"command": "stop"})).await;
    assert!(client.state.now_playing.is_none());
    assert_eq!(client.state.current_id, None);
    assert!(!client.state.can_next && !client.state.can_previous);
    client
        .reject(
            serde_json::json!({"command": "next"}),
            ErrorCode::InvalidCommand,
        )
        .await;
    client
        .reject(
            serde_json::json!({"command": "previous"}),
            ErrorCode::InvalidCommand,
        )
        .await;
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_metadata_tracks_consumed_occurrence_through_prefetch_end_and_reconnect() {
    let scratch = Scratch::new();
    let first = scratch.path("first.wav");
    let second = scratch.path("second.wav");
    tagged_wav(&first, 38_400, b"First title");
    tagged_wav(&second, 86_400, b"Second title");
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    client.append(&first).await;
    client.append(&second).await;
    client
        .apply(serde_json::json!({"command": "play", "entry_id": 1}))
        .await;
    client.apply(serde_json::json!({"command": "pause"})).await;
    // More owner turns than the 64-block output pool: the second source is
    // prefetched, but the paused current occurrence must still describe first.
    for _ in 0..80 {
        client
            .apply(serde_json::json!({"command": "snapshot"}))
            .await;
        client.selected(1, "First title");
    }
    let metadata = client.state.now_playing.as_ref().unwrap();
    assert_eq!(metadata.artist, "Fixture Artist");
    assert_eq!(metadata.album, "Fixture Album");
    assert_eq!(metadata.duration_frames, Some(38_400));
    assert_eq!(metadata.metadata_error, None);
    // No library scan or queue page was requested, and reopening this source is
    // now impossible. Playback, metadata events, and Hello must use open state.
    fs::remove_file(&second).unwrap();
    let mut paused_reconnect = PlaybackClient::connect(&running).await;
    paused_reconnect.selected(1, "First title");
    assert_eq!(paused_reconnect.state.transport, Transport::Paused);
    client.apply(serde_json::json!({"command": "play"})).await;
    client.until(|state| state.current_id == Some(2)).await;
    client.selected(2, "Second title");
    assert_eq!(
        client.state.now_playing.as_ref().unwrap().duration_frames,
        Some(86_400)
    );
    assert!(!client.state.can_next);
    client
        .reject(
            serde_json::json!({
                "command": "seek", "entry_id": 1, "position_frames": 0,
            }),
            ErrorCode::InvalidCommand,
        )
        .await;
    client.selected(2, "Second title");
    let reconnected = PlaybackClient::connect(&running).await;
    reconnected.selected(2, "Second title");
    client
        .until(|state| state.transport == Transport::Ended)
        .await;
    client.selected(2, "Second title");
    assert_eq!(client.state.position_frames, 86_400);
    assert!(!client.state.can_next);
    let ended_reconnect = PlaybackClient::connect(&running).await;
    ended_reconnect.selected(2, "Second title");
    assert_eq!(ended_reconnect.state.transport, Transport::Ended);
    client
        .reject(
            serde_json::json!({"command": "next"}),
            ErrorCode::InvalidCommand,
        )
        .await;
    client
        .apply(serde_json::json!({"command": "previous"}))
        .await;
    client.selected(1, "First title");
    assert_eq!(client.state.position_frames, 0);
    assert_eq!(client.state.transport, Transport::Paused);
    client.apply(serde_json::json!({"command": "stop"})).await;
    assert!(client.state.now_playing.is_none());
    paused_reconnect.socket.close(None).await.unwrap();
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_next_preserves_selection_position_metadata_and_paused_intent() {
    let scratch = Scratch::new();
    let first = scratch.path("good.wav");
    let broken = scratch.path("broken.wav");
    tagged_wav(&first, 384_000, b"Good source");
    fs::write(&broken, b"not an audio source").unwrap();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    client.append(&first).await;
    client.append(&broken).await;
    client
        .apply(serde_json::json!({"command": "play", "entry_id": 1}))
        .await;
    client.apply(serde_json::json!({"command": "pause"})).await;
    client.seek(1, 48_000).await;
    let metadata = client.state.now_playing.clone();
    assert!(client.state.can_next);
    client
        .reject(serde_json::json!({"command": "next"}), ErrorCode::Playback)
        .await;
    client.selected(1, "Good source");
    assert_eq!(client.state.now_playing, metadata);
    assert_eq!(client.state.position_frames, 48_000);
    assert_eq!(client.state.transport, Transport::Paused);
    assert!(client.state.error.is_none());
    let reconnected = PlaybackClient::connect(&running).await;
    reconnected.selected(1, "Good source");
    assert_eq!(reconnected.state.position_frames, 48_000);
    assert_eq!(reconnected.state.transport, Transport::Paused);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_display_metadata_warns_without_breaking_audio_or_seek() {
    let scratch = Scratch::new();
    let source = scratch.path("fallback-title.wav");
    tagged_wav(&source, 384_000, &vec![b'x'; 65_537]);
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    client.append(&source).await;
    client
        .apply(serde_json::json!({"command": "play", "entry_id": 1}))
        .await;
    client.until(|state| state.position_frames > 0).await;
    client.selected(1, "fallback-title");
    let metadata = client.state.now_playing.as_ref().unwrap();
    assert_eq!(metadata.artist, "Unknown Artist");
    assert_eq!(metadata.album, "Unknown Album");
    assert_eq!(metadata.duration_frames, Some(384_000));
    assert!(
        metadata
            .metadata_error
            .as_ref()
            .unwrap()
            .contains("tag limits")
    );
    assert!(client.state.error.is_none());
    client.apply(serde_json::json!({"command": "pause"})).await;
    client.seek(1, 48_000).await;
    client.selected(1, "fallback-title");
    assert_eq!(client.state.position_frames, 48_000);
    assert_eq!(client.state.transport, Transport::Paused);
    assert!(
        client
            .state
            .now_playing
            .as_ref()
            .unwrap()
            .metadata_error
            .is_some()
    );
    assert!(client.state.error.is_none());
    running.stop().await;
}

#[tokio::test]
async fn artwork_keeps_pipelined_commands_ordered_and_queue_state_unchanged() {
    let scratch = Scratch::new();
    let source = scratch.path("direct.wav");
    tagged_wav(&source, 480, b"Artwork direct path");
    let running = Running::start(&scratch).await;
    let mut control = PlaybackClient::connect(&running).await;
    control.append(&source).await;
    let before = control.state.clone();
    let mut socket = running.connect().await;
    hello(&mut socket, &running.instance_id).await;
    for command in [
        serde_json::json!({"id":1,"command":"artwork","album_id":null,"entry_id":1,
            "library_revision":before.library.revision,"size":"display"}),
        serde_json::json!({"id":2,"command":"snapshot"}),
    ] {
        socket
            .send(Frame::Text(command.to_string().into()))
            .await
            .unwrap();
    }
    let mut expected = 1;
    while expected <= 2 {
        match receive(&mut socket).await {
            Message::State { .. } => {}
            Message::Reply { id, reply } => {
                assert_eq!(id, expected);
                match (id, reply) {
                    (
                        1,
                        Reply::Ok {
                            result: Response::Artwork { artwork },
                        },
                    ) => {
                        assert_eq!(artwork.status, tyranny::protocol::ArtworkStatus::Missing);
                        assert!(artwork.data.is_none());
                    }
                    (
                        2,
                        Reply::Ok {
                            result: Response::Snapshot { snapshot },
                        },
                    ) => {
                        assert_eq!(snapshot.queue_revision, before.queue_revision);
                        assert_eq!(snapshot.current_id, before.current_id);
                        assert_eq!(snapshot.error, before.error);
                    }
                    other => panic!("unexpected pipelined reply: {other:?}"),
                }
                expected += 1;
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
    socket.close(None).await.unwrap();
    control.socket.close(None).await.unwrap();
    running.stop().await;
}

/// The next reply's JSON, skipping state messages.
async fn raw_reply(socket: &mut Socket, id: u64) -> serde_json::Value {
    timeout(DEADLINE, async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Frame::Text(text) => {
                    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if value["type"] == "state" {
                        continue;
                    }
                    assert_eq!(value["type"], "reply", "{value}");
                    assert_eq!(value["id"], id, "{value}");
                    return value;
                }
                Frame::Ping(_) => socket.flush().await.unwrap(),
                frame => panic!("unexpected WebSocket frame: {frame:?}"),
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tag_commands_answer_without_network() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut socket = running.connect().await;
    let snapshot = hello(&mut socket, &running.instance_id).await;
    let revision = snapshot.library.revision;
    let refused = |code: &str, message: &str| {
        serde_json::json!({"type":"reply","id":0,"status":"error","code":code,
            "message":message,"queue_revision":snapshot.queue_revision})
    };
    let cases = [
        (
            serde_json::json!({"command":"tag_edits"}),
            serde_json::json!({"type":"reply","id":0,"status":"ok",
                "result":{"kind":"tag_edits","edits":[]}}),
        ),
        (
            serde_json::json!({"command":"musicbrainz_lookup","album_id":1,"library_revision":0}),
            refused(
                "stale_library_revision",
                &format!("library revision is {revision}"),
            ),
        ),
        (
            serde_json::json!({"command":"musicbrainz_lookup","album_id":999,
                "library_revision":revision}),
            refused("invalid_command", "unknown album"),
        ),
        (
            serde_json::json!({"command":"apply_tags","proposal_id":"x","track_ids":[1]}),
            refused("invalid_command", "lookup expired, look the album up again"),
        ),
        (
            serde_json::json!({"command":"revert_tag_edit","edit_id":99}),
            refused("invalid_command", "unknown tag edit"),
        ),
    ];
    for (id, (mut command, mut expected)) in (1..).zip(cases) {
        command["id"] = id.into();
        expected["id"] = id.into();
        socket
            .send(Frame::Text(command.to_string().into()))
            .await
            .unwrap();
        assert_eq!(raw_reply(&mut socket, id).await, expected, "{command}");
    }
    socket.close(None).await.unwrap();
    running.stop().await;
}
