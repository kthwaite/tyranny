//! Run selections resolve off the playback owner and commit on it.
use super::*;
use crate::{
    library_types::ScanPhase,
    listing::{Browser, Grouping, ListDescription, ListSource},
    protocol::{Playlist, SourceKind},
};
use std::sync::Mutex;
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(10);

/// A 48-frame silent stereo WAV.
fn wav(path: &Path) {
    wav_frames(path, 48);
}
/// A silent stereo WAV `frames` long.
fn wav_frames(path: &Path, frames: u32) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&48_000u32.to_le_bytes());
    bytes.extend_from_slice(&192_000u32.to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 4).to_le_bytes());
    bytes.resize(bytes.len() + frames as usize * 4, 0);
    fs::write(path, bytes).unwrap();
}

struct Fixture {
    root: PathBuf,
    owner: Option<Owner>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("tyranny-owner-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("music")).unwrap();
        let owner = Owner::start(OwnerConfig {
            database_path: root.join("state.sqlite"),
            output: OutputConfig::Capture {
                path: root.join("capture.f32"),
                clocked: true,
            },
        })
        .unwrap();
        Self {
            root,
            owner: Some(owner),
        }
    }
    fn control(&self) -> Controller {
        self.owner.as_ref().unwrap().controller()
    }
    fn music(&self, name: &str) -> PathBuf {
        self.root.join("music").join(name)
    }
    /// Indexes one WAV per name under a new root. Returns the root ID.
    async fn index(&self, names: &[&str]) -> u64 {
        for name in names {
            wav(&self.music(&format!("{name}.wav")));
        }
        let path = self.music("").to_str().unwrap().to_owned();
        let Response::RootAdded { root } = ok(&self.control(), Command::AddRoot { path }).await
        else {
            panic!("expected a new root");
        };
        self.scanned(names.len() as u64).await;
        root.id
    }
    /// Waits for a completed scan with `tracks` tracks.
    async fn scanned(&self, tracks: u64) -> ControlState {
        timeout(DEADLINE, async {
            loop {
                let state = snapshot(&self.control()).await;
                if state.library.scan.phase == ScanPhase::Complete && state.library.tracks == tracks
                {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("scan deadline exceeded")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let _ = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(owner.shutdown())
            })
            .join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn ask(control: &Controller, command: Command) -> Reply {
    let message = timeout(DEADLINE, control.request(Request { id: 1, command }))
        .await
        .expect("owner reply deadline exceeded")
        .unwrap();
    let Message::Reply { reply, .. } = message else {
        panic!("expected a reply: {message:?}");
    };
    reply
}
async fn ok(control: &Controller, command: Command) -> Response {
    match ask(control, command).await {
        Reply::Ok { result } => result,
        reply => panic!("command failed: {reply:?}"),
    }
}
async fn snapshot(control: &Controller) -> ControlState {
    let Response::Snapshot { snapshot } = ok(control, Command::Snapshot).await else {
        panic!("expected a snapshot");
    };
    *snapshot
}

/// Every library row.
fn songs(library_revision: u64) -> TrackSelection {
    TrackSelection::List {
        list: ListDescription {
            source: ListSource::Library,
            browser: Browser::default(),
            query: String::new(),
            sort: Vec::new(),
            group: Grouping::None,
        },
        library_revision,
        ranges: vec![[0, u64::MAX]],
        excluded: Vec::new(),
        first_row_id: None,
    }
}
fn enqueue(state: &ControlState) -> Command {
    Command::EnqueueTracks {
        selection: songs(state.library.revision),
        expected_revision: state.queue_revision,
        placement: Placement::Append,
        play: false,
        source: QueueSource::stamped(SourceKind::Songs, "Songs"),
    }
}
fn add_selection(state: &ControlState, playlist: &Playlist) -> Command {
    Command::PlaylistAddSelection {
        playlist_id: playlist.id,
        expected_revision: playlist.revision,
        selection: songs(state.library.revision),
        before: None,
    }
}

/// Holds each resolved selection on its worker until the test releases it.
struct Gate {
    resolved: mpsc::UnboundedReceiver<()>,
    release: startup::Sender<()>,
}
fn gated(control: &Controller) -> (Controller, Gate) {
    let (resolved_tx, resolved) = mpsc::unbounded_channel();
    let (release, release_rx) = startup::channel();
    let release_rx = Mutex::new(release_rx);
    let mut resolver = (*control.resolver).clone();
    resolver.resolved = Some(Arc::new(move || {
        let _ = resolved_tx.send(());
        let _ = release_rx.lock().unwrap().recv();
    }));
    let mut gated = control.clone();
    gated.resolver = Arc::new(resolver);
    (gated, Gate { resolved, release })
}
impl Gate {
    async fn wait(&mut self) {
        timeout(DEADLINE, self.resolved.recv())
            .await
            .expect("resolution deadline exceeded")
            .expect("the request finished without resolving off the playback owner");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controls_answer_while_a_selection_resolves() {
    let fixture = Fixture::new();
    fixture.index(&["a", "b", "c"]).await;
    let control = fixture.control();
    let state = snapshot(&control).await;
    let (gated, mut gate) = gated(&control);
    let command = enqueue(&state);
    let pending = tokio::spawn(async move { ask(&gated, command).await });
    gate.wait().await;
    // The owner answers while the resolved selection waits.
    let paused = timeout(Duration::from_millis(500), ask(&control, Command::Pause))
        .await
        .expect("pause must not wait for selection resolution");
    assert!(matches!(paused, Reply::Ok { .. }), "{paused:?}");
    assert!(!pending.is_finished());
    gate.release.send(()).unwrap();
    let Reply::Ok {
        result: Response::Applied { snapshot, entry_id },
    } = pending.await.unwrap()
    else {
        panic!("the enqueue was not applied");
    };
    assert_eq!(snapshot.queue_length, 3);
    assert_eq!(snapshot.queue_revision, state.queue_revision + 1);
    assert!(entry_id.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_library_change_after_resolution_is_stale_at_commit() {
    let fixture = Fixture::new();
    let root = fixture.index(&["a", "b", "c"]).await;
    let control = fixture.control();
    let state = snapshot(&control).await;
    let Response::PlaylistApplied {
        playlist: Some(playlist),
        ..
    } = ok(
        &control,
        Command::CreatePlaylist {
            expected_revision: state.playlists_revision,
            name: "Mix".into(),
        },
    )
    .await
    else {
        panic!("expected a new playlist");
    };
    let mut tracks = 3;
    for add_to_playlist in [false, true] {
        let before = snapshot(&control).await;
        let command = if add_to_playlist {
            add_selection(&before, &playlist)
        } else {
            enqueue(&before)
        };
        let (gated, mut gate) = gated(&control);
        let pending = tokio::spawn(async move { ask(&gated, command).await });
        gate.wait().await;
        // A rescan installs one more track between resolution and commit.
        tracks += 1;
        wav(&fixture.music(&format!("new {tracks}.wav")));
        ok(&control, Command::Rescan { root_id: root }).await;
        let rescanned = fixture.scanned(tracks).await;
        assert!(rescanned.library.revision > before.library.revision);
        gate.release.send(()).unwrap();
        match pending.await.unwrap() {
            Reply::Error {
                code,
                queue_revision,
                ..
            } => {
                assert_eq!(code, ErrorCode::StaleLibraryRevision);
                assert_eq!(queue_revision, before.queue_revision);
            }
            reply => panic!("a stale selection was committed: {reply:?}"),
        }
        let after = snapshot(&control).await;
        assert_eq!(
            (
                after.queue_revision,
                after.queue_length,
                after.playlists_revision
            ),
            (
                before.queue_revision,
                before.queue_length,
                before.playlists_revision
            )
        );
    }
    // Resolved again under the current revision, the same run commits.
    let state = snapshot(&control).await;
    let Response::Applied { snapshot, .. } = ok(&control, enqueue(&state)).await else {
        panic!("the enqueue was not applied");
    };
    assert_eq!(snapshot.queue_length, tracks as usize);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enqueue_playlist_resolves_off_the_owner_and_rechecks_at_commit() {
    let fixture = Fixture::new();
    let root = fixture.index(&["a", "b", "c"]).await;
    let control = fixture.control();
    let state = snapshot(&control).await;
    let Response::PlaylistApplied {
        playlist: Some(playlist),
        ..
    } = ok(
        &control,
        Command::CreatePlaylist {
            expected_revision: state.playlists_revision,
            name: "Mix".into(),
        },
    )
    .await
    else {
        panic!("expected a new playlist");
    };
    let Response::PlaylistApplied {
        playlist: Some(playlist),
        ..
    } = ok(&control, add_selection(&state, &playlist)).await
    else {
        panic!("expected the filled playlist");
    };
    let enqueue_playlist = |state: &ControlState| Command::EnqueuePlaylist {
        playlist_id: playlist.id,
        playlist_revision: playlist.revision,
        library_revision: state.library.revision,
        expected_revision: state.queue_revision,
        play: false,
    };
    // The owner answers while the playlist's sources resolve.
    let before = snapshot(&control).await;
    let (held, mut gate) = gated(&control);
    let command = enqueue_playlist(&before);
    let pending = tokio::spawn(async move { ask(&held, command).await });
    gate.wait().await;
    let paused = timeout(Duration::from_millis(500), ask(&control, Command::Pause))
        .await
        .expect("pause must not wait for playlist resolution");
    assert!(matches!(paused, Reply::Ok { .. }), "{paused:?}");
    assert!(!pending.is_finished());
    gate.release.send(()).unwrap();
    let Reply::Ok {
        result: Response::Applied {
            snapshot: applied,
            entry_id,
        },
    } = pending.await.unwrap()
    else {
        panic!("the playlist was not enqueued");
    };
    assert_eq!(applied.queue_length, 3);
    assert!(entry_id.is_some());
    // A library change after resolution is stale at commit.
    let before = snapshot(&control).await;
    let (held, mut gate) = gated(&control);
    let command = enqueue_playlist(&before);
    let pending = tokio::spawn(async move { ask(&held, command).await });
    gate.wait().await;
    wav(&fixture.music("new.wav"));
    ok(&control, Command::Rescan { root_id: root }).await;
    fixture.scanned(4).await;
    gate.release.send(()).unwrap();
    match pending.await.unwrap() {
        Reply::Error { code, .. } => assert_eq!(code, ErrorCode::StaleLibraryRevision),
        reply => panic!("a stale playlist was enqueued: {reply:?}"),
    }
    let after = snapshot(&control).await;
    assert_eq!(
        (after.queue_revision, after.queue_length),
        (before.queue_revision, before.queue_length)
    );
}

fn applied(actor: &mut Actor, command: Command) -> Response {
    match actor.command(command) {
        Ok(response) => response,
        Err(failure) => panic!("command failed: {:#}", failure.error),
    }
}
/// Remove or Clear of the current entry just after output reached its
/// threshold, before the owner's next pump saw it, still records the play.
#[test]
fn a_play_counted_just_before_its_entry_is_deleted_is_recorded() {
    for clear in [false, true] {
        let root = std::env::temp_dir().join(format!("tyranny-count-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        // Two seconds: the play counts at one.
        let source = root.join("song.wav");
        wav_frames(&source, 96_000);
        let mut actor = Actor::open(OwnerConfig {
            database_path: root.join("state.sqlite"),
            output: OutputConfig::Capture {
                path: root.join("capture.f32"),
                clocked: true,
            },
        })
        .unwrap();
        let expected_revision = actor.revision();
        applied(
            &mut actor,
            Command::Append {
                expected_revision,
                path: source.to_str().unwrap().to_owned(),
            },
        );
        applied(&mut actor, Command::Play { entry_id: None });
        // Decode past the threshold, then stop pumping: no pump sees output
        // reach it.
        let deadline = Instant::now() + DEADLINE;
        while actor.player.snapshot().output.buffered_frames < 60_000 {
            assert!(
                Instant::now() < deadline,
                "decoding did not fill the buffer"
            );
            actor.player.pump().unwrap();
        }
        while actor.player.snapshot().output.position_frames < 50_000 {
            assert!(
                Instant::now() < deadline,
                "output did not reach the threshold"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let entry_id = actor.player.snapshot().current_id.unwrap();
        let expected_revision = actor.revision();
        applied(
            &mut actor,
            if clear {
                Command::Clear { expected_revision }
            } else {
                Command::Remove {
                    expected_revision,
                    entry_id,
                }
            },
        );
        actor.player.pump().unwrap();
        for play in actor.player.take_counted_plays() {
            actor.count_play(play);
        }
        assert!(actor.write_plays(), "clear {clear}: the play was dropped");
        let page = actor.store.history_page(0, 10).unwrap();
        assert_eq!(page.total, 1, "clear {clear}");
        assert_eq!(page.entries[0].title, "song");
        actor.library.shutdown().unwrap();
        let _ = fs::remove_dir_all(&root);
    }
}
