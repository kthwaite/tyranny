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
        let mut fixture = Self { root, owner: None };
        fixture.start();
        fixture
    }
    /// Starts an owner on the fixture's database.
    fn start(&mut self) {
        self.owner = Some(
            Owner::start(OwnerConfig {
                database_path: self.database(),
                output: OutputConfig::Capture {
                    path: self.root.join("capture.f32"),
                    clocked: true,
                },
            })
            .unwrap(),
        );
    }
    /// Shuts the owner down, as the daemon does when it quits.
    async fn stop(&mut self) {
        self.owner.take().unwrap().shutdown().await.unwrap();
    }
    fn database(&self) -> PathBuf {
        self.root.join("state.sqlite")
    }
    fn control(&self) -> Controller {
        self.owner.as_ref().unwrap().controller()
    }
    fn music(&self, name: &str) -> PathBuf {
        self.root.join("music").join(name)
    }
    /// Indexes one WAV per name under a new root. Returns the root ID.
    async fn index(&self, names: &[&str]) -> u64 {
        self.index_frames(names, 48).await
    }
    /// Indexes one WAV `frames` long per name under a new root. Returns the
    /// root ID.
    async fn index_frames(&self, names: &[&str], frames: u32) -> u64 {
        for name in names {
            wav_frames(&self.music(&format!("{name}.wav")), frames);
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
        actor.take_plays();
        assert!(actor.write_plays(), "clear {clear}: the play was dropped");
        let page = actor.store.history_page(0, 10).unwrap();
        assert_eq!(page.total, 1, "clear {clear}");
        assert_eq!(page.entries[0].title, "song");
        actor.library.shutdown().unwrap();
        let _ = fs::remove_dir_all(&root);
    }
}

/// A radio play keeps the station's now-playing text, split into artist and
/// title, and the station as its source.
#[test]
fn a_radio_play_is_described_by_its_station_text() {
    let root = std::env::temp_dir().join(format!("tyranny-radio-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let mut actor = Actor::open(OwnerConfig {
        database_path: root.join("state.sqlite"),
        output: OutputConfig::Capture {
            path: root.join("capture.f32"),
            clocked: true,
        },
    })
    .unwrap();
    let expected_revision = actor.revision();
    let Response::Applied {
        entry_id: Some(entry_id),
        ..
    } = applied(
        &mut actor,
        Command::AppendStream {
            expected_revision,
            url: "http://127.0.0.1:9/live".into(),
            title: "NTS".into(),
        },
    )
    else {
        panic!("expected the station's entry");
    };
    let text = RadioText {
        artist: "Burial".into(),
        title: "Archangel".into(),
        raw: "Burial - Archangel".into(),
        junk: false,
    };
    let record = actor
        .describe_play(entry_id, 1_700_000_000, Some(text))
        .unwrap();
    assert_eq!(
        (
            record.entry_id,
            record.track_id,
            record.artist.as_str(),
            record.title.as_str(),
            record.raw_title.as_deref(),
            record.duration_ms,
            record.source,
            record.played_at,
        ),
        (
            entry_id,
            None,
            "Burial",
            "Archangel",
            Some("Burial - Archangel"),
            None,
            Some(QueueSource::stamped(SourceKind::Stream, "NTS")),
            1_700_000_000,
        )
    );
    actor.library.shutdown().unwrap();
    let _ = fs::remove_dir_all(&root);
}

/// The owner exits when every controller is gone. Plays still waiting for a
/// history write are written first, as on shutdown.
#[test]
fn a_disconnected_owner_writes_its_pending_plays() {
    let root = std::env::temp_dir().join(format!("tyranny-exit-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let database_path = root.join("state.sqlite");
    let mut actor = Actor::open(OwnerConfig {
        database_path: database_path.clone(),
        output: OutputConfig::Capture {
            path: root.join("capture.f32"),
            clocked: true,
        },
    })
    .unwrap();
    let record = PlayRecord {
        entry_id: 1,
        track_id: None,
        path: "/music/a.flac".into(),
        title: "a".into(),
        artist: String::new(),
        album: String::new(),
        duration_ms: None,
        source: None,
        played_at: 1_700_000_000,
        raw_title: None,
    };
    actor
        .pending_plays
        .push(HistoryEvent::Open { play: 1, record });
    actor.pending_plays.push(HistoryEvent::Count { play: 1 });
    // An earlier write failed, so the loop leaves the play for its retry.
    actor.retrying_plays = true;
    let (sender, receiver) = mpsc::channel(1);
    drop(sender);
    actor.run(receiver).unwrap();
    let page = Store::open(&database_path)
        .unwrap()
        .history_page(0, 10)
        .unwrap();
    assert_eq!(page.total, 1);
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equalizer_rejects_invalid_values_without_mutating_state_or_storage() {
    use crate::equalizer::EqualizerSettings;

    let fixture = Fixture::new();
    let control = fixture.control();
    let equalizer = EqualizerSettings::from_graphic(
        true,
        -6.0,
        [6.0, 4.0, 2.0, 0.0, -1.0, -2.0, 0.0, 1.0, 2.0, 3.0],
    );
    ok(
        &control,
        Command::SetEqualizer {
            equalizer: equalizer.clone(),
        },
    )
    .await;
    let before = snapshot(&control).await;
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -96.01, 12.01] {
        let mut invalid = equalizer.clone();
        invalid.preamp_db = value;
        match ask(&control, Command::SetEqualizer { equalizer: invalid }).await {
            Reply::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidCommand),
            reply => panic!("accepted invalid preamp {value}: {reply:?}"),
        }
        assert_eq!(snapshot(&control).await, before);
        assert_eq!(
            Store::open(&fixture.root.join("state.sqlite"))
                .unwrap()
                .load()
                .unwrap()
                .settings,
            before.settings
        );
    }
    for band in 0..10 {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -24.01, 24.01] {
            let mut invalid = equalizer.clone();
            invalid.bands[band].gain_db = value;
            match ask(&control, Command::SetEqualizer { equalizer: invalid }).await {
                Reply::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidCommand),
                reply => panic!("accepted invalid band {band} = {value}: {reply:?}"),
            }
            assert_eq!(snapshot(&control).await, before);
        }
    }
    for field in 0..5 {
        let mut invalid = equalizer.clone();
        invalid.enabled = false;
        invalid.bands[0].enabled = false;
        match field {
            0 => invalid.bands[0].frequency_hz = f64::NAN,
            1 => invalid.bands[0].q = 18.01,
            2 => invalid.bands[0].shelf_slope = 0.0,
            3 => invalid.bands[0].slope_db = 18,
            _ => invalid.bands.resize(17, invalid.bands[0].clone()),
        }
        assert!(matches!(
            ask(&control, Command::SetEqualizer { equalizer: invalid }).await,
            Reply::Error {
                code: ErrorCode::InvalidCommand,
                ..
            }
        ));
        assert_eq!(snapshot(&control).await, before);
    }
    assert_eq!(
        Store::open(&fixture.root.join("state.sqlite"))
            .unwrap()
            .load()
            .unwrap()
            .settings,
        before.settings
    );
}

#[test]
fn equalizer_persistence_failure_does_not_publish_or_change_settings() {
    let root = std::env::temp_dir().join(format!("tyranny-equalizer-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let database_path = root.join("state.sqlite");
    let mut actor = Actor::open(OwnerConfig {
        database_path: database_path.clone(),
        output: OutputConfig::Capture {
            path: root.join("capture.f32"),
            clocked: true,
        },
    })
    .unwrap();
    assert!(
        actor
            .command(Command::SaveEqualizerPreset {
                name: "Existing".into(),
                equalizer: Default::default(),
            })
            .is_ok()
    );
    let before = actor.last.clone();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_settings BEFORE UPDATE OF settings ON control
         BEGIN SELECT RAISE(ABORT, 'test settings write failure'); END;",
        )
        .unwrap();
    let result = actor.command(Command::SetEqualizer {
        equalizer: crate::equalizer::EqualizerSettings::from_graphic(true, -6.0, [3.0; 10]),
    });
    assert!(result.is_err());
    assert_eq!(actor.last, before);
    assert_eq!(actor.settings, before.settings);
    assert_eq!(actor.store.load().unwrap().settings, before.settings);
    for command in [
        Command::SaveEqualizerPreset {
            name: "Test".into(),
            equalizer: Default::default(),
        },
        Command::RenameEqualizerPreset {
            name: "Existing".into(),
            new_name: "Renamed".into(),
        },
        Command::RemoveEqualizerPreset {
            name: "Existing".into(),
        },
    ] {
        assert!(actor.command(command).is_err());
        assert_eq!(actor.last, before);
        assert_eq!(actor.settings, before.settings);
        assert_eq!(actor.store.load().unwrap().settings, before.settings);
    }
    actor.library.shutdown().unwrap();
    drop(actor.player);
    drop(actor.store);
    drop(connection);
    let _ = fs::remove_dir_all(&root);
}

/// One `play_history` row, as the owner wrote it.
#[derive(Debug)]
struct PlayRow {
    id: i64,
    entry_id: u64,
    played_at: i64,
    counted: bool,
    listened_ms: Option<u64>,
    end_reason: Option<String>,
    ended_at: Option<i64>,
}
impl Fixture {
    /// Every play row, oldest first.
    fn plays(&self) -> Vec<PlayRow> {
        let connection = rusqlite::Connection::open(self.database()).unwrap();
        let mut query = connection
            .prepare(
                "SELECT id,entry_id,played_at,counted,listened_ms,end_reason,ended_at
                 FROM play_history ORDER BY id",
            )
            .unwrap();
        query
            .query_map([], |row| {
                Ok(PlayRow {
                    id: row.get(0)?,
                    entry_id: row.get(1)?,
                    played_at: row.get(2)?,
                    counted: row.get(3)?,
                    listened_ms: row.get(4)?,
                    end_reason: row.get(5)?,
                    ended_at: row.get(6)?,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    /// Waits until the play rows satisfy `done`, and returns them.
    async fn until_plays(&self, done: impl Fn(&[PlayRow]) -> bool) -> Vec<PlayRow> {
        timeout(DEADLINE, async {
            loop {
                let plays = self.plays();
                if done(&plays) {
                    return plays;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("play rows deadline exceeded: {:?}", self.plays()))
    }
    /// Waits until a snapshot satisfies `done`, and returns it.
    async fn until(&self, done: impl Fn(&ControlState) -> bool) -> ControlState {
        timeout(DEADLINE, async {
            loop {
                let state = snapshot(&self.control()).await;
                if done(&state) {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("snapshot deadline exceeded")
    }
    /// Queues every library track, in library order. Returns the first entry.
    async fn queue_library(&self) -> u64 {
        let state = snapshot(&self.control()).await;
        let Response::Applied {
            entry_id: Some(first),
            ..
        } = ok(&self.control(), enqueue(&state)).await
        else {
            panic!("the library was not queued");
        };
        first
    }
    /// Plays entry `entry_id` until output is `frames` into it.
    async fn play_to(&self, entry_id: u64, frames: u64) {
        ok(
            &self.control(),
            Command::Play {
                entry_id: Some(entry_id),
            },
        )
        .await;
        self.until(|state| state.current_id == Some(entry_id) && state.position_frames >= frames)
            .await;
    }
}
/// A closed row of entry `entry_id`.
fn closed(plays: &[PlayRow], entry_id: u64) -> Option<&PlayRow> {
    plays
        .iter()
        .find(|play| play.entry_id == entry_id && play.end_reason.is_some())
}

/// Ten seconds: a play counts at five.
const TEN_SECONDS: u32 = 480_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skipping_mid_track_records_an_uncounted_skipped_play() {
    let fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
    let first = fixture.queue_library().await;
    let before = snapshot(&fixture.control()).await;
    fixture.play_to(first, 48_000).await;
    ok(&fixture.control(), Command::Next).await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    let skipped = closed(&plays, first).unwrap();
    assert_eq!(
        (skipped.counted, skipped.end_reason.as_deref()),
        (false, Some("skipped")),
        "{plays:?}"
    );
    let listened = skipped.listened_ms.unwrap();
    assert!((1_000..3_000).contains(&listened), "{plays:?}");
    assert!(skipped.ended_at.unwrap() >= skipped.played_at);
    let after = snapshot(&fixture.control()).await;
    assert_eq!(after.history_revision, before.history_revision);
    assert!(after.stats_revision > before.stats_revision, "{after:?}");
    let history = Store::open(&fixture.database())
        .unwrap()
        .history_page(0, 10)
        .unwrap();
    assert_eq!(history.total, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reselecting_the_current_entry_continues_its_play() {
    let fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
    let first = fixture.queue_library().await;
    fixture.play_to(first, 48_000).await;
    fixture.play_to(first, 72_000).await;
    ok(&fixture.control(), Command::Next).await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    let firsts: Vec<_> = plays.iter().filter(|play| play.entry_id == first).collect();
    assert_eq!(firsts.len(), 1, "{plays:?}");
    assert_eq!(
        firsts[0].end_reason.as_deref(),
        Some("skipped"),
        "{plays:?}"
    );
    assert!(firsts[0].listened_ms.unwrap() >= 2_400, "{plays:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_the_current_entry_closes_its_play_as_skipped() {
    let fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
    let first = fixture.queue_library().await;
    fixture.play_to(first, 48_000).await;
    let state = snapshot(&fixture.control()).await;
    ok(
        &fixture.control(),
        Command::Remove {
            expected_revision: state.queue_revision,
            entry_id: first,
        },
    )
    .await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    let removed = closed(&plays, first).unwrap();
    assert_eq!(
        (removed.counted, removed.end_reason.as_deref()),
        (false, Some("skipped")),
        "{plays:?}"
    );
    assert!(removed.listened_ms.unwrap() >= 1_000, "{plays:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_the_queue_closes_its_play_as_stopped() {
    let fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
    let first = fixture.queue_library().await;
    fixture.play_to(first, 48_000).await;
    let state = snapshot(&fixture.control()).await;
    ok(
        &fixture.control(),
        Command::Clear {
            expected_revision: state.queue_revision,
        },
    )
    .await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    assert_eq!(plays.len(), 1, "{plays:?}");
    assert_eq!(
        (plays[0].counted, plays[0].end_reason.as_deref()),
        (false, Some("stopped")),
        "{plays:?}"
    );
    assert!(plays[0].listened_ms.unwrap() >= 1_000, "{plays:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_closes_the_play_as_stopped() {
    let fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
    let first = fixture.queue_library().await;
    fixture.play_to(first, 48_000).await;
    ok(&fixture.control(), Command::Stop).await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    assert_eq!(plays.len(), 1, "{plays:?}");
    assert_eq!(
        (plays[0].counted, plays[0].end_reason.as_deref()),
        (false, Some("stopped")),
        "{plays:?}"
    );
    assert!(plays[0].listened_ms.unwrap() >= 1_000, "{plays:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playing_an_enqueued_selection_mid_track_closes_the_play_as_skipped() {
    let fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
    let first = fixture.queue_library().await;
    fixture.play_to(first, 48_000).await;
    let state = snapshot(&fixture.control()).await;
    let Command::EnqueueTracks {
        selection,
        expected_revision,
        source,
        ..
    } = enqueue(&state)
    else {
        unreachable!()
    };
    ok(
        &fixture.control(),
        Command::EnqueueTracks {
            selection,
            expected_revision,
            placement: Placement::ReplaceUpcoming,
            play: true,
            source,
        },
    )
    .await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    let replaced = closed(&plays, first).unwrap();
    assert_eq!(
        (replaced.counted, replaced.end_reason.as_deref()),
        (false, Some("skipped")),
        "{plays:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playing_a_playlist_mid_track_closes_the_play_as_skipped() {
    let fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
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
    let first = fixture.queue_library().await;
    fixture.play_to(first, 48_000).await;
    let state = snapshot(&control).await;
    ok(
        &control,
        Command::EnqueuePlaylist {
            playlist_id: playlist.id,
            playlist_revision: playlist.revision,
            library_revision: state.library.revision,
            expected_revision: state.queue_revision,
            play: true,
        },
    )
    .await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    let replaced = closed(&plays, first).unwrap();
    assert_eq!(
        (replaced.counted, replaced.end_reason.as_deref()),
        (false, Some("skipped")),
        "{plays:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_continues_the_restored_play() {
    let mut fixture = Fixture::new();
    // Four seconds: the play counts at two.
    fixture.index_frames(&["a"], 192_000).await;
    let first = fixture.queue_library().await;
    fixture.play_to(first, 120_000).await;
    fixture.until(|state| state.history_revision == 1).await;
    fixture.stop().await;
    // Shutdown saves the listened time, and leaves the play open.
    let plays = fixture.plays();
    assert_eq!(plays.len(), 1, "{plays:?}");
    let (row, before) = (plays[0].id, plays[0].listened_ms.unwrap());
    assert!(
        plays[0].counted && plays[0].end_reason.is_none(),
        "{plays:?}"
    );
    assert!(before >= 2_500, "{plays:?}");

    fixture.start();
    ok(&fixture.control(), Command::Play { entry_id: None }).await;
    fixture
        .until(|state| state.transport == Transport::Ended)
        .await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    assert_eq!(plays.len(), 1, "{plays:?}");
    assert_eq!(
        (
            plays[0].id,
            plays[0].counted,
            plays[0].end_reason.as_deref()
        ),
        (row, true, Some("finished")),
        "{plays:?}"
    );
    assert!(plays[0].listened_ms.unwrap() > before, "{plays:?}");
    assert_eq!(snapshot(&fixture.control()).await.history_revision, 1);
    let play_count: u64 = rusqlite::Connection::open(fixture.database())
        .unwrap()
        .query_row("SELECT play_count FROM track_stats", [], |row| row.get(0))
        .unwrap();
    assert_eq!(play_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_closes_other_open_rows_as_stopped() {
    let mut fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
    let first = fixture.queue_library().await;
    fixture.play_to(first, 48_000).await;
    fixture.stop().await;
    // A play of the other entry left open, as by a crash.
    let mut store = Store::open(&fixture.database()).unwrap();
    let other = store
        .open_play(&PlayRecord {
            entry_id: first + 1,
            track_id: None,
            path: "/music/b.wav".into(),
            title: "b".into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: None,
            source: None,
            played_at: 1_700_000_000,
            raw_title: None,
        })
        .unwrap();
    store.checkpoint_play(other, 3_000).unwrap();
    drop(store);

    fixture.start();
    let plays = fixture
        .until_plays(|plays| closed(plays, first + 1).is_some())
        .await;
    let stale = closed(&plays, first + 1).unwrap();
    assert_eq!(
        (stale.id, stale.end_reason.as_deref(), stale.ended_at),
        (other, Some("stopped"), Some(1_700_000_003)),
        "{plays:?}"
    );
    // The restored selection's play stays open until the selection changes.
    let restored = plays.iter().find(|play| play.entry_id == first).unwrap();
    assert_eq!(restored.end_reason, None, "{plays:?}");
    let restored = restored.id;
    ok(
        &fixture.control(),
        Command::Play {
            entry_id: Some(first + 1),
        },
    )
    .await;
    let plays = fixture
        .until_plays(|plays| closed(plays, first).is_some())
        .await;
    let changed = closed(&plays, first).unwrap();
    assert_eq!(
        (changed.id, changed.end_reason.as_deref()),
        (restored, Some("stopped")),
        "{plays:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_write_failures_queue_events_without_stalling_playback() {
    let fixture = Fixture::new();
    fixture.index_frames(&["a", "b"], TEN_SECONDS).await;
    let first = fixture.queue_library().await;
    // Every history insert fails until the trigger is dropped.
    let database = rusqlite::Connection::open(fixture.database()).unwrap();
    database
        .execute_batch(
            "CREATE TRIGGER injected BEFORE INSERT ON play_history
             BEGIN SELECT RAISE(ABORT,'injected failure'); END;",
        )
        .unwrap();
    let before = snapshot(&fixture.control()).await;
    fixture.play_to(first, 48_000).await;
    let next = timeout(
        Duration::from_millis(500),
        ask(&fixture.control(), Command::Next),
    )
    .await
    .expect("Next must not wait for history writes");
    assert!(matches!(next, Reply::Ok { .. }), "{next:?}");
    let state = fixture
        .until(|state| state.current_id == Some(first + 1) && state.position_frames >= 24_000)
        .await;
    assert_eq!(
        (state.transport, state.error, state.stats_revision),
        (Transport::Playing, None, before.stats_revision)
    );
    assert!(fixture.plays().is_empty());
    database.execute_batch("DROP TRIGGER injected;").unwrap();
    // The queued events are written in order once writes succeed.
    let plays = fixture.until_plays(|plays| plays.len() == 2).await;
    let skipped = closed(&plays, first).unwrap();
    assert_eq!(
        (skipped.id, skipped.counted, skipped.end_reason.as_deref()),
        (plays[0].id, false, Some("skipped")),
        "{plays:?}"
    );
    assert_eq!(
        (plays[1].entry_id, plays[1].end_reason.as_deref()),
        (first + 1, None),
        "{plays:?}"
    );
}

#[tokio::test]
async fn stats_replies_do_not_wait_for_the_owner() {
    let fixture = Fixture::new();
    let mut control = fixture.control();
    let revision = snapshot(&control).await.queue_revision;
    // Nothing reads this channel, so any owner round trip would hang.
    let (stalled, _unserviced) = mpsc::channel(64);
    control.sender = stalled;
    let ask = |id: u64, command: Command| {
        let control = control.clone();
        async move {
            timeout(
                Duration::from_secs(2),
                control.request(Request { id, command }),
            )
            .await
            .expect("stats waited for the owner")
            .unwrap()
        }
    };
    let period = |anchor: &str| crate::stats_types::PeriodRequest {
        kind: crate::stats_types::PeriodKind::Day,
        anchor: anchor.into(),
    };
    match ask(
        1,
        Command::Stats {
            period: period("2026-10-21"),
        },
    )
    .await
    {
        Message::Reply {
            reply: Reply::Ok {
                result: Response::Stats { .. },
            },
            ..
        } => {}
        other => panic!("unexpected reply: {other:?}"),
    }
    match ask(
        2,
        Command::Stats {
            period: period("2026-02-30"),
        },
    )
    .await
    {
        Message::Reply {
            reply:
                Reply::Error {
                    code: ErrorCode::InvalidCommand,
                    queue_revision,
                    ..
                },
            ..
        } => assert_eq!(queue_revision, revision),
        other => panic!("unexpected reply: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equalizer_presets_are_durable_sorted_and_do_not_recall_on_edit() {
    let fixture = Fixture::new();
    let control = fixture.control();
    let active = snapshot(&control).await.settings.equalizer;
    let curve = crate::equalizer::EqualizerSettings::from_graphic(true, -6.0, [3.0; 10]);
    for name in ["Zulu", "Alpha"] {
        ok(
            &control,
            Command::SaveEqualizerPreset {
                name: name.into(),
                equalizer: curve.clone(),
            },
        )
        .await;
    }
    ok(
        &control,
        Command::SaveEqualizerPreset {
            name: "Zulu".into(),
            equalizer: active.clone(),
        },
    )
    .await;
    let settings = snapshot(&control).await.settings;
    assert_eq!(settings.equalizer, active);
    assert_eq!(
        settings
            .equalizer_presets
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "Zulu"]
    );
    assert_eq!(settings.equalizer_presets[1].equalizer, active);
    for command in [
        Command::RenameEqualizerPreset {
            name: "Alpha".into(),
            new_name: "Zulu".into(),
        },
        Command::RenameEqualizerPreset {
            name: "Missing".into(),
            new_name: "New".into(),
        },
        Command::RemoveEqualizerPreset {
            name: "Missing".into(),
        },
    ] {
        assert!(matches!(
            ask(&control, command).await,
            Reply::Error {
                code: ErrorCode::InvalidCommand,
                ..
            }
        ));
        assert_eq!(snapshot(&control).await.settings, settings);
    }
    for name in [
        "".to_owned(),
        " spaced".into(),
        "spaced ".into(),
        "bad\nname".into(),
        "é".repeat(81),
    ] {
        assert!(matches!(
            ask(
                &control,
                Command::SaveEqualizerPreset {
                    name,
                    equalizer: curve.clone()
                }
            )
            .await,
            Reply::Error {
                code: ErrorCode::InvalidCommand,
                ..
            }
        ));
    }
    let mut invalid = curve.clone();
    invalid.bands[0].frequency_hz = 0.0;
    assert!(matches!(
        ask(
            &control,
            Command::SaveEqualizerPreset {
                name: "Invalid".into(),
                equalizer: invalid
            }
        )
        .await,
        Reply::Error {
            code: ErrorCode::InvalidCommand,
            ..
        }
    ));
    ok(
        &control,
        Command::RenameEqualizerPreset {
            name: "Zulu".into(),
            new_name: "Beta".into(),
        },
    )
    .await;
    ok(
        &control,
        Command::RemoveEqualizerPreset {
            name: "Alpha".into(),
        },
    )
    .await;
    let settings = snapshot(&control).await.settings;
    assert_eq!(settings.equalizer, active);
    assert_eq!(settings.equalizer_presets.len(), 1);
    assert_eq!(settings.equalizer_presets[0].name, "Beta");
    assert_eq!(
        Store::open(&fixture.root.join("state.sqlite"))
            .unwrap()
            .load()
            .unwrap()
            .settings,
        settings
    );
    for index in 0..63 {
        ok(
            &control,
            Command::SaveEqualizerPreset {
                name: format!("Preset {index:02}"),
                equalizer: active.clone(),
            },
        )
        .await;
    }
    assert!(matches!(
        ask(
            &control,
            Command::SaveEqualizerPreset {
                name: "Overflow".into(),
                equalizer: active.clone()
            }
        )
        .await,
        Reply::Error {
            code: ErrorCode::InvalidCommand,
            ..
        }
    ));
    ok(
        &control,
        Command::SaveEqualizerPreset {
            name: "Beta".into(),
            equalizer: curve,
        },
    )
    .await;
    assert_eq!(
        snapshot(&control).await.settings.equalizer_presets.len(),
        64
    );
}
