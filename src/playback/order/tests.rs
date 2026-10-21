use super::*;
use crate::{
    decoder::GainMode,
    output::OutputConfig,
    play_order::{RepeatScope, RunInfo},
    playback::{RadioText, Start, radio},
    protocol::StreamSpec,
    store::{EndReason, LoadedEntry},
};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

#[test]
fn repeat_all_trim_keeps_the_current_context() -> anyhow::Result<()> {
    let directory = std::env::temp_dir().join(format!("tyranny-trim-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    // 103 loose songs, then a three-entry album run; every entry is played.
    let loaded = |id: u64, run_id: u64, scope: RepeatScope| LoadedEntry {
        id,
        path: PathBuf::from(format!("/music/{id}.flac")),
        stream: None,
        run: Some(RunInfo { run_id, scope }),
        played_seq: Some(id),
    };
    let entries = (1..=103)
        .map(|id| loaded(id, id, RepeatScope::Queue))
        .chain((104..=106).map(|id| loaded(id, 104, RepeatScope::Run)))
        .collect();
    let mut player = Player::from_loaded(
        entries,
        1,
        107,
        OutputConfig::Capture {
            path: directory.join("capture.f32"),
            clocked: false,
        },
        GainMode::Off,
    )?;
    let newest_first: Vec<u64> = (1..=106).rev().collect();
    // Off keeps the newest 100 besides the current entry.
    assert_eq!(player.trimmed(&newest_first, 106), [5, 4, 3, 2, 1]);
    player.set_repeat(RepeatMode::All);
    // The album run is kept whole; 100 of the other played entries stay.
    assert_eq!(player.trimmed(&newest_first, 106), [3, 2, 1]);
    // A song's context is the whole queue: nothing is trimmed.
    assert!(player.trimmed(&newest_first, 50).is_empty());
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn previous_under_repeat_all_wraps_within_the_context() -> anyhow::Result<()> {
    let directory = std::env::temp_dir().join(format!("tyranny-previous-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    // A loose song, a three-entry album run, then another loose song.
    let loaded = |id: u64, run_id: u64, scope: RepeatScope| LoadedEntry {
        id,
        path: PathBuf::from(format!("/music/{id}.flac")),
        stream: None,
        run: Some(RunInfo { run_id, scope }),
        played_seq: None,
    };
    let entries = vec![
        loaded(1, 1, RepeatScope::Queue),
        loaded(2, 2, RepeatScope::Run),
        loaded(3, 2, RepeatScope::Run),
        loaded(4, 2, RepeatScope::Run),
        loaded(5, 5, RepeatScope::Queue),
    ];
    let mut player = Player::from_loaded(
        entries,
        1,
        6,
        OutputConfig::Capture {
            path: directory.join("capture.f32"),
            clocked: false,
        },
        GainMode::Off,
    )?;
    // Off steps back one entry, out of the run, and stops at the start.
    assert_eq!(player.previous_index(1), Some(0));
    assert_eq!(player.previous_index(0), None);
    player.set_repeat(RepeatMode::All);
    // From the run's first entry to its last; inside it, one back.
    assert_eq!(player.previous_index(1), Some(3));
    assert_eq!(player.previous_index(2), Some(1));
    // A loose song's context is the whole queue.
    assert_eq!(player.previous_index(0), Some(4));
    // Shuffled, history from before the run does not leave it either.
    player.set_shuffle(Some(Box::new(Last)));
    player.order.advanced(1);
    assert_eq!(player.previous_index(1), Some(3));
    player.order.advanced(3);
    player.order.advanced(2);
    assert_eq!(player.previous_index(1), Some(2));
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

/// `frames` frames of stereo 16-bit silence.
fn silence(directory: &std::path::Path, frames: u32) -> anyhow::Result<PathBuf> {
    let source = directory.join("source.wav");
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    for field in [16_u32, 0x0002_0001, 48_000, 192_000, 0x0010_0004] {
        bytes.extend_from_slice(&field.to_le_bytes());
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 4).to_le_bytes());
    bytes.resize(bytes.len() + frames as usize * 4, 0);
    fs::write(&source, bytes)?;
    Ok(source)
}

/// Always picks the last candidate.
struct Last;
impl ShuffleStrategy for Last {
    fn choose(&mut self, candidates: &[u64]) -> usize {
        candidates.len() - 1
    }
}

#[test]
fn shuffled_start_plays_a_pick_within_the_new_run() -> anyhow::Result<()> {
    let directory = std::env::temp_dir().join(format!("tyranny-start-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    let source = silence(&directory, 100)?;
    let mut player = Player::new(
        vec![source.clone()],
        OutputConfig::Capture {
            path: directory.join("capture.f32"),
            clocked: false,
        },
        GainMode::Off,
    )?;
    player.set_shuffle(Some(Box::new(Last)));
    // Runs 2..=4 then 5..=7: without a shuffled start the run's first
    // entry plays; with one, the strategy's pick does.
    for (start_from, start) in [(Start::First, 2), (Start::Pick, 7)] {
        let revision = player.snapshot().queue_revision;
        let prepared = player.prepare_replace_upcoming(
            vec![source.clone(); 3].into_iter(),
            crate::protocol::Placement::ReplaceUpcoming,
            revision,
            Some(start_from),
            RepeatScope::Run,
            &[],
        )?;
        assert_eq!(prepared.start_id(), start);
        player.commit_replace_upcoming(prepared);
        assert_eq!(player.snapshot().current_id, Some(start));
    }
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn a_selection_never_inherits_a_planned_fresh_pass() -> anyhow::Result<()> {
    let directory = std::env::temp_dir().join(format!("tyranny-fresh-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    let source = silence(&directory, 100)?;
    let mut player = Player::new(
        vec![source.clone(), source],
        OutputConfig::Capture {
            path: directory.join("capture.f32"),
            clocked: false,
        },
        GainMode::Off,
    )?;
    player.set_repeat(RepeatMode::All);
    // The cursor's next pass was planned to start a fresh Repeat All pass.
    player.opening = Some(vec![1, 2]);
    let prepared = player.prepare_selection(2, 0, GainMode::Off)?;
    player.commit_selection(prepared);
    assert!(player.passes.iter().all(|pass| pass.fresh.is_none()));
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

/// A clocked capture player over `sources`.
fn clocked(directory: &std::path::Path, sources: Vec<PathBuf>) -> anyhow::Result<Player> {
    Player::new(
        sources,
        OutputConfig::Capture {
            path: directory.join("capture.f32"),
            clocked: true,
        },
        GainMode::Off,
    )
}

/// Pumps until `done`, gathering the play events.
fn pump_until(
    player: &mut Player,
    events: &mut Vec<PlayEvent>,
    mut done: impl FnMut(&Player, &[PlayEvent]) -> bool,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(player, events) {
        anyhow::ensure!(Instant::now() < deadline, "timed out: {events:?}");
        player.pump()?;
        events.extend(player.take_play_events());
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn position(player: &Player) -> u64 {
    player.snapshot().position_frames
}

fn listened(player: &Player) -> u64 {
    player.open_play().map_or(0, |(_, listened)| listened)
}

fn near(actual: u64, expected: u64, tolerance: u64) -> bool {
    actual.abs_diff(expected) <= tolerance
}

fn opened(events: &[PlayEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayEvent::Opened { play, .. } => Some(*play),
            _ => None,
        })
        .collect()
}

fn counted(events: &[PlayEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayEvent::Counted { play } => Some(*play),
            _ => None,
        })
        .collect()
}

fn scratch(name: &str) -> anyhow::Result<PathBuf> {
    let directory = std::env::temp_dir().join(format!("tyranny-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    Ok(directory)
}

#[test]
fn a_track_heard_to_its_end_opens_counts_and_finishes() -> anyhow::Result<()> {
    let directory = scratch("finish")?;
    let mut player = clocked(&directory, vec![silence(&directory, 24_000)?])?;
    let before = unix_now();
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, _| player.completed())?;
    player.pump()?;
    events.extend(player.take_play_events());
    let [
        PlayEvent::Opened {
            play,
            entry_id: 1,
            started_at,
            radio: None,
        },
        PlayEvent::Counted { play: counted },
        PlayEvent::Closed {
            play: closed,
            listened_ms,
            reason: EndReason::Finished,
            ended_at,
        },
    ] = events[..]
    else {
        panic!("{events:?}");
    };
    assert_eq!((counted, closed), (play, play));
    assert!(near(listened_ms, 500, 50), "{listened_ms}");
    assert!(before <= started_at && started_at <= ended_at);
    assert_eq!(player.open_play(), None);
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn end_play_skipped_before_the_threshold_closes_uncounted() -> anyhow::Result<()> {
    let directory = scratch("skip")?;
    let mut player = clocked(&directory, vec![silence(&directory, 480_000)?])?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, _| {
        position(player) >= 48_000
    })?;
    player.end_play(EndReason::Skipped);
    events.extend(player.take_play_events());
    let [
        PlayEvent::Opened { play, .. },
        PlayEvent::Closed {
            play: closed,
            listened_ms,
            reason: EndReason::Skipped,
            ..
        },
    ] = events[..]
    else {
        panic!("{events:?}");
    };
    assert_eq!(closed, play);
    assert!(near(listened_ms, 1_000, 50), "{listened_ms}");
    assert_eq!(player.open_play(), None);
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn pause_is_not_listened_time() -> anyhow::Result<()> {
    let directory = scratch("pause")?;
    let mut player = clocked(&directory, vec![silence(&directory, 480_000)?])?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, _| {
        position(player) >= 48_000
    })?;
    player.pause();
    let paused = Instant::now();
    pump_until(&mut player, &mut events, |_, _| {
        paused.elapsed() >= Duration::from_secs(2)
    })?;
    player.play();
    pump_until(&mut player, &mut events, |player, _| {
        position(player) >= 96_000
    })?;
    assert!(near(listened(&player), 2_000, 100), "{}", listened(&player));
    assert_eq!(opened(&events).len(), 1, "{events:?}");
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn seeking_continues_the_same_play_without_double_counting() -> anyhow::Result<()> {
    let directory = scratch("seek")?;
    let mut player = clocked(&directory, vec![silence(&directory, 480_000)?])?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, _| {
        position(player) >= 24_000
    })?;
    // Forward 5 s, past the 5 s threshold: the play counts.
    player.seek(264_000)?;
    pump_until(&mut player, &mut events, |player, _| {
        position(player) >= 288_000
    })?;
    // Back 3 s, then across the threshold again.
    player.seek(144_000)?;
    pump_until(&mut player, &mut events, |player, _| {
        position(player) >= 264_000
    })?;
    let opened = opened(&events);
    assert_eq!(opened.len(), 1, "{events:?}");
    assert_eq!(counted(&events), opened, "{events:?}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayEvent::Closed { .. })),
        "{events:?}"
    );
    let (play, listened) = player.open_play().expect("an open play");
    assert_eq!(play, opened[0]);
    // About 3.5 s rendered, though the position is 5.5 s.
    let rendered = player.snapshot().output.rendered_frames * 1_000 / 48_000;
    assert!(near(listened, rendered, 50), "{listened} {rendered}");
    assert!(near(listened, 3_500, 300), "{listened}");
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn restored_play_keeps_its_key_and_counted_state() -> anyhow::Result<()> {
    let directory = scratch("restore")?;
    let mut player = clocked(&directory, vec![silence(&directory, 480_000)?])?;
    // Restored past the 5 s threshold.
    let prepared = player.prepare_selection(1, 288_000, GainMode::Off)?;
    player.commit_selection(prepared);
    player.restored_selection();
    let play = player.restore_play(1, true, 90_000, None);
    assert_eq!(player.open_play(), Some((play, 90_000)));
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, _| {
        position(player) >= 336_000
    })?;
    assert!(events.is_empty(), "{events:?}");
    let (open, listened) = player.open_play().expect("an open play");
    assert_eq!(open, play);
    assert!(near(listened, 91_000, 50), "{listened}");
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn restored_play_never_heard_closes_as_stopped() -> anyhow::Result<()> {
    let directory = scratch("unheard")?;
    let mut player = clocked(&directory, vec![silence(&directory, 480_000)?])?;
    let prepared = player.prepare_selection(1, 0, GainMode::Off)?;
    player.commit_selection(prepared);
    player.restored_selection();
    let play = player.restore_play(1, false, 4_000, None);
    player.end_play(EndReason::Skipped);
    let events = player.take_play_events();
    let [
        PlayEvent::Closed {
            play: closed,
            listened_ms: 4_000,
            reason: EndReason::Stopped,
            ..
        },
    ] = events[..]
    else {
        panic!("{events:?}");
    };
    assert_eq!(closed, play);
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn a_restored_counted_pass_without_a_row_opens_uncounted_and_stops() -> anyhow::Result<()> {
    let directory = scratch("rowless")?;
    let mut player = clocked(&directory, vec![silence(&directory, 480_000)?])?;
    let prepared = player.prepare_selection(1, 288_000, GainMode::Off)?;
    player.commit_selection(prepared);
    player.restored_selection();
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, _| {
        position(player) >= 312_000
    })?;
    player.end_play(EndReason::Skipped);
    events.extend(player.take_play_events());
    let [
        PlayEvent::Opened { play, .. },
        PlayEvent::Closed {
            play: closed,
            reason: EndReason::Stopped,
            ..
        },
    ] = events[..]
    else {
        panic!("{events:?}");
    };
    assert_eq!(closed, play);
    drop(player);
    fs::remove_dir_all(&directory)?;
    Ok(())
}

#[test]
fn radio_play_counts_after_sixty_seconds() {
    let frames = |ms: u64| ms * 48;
    let song = radio::parse("Burial - Archangel", "NTS");
    let mut play = Play::new(1, Some(&song));
    assert!(!play.listen(frames(59_900)));
    assert!(play.listen(frames(100)));
    assert!(!play.listen(frames(60_000)), "a radio play counts once");
    let mut junk = Play::new(2, Some(&radio::parse("NTS", "NTS")));
    assert!(!junk.listen(frames(120_000)));
    let mut file = Play::new(3, None);
    assert!(
        !file.listen(frames(120_000)),
        "file plays count by position"
    );
}

/// A local ICY station playing `live.mp3` in a loop at about 25 times real
/// time, so a minute of radio plays in seconds. Like a real station, a new
/// connection announces its text only after a while: its first
/// `QUIET_BLOCKS` metadata blocks are empty, about 8 s of audio.
struct Station {
    address: std::net::SocketAddr,
    title: Arc<std::sync::Mutex<String>>,
    /// Connections accepted so far.
    connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    /// Stream workers are few and process-wide: one station test at a time.
    _turn: std::sync::MutexGuard<'static, ()>,
}

static STATIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());
const QUIET_BLOCKS: usize = 64;
const AUDIO: &[u8] = include_bytes!("../../../tests/fixtures/streams/live.mp3");

impl Station {
    fn start(title: &str) -> anyhow::Result<Self> {
        Self::open(title, None)
    }

    /// The first connection ends after `loops` loops of the file, a whole
    /// number of blocks, so the player reconnects.
    fn cutting(title: &str, loops: usize) -> anyhow::Result<Self> {
        Self::open(title, Some(loops))
    }

    fn open(title: &str, cut: Option<usize>) -> anyhow::Result<Self> {
        let turn = STATIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let title = Arc::new(std::sync::Mutex::new(title.to_owned()));
        let connections = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (current, count, halt) = (
            Arc::clone(&title),
            Arc::clone(&connections),
            Arc::clone(&stop),
        );
        let thread = thread::spawn(move || {
            let mut clients = Vec::new();
            while !halt.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let first = count.fetch_add(1, Ordering::AcqRel) == 0;
                        let loops = cut.filter(|_| first);
                        let (current, halt) = (Arc::clone(&current), Arc::clone(&halt));
                        clients.push(thread::spawn(move || {
                            let _ = Self::serve(socket, &current, loops, &halt);
                        }));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(5)),
                }
            }
            for client in clients {
                let _ = client.join();
            }
        });
        Ok(Self {
            address,
            title,
            connections,
            stop,
            thread: Some(thread),
            _turn: turn,
        })
    }

    /// The metadata block sent after audio block `sent`.
    fn metadata(sent: usize, title: &str) -> Vec<u8> {
        if sent < QUIET_BLOCKS {
            return vec![0];
        }
        let text = format!("StreamTitle='{title}';");
        let blocks = text.len().div_ceil(16);
        let mut metadata = vec![0; blocks * 16 + 1];
        metadata[0] = blocks as u8;
        metadata[1..=text.len()].copy_from_slice(text.as_bytes());
        metadata
    }

    fn serve(
        mut socket: TcpStream,
        title: &std::sync::Mutex<String>,
        loops: Option<usize>,
        stop: &AtomicBool,
    ) -> std::io::Result<()> {
        socket.set_nonblocking(false)?;
        socket.set_write_timeout(Some(Duration::from_millis(500)))?;
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte)?;
            request.push(byte[0]);
        }
        // Chunked, so a cut connection ends with a last chunk, not an error.
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nicy-metaint: 1024\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")?;
        let mut send = |bytes: &[u8]| {
            socket.write_all(format!("{:x}\r\n", bytes.len()).as_bytes())?;
            socket.write_all(bytes)?;
            socket.write_all(b"\r\n")
        };
        // A cut connection sends whole files, which end on frame boundaries.
        let blocks = loops.map_or(usize::MAX, |loops| loops * AUDIO.len() / 1024);
        let mut position = 0;
        for sent in 0..blocks {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let mut block = Vec::with_capacity(1024);
            while block.len() < 1024 {
                let count = (1024 - block.len()).min(AUDIO.len() - position);
                block.extend_from_slice(&AUDIO[position..position + count]);
                position = (position + count) % AUDIO.len();
            }
            send(&block)?;
            let text = title.lock().unwrap().clone();
            send(&Self::metadata(sent, &text))?;
            thread::sleep(Duration::from_millis(5));
        }
        send(&[])?;
        Ok(())
    }

    fn set_title(&self, title: &str) {
        title.clone_into(&mut self.title.lock().unwrap());
    }

    /// A player whose only entry is this station, named "Fixture Radio".
    /// Output is unclocked: it plays as fast as the station sends.
    fn player(&self) -> anyhow::Result<Player> {
        let spec = StreamSpec::new(
            format!("http://{}/live", self.address),
            "Fixture Radio".into(),
        )?;
        Player::from_loaded(
            vec![LoadedEntry {
                id: 1,
                path: PathBuf::from(&spec.url),
                stream: Some(spec),
                run: None,
                played_seq: None,
            }],
            1,
            2,
            OutputConfig::Capture {
                path: PathBuf::from("/dev/null"),
                clocked: false,
            },
            GainMode::Off,
        )
    }
}

impl Drop for Station {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.thread.take().map(thread::JoinHandle::join);
    }
}

/// The radio play opened with now-playing text `raw`, if any.
fn radio_play<'a>(events: &'a [PlayEvent], raw: &str) -> Option<(u64, &'a RadioText)> {
    events.iter().find_map(|event| match event {
        PlayEvent::Opened {
            play,
            radio: Some(text),
            ..
        } if text.raw == raw => Some((*play, text)),
        _ => None,
    })
}

#[test]
fn radio_text_change_finishes_one_radio_play_and_opens_the_next() -> anyhow::Result<()> {
    let station = Station::start("Artist One - First Song")?;
    let mut player = station.player()?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |_, events| {
        radio_play(events, "Artist One - First Song").is_some()
    })?;
    station.set_title("Artist Two - Second Song");
    pump_until(&mut player, &mut events, |_, events| {
        radio_play(events, "Artist Two - Second Song").is_some()
    })?;
    let (first, _) = radio_play(&events, "Artist One - First Song").unwrap();
    let (second, text) = radio_play(&events, "Artist Two - Second Song").unwrap();
    assert_eq!(
        (text.artist.as_str(), text.title.as_str()),
        ("Artist Two", "Second Song")
    );
    assert!(!text.junk);
    let closed = events.iter().position(|event| {
        matches!(event, PlayEvent::Closed { play, reason: EndReason::Finished, .. } if *play == first)
    });
    let next = events
        .iter()
        .position(|event| matches!(event, PlayEvent::Opened { play, .. } if *play == second));
    assert!(closed.is_some() && closed < next, "{events:?}");
    assert_eq!(player.open_play().map(|(play, _)| play), Some(second));
    drop(player);
    Ok(())
}

#[test]
fn repeated_radio_text_does_not_split_the_play() -> anyhow::Result<()> {
    let station = Station::start("Artist One - First Song")?;
    let mut player = station.player()?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |_, events| {
        radio_play(events, "Artist One - First Song").is_some()
    })?;
    let (play, _) = radio_play(&events, "Artist One - First Song").unwrap();
    let start = listened(&player);
    let seen = events.len();
    // Ten seconds of radio repeat the same text in dozens of metadata blocks.
    pump_until(&mut player, &mut events, |player, _| {
        listened(player) >= start + 10_000
    })?;
    // The stream reports a metadata update; the text is the same.
    player.retitle(player.passes.len() - 1);
    events.extend(player.take_play_events());
    assert!(events[seen..].is_empty(), "{events:?}");
    assert_eq!(player.open_play().map(|(open, _)| open), Some(play));
    drop(player);
    Ok(())
}

#[test]
fn junk_radio_play_never_counts() -> anyhow::Result<()> {
    // Blank text stands for the station name.
    let station = Station::start("")?;
    let mut player = station.player()?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, _| {
        listened(player) >= 61_000
    })?;
    let [
        PlayEvent::Opened {
            play,
            radio: Some(ref text),
            ..
        },
    ] = events[..]
    else {
        panic!("{events:?}");
    };
    assert!(text.junk, "{text:?}");
    assert_eq!(player.open_play().map(|(open, _)| open), Some(play));
    drop(player);
    Ok(())
}

#[test]
fn leaving_a_station_closes_as_stopped() -> anyhow::Result<()> {
    let station = Station::start("Artist One - First Song")?;
    let mut player = station.player()?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |_, events| {
        radio_play(events, "Artist One - First Song").is_some()
    })?;
    let (play, _) = radio_play(&events, "Artist One - First Song").unwrap();
    player.end_play(EndReason::Skipped);
    let events = player.take_play_events();
    assert!(
        matches!(events[..], [PlayEvent::Closed { play: closed, reason: EndReason::Stopped, .. }] if closed == play),
        "{events:?}"
    );
    assert_eq!(player.open_play(), None);
    drop(player);
    Ok(())
}

/// Restores entry 1 of `station`'s player as the selection, with an open
/// radio play from before the restart.
fn restored_radio(station: &Station, counted: bool, raw: &str) -> anyhow::Result<(Player, u64)> {
    let mut player = station.player()?;
    let prepared = player.prepare_selection(1, 0, GainMode::Off)?;
    player.commit_selection(prepared);
    player.restored_selection();
    let play = player.restore_play(1, counted, 1_000, Some(raw));
    Ok((player, play))
}

fn closed_as(events: &[PlayEvent], play: u64) -> Option<EndReason> {
    events.iter().find_map(|event| match event {
        PlayEvent::Closed {
            play: closed,
            reason,
            ..
        } if *closed == play => Some(*reason),
        _ => None,
    })
}

#[test]
fn a_radio_play_counts_once_after_a_minute() -> anyhow::Result<()> {
    let station = Station::start("Artist One - First Song")?;
    let mut player = station.player()?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, events| {
        radio_play(events, "Artist One - First Song")
            .is_some_and(|(play, _)| player.open_play().map(|(open, _)| open) == Some(play))
            && listened(player) >= 61_000
    })?;
    let (play, _) = radio_play(&events, "Artist One - First Song").unwrap();
    let from = events
        .iter()
        .position(
            |event| matches!(event, PlayEvent::Opened { play: opened, .. } if *opened == play),
        )
        .unwrap();
    assert!(
        matches!(events[from + 1..], [PlayEvent::Counted { play: counted }] if counted == play),
        "{events:?}"
    );
    assert_eq!(counted(&events), [play]);
    drop(player);
    Ok(())
}

#[test]
fn radio_plays_after_a_restored_counted_play_are_not_forced() -> anyhow::Result<()> {
    let station = Station::start("Artist One - First Song")?;
    let (mut player, restored) = restored_radio(&station, true, "Artist One - First Song")?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |player, _| {
        listened(player) >= 3_000
    })?;
    station.set_title("Artist Two - Second Song");
    pump_until(&mut player, &mut events, |_, events| {
        radio_play(events, "Artist Two - Second Song").is_some()
    })?;
    station.set_title("Artist Three - Third Song");
    pump_until(&mut player, &mut events, |_, events| {
        radio_play(events, "Artist Three - Third Song").is_some()
    })?;
    let (second, _) = radio_play(&events, "Artist Two - Second Song").unwrap();
    assert_eq!(
        closed_as(&events, restored),
        Some(EndReason::Finished),
        "{events:?}"
    );
    assert_eq!(
        closed_as(&events, second),
        Some(EndReason::Finished),
        "{events:?}"
    );
    drop(player);
    Ok(())
}

#[test]
fn a_restored_radio_play_continues_with_the_same_text() -> anyhow::Result<()> {
    let station = Station::start("Burial - Archangel")?;
    let (mut player, restored) = restored_radio(&station, true, "Burial - Archangel")?;
    player.play();
    let mut events = Vec::new();
    // The station announces the restored text after about 8 s, then repeats it.
    pump_until(&mut player, &mut events, |player, _| {
        listened(player) >= 16_000
    })?;
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(player.open_play().map(|(open, _)| open), Some(restored));
    drop(player);
    Ok(())
}

#[test]
fn a_reconnect_keeps_the_radio_play() -> anyhow::Result<()> {
    // The first connection ends after 16 s of audio, 8 s after its text.
    let station = Station::cutting("Artist One - First Song", 4)?;
    let mut player = station.player()?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |_, events| {
        radio_play(events, "Artist One - First Song").is_some()
    })?;
    let (play, _) = radio_play(&events, "Artist One - First Song").unwrap();
    let seen = events.len();
    pump_until(&mut player, &mut events, |player, _| {
        station.connections.load(Ordering::Acquire) >= 2 && listened(player) >= 61_000
    })?;
    // The new connection starts from the song's text: no split, one count.
    assert_eq!(events[seen..], [PlayEvent::Counted { play }], "{events:?}");
    assert_eq!(player.open_play().map(|(open, _)| open), Some(play));
    drop(player);
    Ok(())
}

#[test]
fn a_gain_change_keeps_the_radio_play() -> anyhow::Result<()> {
    let station = Station::start("Artist One - First Song")?;
    let mut player = station.player()?;
    player.play();
    let mut events = Vec::new();
    pump_until(&mut player, &mut events, |_, events| {
        radio_play(events, "Artist One - First Song").is_some()
    })?;
    let (play, _) = radio_play(&events, "Artist One - First Song").unwrap();
    let seen = events.len();
    let before = listened(&player);
    let shown = |player: &Player| player.snapshot().now_playing.map(|now| now.title.clone());
    // A gain change replaces the decoder, which opens a new connection. It
    // shows the song, not the station name, until the station announces.
    let prepared = player.prepare_selection(1, 0, GainMode::Track)?;
    player.commit_selection(prepared);
    let mut titles = HashSet::new();
    pump_until(&mut player, &mut events, |player, _| {
        titles.extend(shown(player));
        station.connections.load(Ordering::Acquire) >= 2 && listened(player) >= before + 10_000
    })?;
    assert_eq!(
        titles,
        HashSet::from(["Artist One - First Song".to_owned()])
    );
    assert!(events[seen..].is_empty(), "{events:?}");
    assert_eq!(player.open_play().map(|(open, _)| open), Some(play));
    drop(player);
    Ok(())
}
