use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::UnixStream, sync::oneshot, task::JoinHandle, time::timeout};
use tyranny_likes_smoke::{
    output::OutputConfig,
    protocol::{ControlState, ErrorCode, Message, Reply, Response, Transport},
    server::{Server, ServerConfig},
    transport::{
        FrameReader, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, connect, validate_socket_path,
        write_frame,
    },
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
struct Socket {
    stream: UnixStream,
    reader: FrameReader,
}

impl Socket {
    async fn send(&mut self, json: String) -> std::io::Result<()> {
        write_frame(&mut self.stream, json.as_bytes(), MAX_REQUEST_BYTES).await
    }

    async fn next(&mut self) -> std::io::Result<Option<Message>> {
        self.reader
            .read(&mut self.stream, MAX_RESPONSE_BYTES)
            .await?
            .map(|bytes| serde_json::from_slice(bytes).map_err(std::io::Error::other))
            .transpose()
    }

    async fn close(&mut self) -> std::io::Result<()> {
        self.stream.shutdown().await
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("tyr-{}", Uuid::new_v4().simple()));
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
        output: OutputConfig::Capture {
            path: capture,
            clocked: true,
        },
    }
}

struct Running {
    socket_path: PathBuf,
    instance_id: String,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<anyhow::Result<()>>,
}
impl Running {
    async fn start(scratch: &Scratch) -> Self {
        let server = Server::start(config(scratch.path("state"), scratch.path("capture.f32")))
            .await
            .unwrap();
        let socket_path = server.socket_path().to_owned();
        let instance_id = server.instance_id().to_owned();
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(server.run(async {
            let _ = stopped.await;
        }));
        Self {
            socket_path,
            instance_id,
            shutdown,
            task,
        }
    }
    async fn connect(&self) -> Socket {
        let stream = timeout(DEADLINE, connect(&self.socket_path))
            .await
            .unwrap()
            .unwrap();
        Socket {
            stream,
            reader: FrameReader::default(),
        }
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
    timeout(DEADLINE, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

async fn hello(socket: &mut Socket, instance: &str) -> ControlState {
    match receive(socket).await {
        Message::Hello {
            protocol_version,
            instance_id,
            snapshot,
        } => {
            assert_eq!(protocol_version, tyranny_likes_smoke::protocol::VERSION);
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
                            | Response::PlaylistApplied { snapshot, .. }
                            | Response::StationsApplied { snapshot, .. },
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

#[cfg(target_os = "macos")]
#[tokio::test]
async fn hello_reaches_a_client_with_receive_credit_below_default_send_low_water() {
    use std::os::fd::AsRawFd;
    let scratch = Scratch::new();
    let server = Server::start(config(scratch.path("state"), scratch.path("capture.f32")))
        .await
        .unwrap();
    // Configure before the accept loop begins, so the regression is deterministic.
    let socket = std::os::unix::net::UnixStream::connect(server.socket_path()).unwrap();
    let size: libc::c_int = 1024;
    assert_eq!(
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        },
        0
    );
    socket.set_nonblocking(true).unwrap();
    let mut socket = UnixStream::from_std(socket).unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let mut reader = FrameReader::default();
    let hello = timeout(
        Duration::from_secs(2),
        reader.read(&mut socket, MAX_RESPONSE_BYTES),
    )
    .await;
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    let message: Message = serde_json::from_slice(
        hello
            .expect("Hello stalled below send low-water")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(message, Message::Hello { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragmented_and_coalesced_requests_preserve_order_across_state_events() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut socket = running.connect().await;
    let mut sequence = hello(&mut socket, &running.instance_id).await.sequence;
    let mut other = PlaybackClient::connect(&running).await;
    let payload = br#"{"id":1,"command":"snapshot"}"#;
    let header = (payload.len() as u32).to_be_bytes();
    socket.stream.write_all(&header[..2]).await.unwrap();
    other
        .apply(serde_json::json!({"command":"set_resume","enabled":true}))
        .await;
    // Receiving the event forces the server's select to cancel its partial read.
    assert!(matches!(receive(&mut socket).await, Message::State { .. }));
    socket.stream.write_all(&header[2..]).await.unwrap();
    socket.stream.write_all(&payload[..7]).await.unwrap();
    other
        .apply(serde_json::json!({"command":"set_resume","enabled":false}))
        .await;
    assert!(matches!(receive(&mut socket).await, Message::State { .. }));
    let second = br#"{"id":2,"command":"snapshot"}"#;
    let mut rest = payload[7..].to_vec();
    rest.extend_from_slice(&(second.len() as u32).to_be_bytes());
    rest.extend_from_slice(second);
    socket.stream.write_all(&rest).await.unwrap();
    reply(&mut socket, 1, &mut sequence).await;
    reply(&mut socket, 2, &mut sequence).await;
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
            .send(
                serde_json::json!({
                    "id": id, "command": "set_resume", "enabled": enabled,
                })
                .to_string(),
            )
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
    first.close().await.unwrap();
    second
        .send(r#"{"id":41,"command":"set_resume","enabled":true}"#.into())
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
    let old_instance = running.instance_id.clone();
    running.stop().await;
    let restarted = Running::start(&scratch).await;
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
    use tyranny_likes_smoke::equalizer::EqualizerSettings;

    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut first = running.connect().await;
    let mut second = running.connect().await;
    let initial = hello(&mut first, &running.instance_id).await;
    assert_eq!(initial.settings.equalizer, EqualizerSettings::default());
    let mut first_sequence = initial.sequence;
    let mut second_sequence = hello(&mut second, &running.instance_id).await.sequence;
    let mut equalizer = EqualizerSettings::from_graphic(
        true,
        -6.0,
        [6.0, 4.0, 2.0, 0.0, -1.0, -2.0, 0.0, 1.0, 2.0, 3.0],
    );
    for (id, enabled) in [(1, true), (2, false)] {
        equalizer.enabled = enabled;
        first
            .send(
                serde_json::json!({
                    "id": id, "command": "set_equalizer", "equalizer": equalizer,
                })
                .to_string(),
            )
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
        let saved: tyranny_likes_smoke::protocol::Settings = serde_json::from_str(&saved).unwrap();
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
    invalid.bands[3].gain_db = 24.01;
    first
        .send(
            serde_json::json!({
                "id": 3, "command": "set_equalizer", "equalizer": invalid,
            })
            .to_string(),
        )
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
        .send(r#"{"id":4,"command":"snapshot"}"#.into())
        .await
        .unwrap();
    let unchanged = reply(&mut first, 4, &mut first_sequence).await;
    assert_eq!(unchanged.settings.equalizer, equalizer);
    assert_eq!(unchanged.sequence, second_sequence);
    first.close().await.unwrap();
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
async fn equalizer_preset_wire_lifecycle_survives_restart() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut socket = running.connect().await;
    let initial = hello(&mut socket, &running.instance_id).await;
    let mut sequence = initial.sequence;
    let curve =
        tyranny_likes_smoke::equalizer::EqualizerSettings::from_graphic(true, -6.0, [3.0; 10]);
    for (id, command) in [
        (
            1,
            serde_json::json!({"command":"save_equalizer_preset","name":"First","equalizer":curve}),
        ),
        (
            2,
            serde_json::json!({"command":"rename_equalizer_preset","name":"First","new_name":"Saved"}),
        ),
        (
            3,
            serde_json::json!({"command":"save_equalizer_preset","name":"Temporary","equalizer":curve}),
        ),
        (
            4,
            serde_json::json!({"command":"remove_equalizer_preset","name":"Temporary"}),
        ),
    ] {
        let mut request = command;
        request["id"] = serde_json::json!(id);
        socket.send(request.to_string()).await.unwrap();
        let state = reply(&mut socket, id, &mut sequence).await;
        assert_eq!(state.settings.equalizer, initial.settings.equalizer);
        assert_eq!(state.queue_revision, initial.queue_revision);
    }
    running.stop().await;
    let restarted = Running::start(&scratch).await;
    let mut socket = restarted.connect().await;
    let state = hello(&mut socket, &restarted.instance_id).await;
    assert_eq!(state.settings.equalizer, initial.settings.equalizer);
    assert_eq!(state.settings.equalizer_presets.len(), 1);
    assert_eq!(state.settings.equalizer_presets[0].name, "Saved");
    assert_eq!(state.settings.equalizer_presets[0].equalizer, curve);
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_lengths_json_and_premature_eof_disconnect_only_offender() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut healthy = running.connect().await;
    let mut sequence = hello(&mut healthy, &running.instance_id).await.sequence;
    let mut malformed = Vec::new();
    for payload in [
        b"{".as_slice(),
        br#"{"id":1,"command":"seek","position_frames":0}"#,
        &[0xff],
    ] {
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(payload);
        malformed.push(frame);
    }
    malformed.extend([
        0_u32.to_be_bytes().to_vec(),
        (MAX_REQUEST_BYTES as u32 + 1).to_be_bytes().to_vec(),
        u32::MAX.to_be_bytes().to_vec(),
        vec![0, 0],
        vec![0, 0, 0, 8, b'{'],
    ]);
    for (index, bytes) in malformed.into_iter().enumerate() {
        let mut offending = running.connect().await;
        hello(&mut offending, &running.instance_id).await;
        offending.stream.write_all(&bytes).await.unwrap();
        if index >= 6 {
            offending.stream.shutdown().await.unwrap();
        }
        timeout(DEADLINE, async {
            while let Ok(Some(message)) = offending.next().await {
                assert!(matches!(message, Message::State { .. }));
            }
        })
        .await
        .unwrap();
    }
    healthy
        .send(r#"{"id":1,"command":"snapshot"}"#.into())
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
    assert!(!scratch.path("state/token").exists());
    for file in ["control.sock", "server.lock", "state.sqlite3"] {
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
    assert!(!scratch.path("state/control.sock").exists());
    let restarted = Running::start(&scratch).await;
    restarted.stop().await;
}

fn private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn insecure_state_and_socket_paths_fail_before_starting_audio() {
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
    symlink(scratch.path("missing-socket"), target.join("control.sock")).unwrap();
    assert!(
        Server::start(config(target.clone(), capture.clone()))
            .await
            .is_err()
    );
    assert!(
        fs::symlink_metadata(target.join("control.sock"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!capture.exists());
}

#[tokio::test]
async fn writable_ancestors_are_rejected_by_server_and_client() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let original = fs::metadata(&scratch.0).unwrap().permissions();
    for mode in [0o777, 0o770] {
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(mode)).unwrap();
        let connection = connect(&running.socket_path).await;
        let startup = Server::start(config(
            scratch.path("other-state"),
            scratch.path("other.f32"),
        ))
        .await;
        let startup_rejected = startup.is_err();
        if let Ok(server) = startup {
            server.run(async {}).await.unwrap();
        }
        fs::set_permissions(&scratch.0, original.clone()).unwrap();
        assert!(connection.is_err(), "client trusted a writable ancestor");
        assert!(startup_rejected, "server trusted a writable ancestor");
        assert!(!scratch.path("other.f32").exists());
    }
    // /tmp's canonical root-owned sticky ancestor remains supported.
    let mut connection = running.connect().await;
    hello(&mut connection, &running.instance_id).await;
    running.stop().await;
}

#[tokio::test]
async fn client_resolves_ancestor_aliases_before_applying_the_socket_byte_limit() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let alias = scratch.path(&"a".repeat(100));
    symlink(&scratch.0, &alias).unwrap();
    let path = alias.join("state/control.sock");
    assert!(validate_socket_path(&path).is_err());
    let stream = connect(&path).await.unwrap();
    let mut socket = Socket {
        stream,
        reader: FrameReader::default(),
    };
    hello(&mut socket, &running.instance_id).await;
    running.stop().await;
}

#[tokio::test]
async fn client_rejects_unsafe_socket_paths_and_permissions() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    assert!(
        connect(Path::new("control.sock"))
            .await
            .unwrap_err()
            .to_string()
            .contains("must be absolute")
    );
    let linked = scratch.path("state/alias.sock");
    symlink(&running.socket_path, &linked).unwrap();
    assert!(connect(&linked).await.is_err());
    let regular = scratch.path("state/not-socket");
    fs::write(&regular, b"").unwrap();
    fs::set_permissions(&regular, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(connect(&regular).await.is_err());
    fs::set_permissions(&running.socket_path, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(connect(&running.socket_path).await.is_err());
    fs::set_permissions(&running.socket_path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(scratch.path("state"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(connect(&running.socket_path).await.is_err());
    fs::set_permissions(scratch.path("state"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(validate_socket_path(Path::new(&"x".repeat(200))).is_err());
    running.stop().await;
}

#[tokio::test]
async fn stale_socket_is_replaced_but_live_listener_is_not_unlinked() {
    let scratch = Scratch::new();
    let state = scratch.path("state");
    private_directory(&state);
    let path = state.join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    assert!(
        Server::start(config(state.clone(), scratch.path("capture.f32")))
            .await
            .is_err()
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert!(!scratch.path("capture.f32").exists());
    drop(listener);
    let running = Running::start(&scratch).await;
    let mut socket = running.connect().await;
    hello(&mut socket, &running.instance_id).await;
    running.stop().await;
    assert!(!path.exists());
}

#[tokio::test]
async fn cleanup_never_unlinks_a_replacement_socket() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let path = running.socket_path.clone();
    fs::remove_file(&path).unwrap();
    let replacement = std::os::unix::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    running.stop().await;
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    drop(replacement);
}

#[tokio::test]
async fn insecure_existing_socket_and_regular_file_are_never_removed() {
    let scratch = Scratch::new();
    let state = scratch.path("state");
    private_directory(&state);
    let path = state.join("control.sock");
    fs::write(&path, b"not a socket").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        Server::start(config(state.clone(), scratch.path("capture.f32")))
            .await
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), b"not a socket");
    fs::remove_file(&path).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    drop(listener);
    let inode = fs::metadata(&path).unwrap().ino();
    assert!(
        Server::start(config(state, scratch.path("capture.f32")))
            .await
            .is_err()
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert!(!scratch.path("capture.f32").exists());
}

#[tokio::test]
async fn obsolete_token_is_inert_and_failed_owner_start_cleans_socket() {
    let scratch = Scratch::new();
    let state = scratch.path("state");
    private_directory(&state);
    symlink(scratch.path("missing"), state.join("token")).unwrap();
    let running = Running::start(&scratch).await;
    let mut socket = running.connect().await;
    hello(&mut socket, &running.instance_id).await;
    running.stop().await;
    assert!(
        fs::symlink_metadata(state.join("token"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        Server::start(config(state.clone(), state.join("state.sqlite3")))
            .await
            .is_err()
    );
    assert!(!state.join("control.sock").exists());
    let restarted = Running::start(&scratch).await;
    restarted.stop().await;
}

#[tokio::test]
async fn response_reader_rejects_invalid_lengths_and_partial_eof() {
    for header in [0_u32, MAX_RESPONSE_BYTES as u32 + 1, u32::MAX] {
        let (mut writer, mut reader_socket) = UnixStream::pair().unwrap();
        writer.write_all(&header.to_be_bytes()).await.unwrap();
        let mut reader = FrameReader::default();
        let error = timeout(
            DEADLINE,
            reader.read(&mut reader_socket, MAX_RESPONSE_BYTES),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
    for bytes in [vec![0, 0], vec![0, 0, 0, 5, b'{']] {
        let (mut writer, mut reader_socket) = UnixStream::pair().unwrap();
        writer.write_all(&bytes).await.unwrap();
        writer.shutdown().await.unwrap();
        let mut reader = FrameReader::default();
        let error = reader
            .read(&mut reader_socket, MAX_RESPONSE_BYTES)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_cannot_overwrite_database_or_queued_media() {
    let scratch = Scratch::new();
    let source = scratch.path("queued.wav");
    fs::write(&source, b"queued source must survive capture startup").unwrap();
    let running = Running::start(&scratch).await;
    let mut socket = running.connect().await;
    let initial = hello(&mut socket, &running.instance_id).await;
    socket
        .send(
            serde_json::json!({
                "id": 1, "command": "append",
                "expected_revision": initial.queue_revision,
                "path": source,
            })
            .to_string(),
        )
        .await
        .unwrap();
    let mut sequence = initial.sequence;
    reply(&mut socket, 1, &mut sequence).await;
    running.stop().await;
    let alias = scratch.path("source-alias.wav");
    fs::hard_link(&source, &alias).unwrap();
    for target in [scratch.path("state/state.sqlite3"), source, alias] {
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

// Exercise transport through the same private socket used by controllers;
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
        self.socket.send(command.to_string()).await.unwrap();
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
    paused_reconnect.socket.close().await.unwrap();
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
            "library_revision":before.library.revision,"size":"display",
            "providers":["cover_art_archive"],"discogs_token":null}),
        serde_json::json!({"id":2,"command":"snapshot"}),
    ] {
        socket.send(command.to_string()).await.unwrap();
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
                        assert_eq!(
                            artwork.status,
                            tyranny_likes_smoke::protocol::ArtworkStatus::Missing
                        );
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
    socket.close().await.unwrap();
    control.socket.close().await.unwrap();
    running.stop().await;
}

/// The next reply's JSON, skipping state messages.
async fn raw_reply(socket: &mut Socket, id: u64) -> serde_json::Value {
    timeout(DEADLINE, async {
        loop {
            let bytes = socket
                .reader
                .read(&mut socket.stream, MAX_RESPONSE_BYTES)
                .await
                .unwrap()
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            if value["type"] == "state" {
                continue;
            }
            assert_eq!(value["type"], "reply", "{value}");
            assert_eq!(value["id"], id, "{value}");
            return value;
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
        socket.send(command.to_string()).await.unwrap();
        assert_eq!(raw_reply(&mut socket, id).await, expected, "{command}");
    }
    socket.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn search_directory_rejects_empty_query() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut socket = running.connect().await;
    hello(&mut socket, &running.instance_id).await;
    socket
        .send(serde_json::json!({"id":1,"command":"search_directory","query":"  "}).to_string())
        .await
        .unwrap();
    loop {
        match receive(&mut socket).await {
            Message::State { .. } => {}
            Message::Reply { id, reply } => {
                assert_eq!(id, 1);
                assert!(matches!(
                    reply,
                    Reply::Error {
                        code: tyranny_likes_smoke::protocol::ErrorCode::InvalidCommand,
                        ..
                    }
                ));
                break;
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
    socket.close().await.unwrap();
    running.stop().await;
}

async fn stats_plays(client: &mut PlaybackClient) -> u64 {
    match client
        .request(serde_json::json!({
            "command":"stats","period":{"kind":"all","anchor":"2026-10-21"}
        }))
        .await
    {
        Reply::Ok {
            result: Response::Stats { report, .. },
        } => report.plays,
        reply => panic!("stats failed: {reply:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_reply_reflects_recorded_plays() {
    let scratch = Scratch::new();
    let source = scratch.path("short.wav");
    tagged_wav(&source, 9_600, b"Short");
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    assert_eq!(stats_plays(&mut client).await, 0);
    client.append(&source).await;
    client
        .apply(serde_json::json!({"command": "play", "entry_id": 1}))
        .await;
    timeout(DEADLINE, async {
        loop {
            if stats_plays(&mut client).await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the finished play never reached stats");
    match client
        .request(serde_json::json!({"command":"stats_years"}))
        .await
    {
        Reply::Ok {
            result: Response::StatsYears { years },
        } => assert_eq!(years.len(), 1),
        reply => panic!("stats_years failed: {reply:?}"),
    }
    match client
        .request(serde_json::json!({"command":"wrapped","year":2000}))
        .await
    {
        Reply::Ok {
            result: Response::Wrapped { report, .. },
        } => assert_eq!(report.plays, 0),
        reply => panic!("wrapped failed: {reply:?}"),
    }
    client.socket.close().await.unwrap();
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_anchor_is_an_error_reply() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    for anchor in ["2026-02-30", "yesterday", "2026-1-1"] {
        match client
            .request(serde_json::json!({
                "command":"stats","period":{"kind":"week","anchor":anchor}
            }))
            .await
        {
            Reply::Error {
                code: ErrorCode::InvalidCommand,
                message,
                ..
            } => assert_eq!(message, "invalid period anchor"),
            reply => panic!("{anchor} was not rejected: {reply:?}"),
        }
    }
    client.socket.close().await.unwrap();
    running.stop().await;
}
