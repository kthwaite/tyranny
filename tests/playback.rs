use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use tyranny::{
    decoder::{Decoder, GainMode},
    output::OutputConfig,
    play_order::{RepeatMode, RepeatScope, UniformShuffle},
    playback::Player,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tyranny-playback-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
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

fn waveform(frame: usize) -> [i16; 2] {
    let value = ((frame % 1024) as i16) * 8 - 4096;
    [value, -value]
}
fn wav(path: &Path, frames: usize) {
    let mut file = BufWriter::new(fs::File::create(path).unwrap());
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + frames as u32 * 4).to_le_bytes())
        .unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16_u32.to_le_bytes()).unwrap();
    file.write_all(&1_u16.to_le_bytes()).unwrap();
    file.write_all(&2_u16.to_le_bytes()).unwrap();
    file.write_all(&48_000_u32.to_le_bytes()).unwrap();
    file.write_all(&192_000_u32.to_le_bytes()).unwrap();
    file.write_all(&4_u16.to_le_bytes()).unwrap();
    file.write_all(&16_u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&(frames as u32 * 4).to_le_bytes()).unwrap();
    for frame in 0..frames {
        for sample in waveform(frame) {
            file.write_all(&sample.to_le_bytes()).unwrap();
        }
    }
    file.flush().unwrap();
}
fn pcm(path: &Path) -> Vec<[f32; 2]> {
    fs::read(path)
        .unwrap()
        .as_chunks::<8>()
        .0
        .iter()
        .map(|bytes| {
            [
                f32::from_le_bytes(bytes[..4].try_into().unwrap()),
                f32::from_le_bytes(bytes[4..].try_into().unwrap()),
            ]
        })
        .collect()
}
fn finish(player: &mut Player) {
    let deadline = Instant::now() + Duration::from_secs(10);
    player.play();
    while !player.completed() {
        assert!(
            Instant::now() < deadline,
            "playback did not finish: {:?}",
            player.snapshot()
        );
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    player.pump().unwrap();
    assert!(player.error().is_none(), "{:?}", player.error());
}

#[test]
fn repeated_tracks_have_distinct_queue_identity_and_stale_edits_do_not_remove_them() {
    let scratch = Scratch::new();
    let source = scratch.path("source.wav");
    let capture = scratch.path("capture.f32");
    wav(&source, 137);
    let mut player = Player::new(
        vec![source.clone(), source.clone()],
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: false,
        },
        GainMode::Off,
    )
    .unwrap();
    let revision = player.snapshot().queue_revision;
    assert_eq!(player.append(source, revision).unwrap(), 3);
    assert!(player.remove(2, revision).is_err());
    finish(&mut player);
    assert_eq!(player.started_entries(), &[1, 2, 3]);
    drop(player);
    let actual = pcm(&capture);
    assert_eq!(actual.len(), 137 * 3);
    for (index, samples) in actual.iter().enumerate() {
        assert_eq!(
            *samples,
            waveform(index % 137).map(|value| value as f32 / 32768.0)
        );
    }
}

#[test]
fn seek_returns_exact_target_samples_and_can_seek_backwards() {
    let scratch = Scratch::new();
    let source = scratch.path("seek.wav");
    wav(&source, 144_000);
    let mut decoder = Decoder::open(&source, GainMode::Off).unwrap();
    let mut buffer = [[0.0; 2]; 1024];
    for target in [48_017, 1_337, 119_123] {
        decoder.seek(target).unwrap();
        let count = decoder.read(&mut buffer).unwrap();
        assert!(count > 0);
        for (index, samples) in buffer[..count].iter().enumerate() {
            assert_eq!(
                *samples,
                waveform(target as usize + index).map(|value| value as f32 / 32768.0)
            );
        }
    }
}

#[test]
fn removing_current_entry_stops_consumption_instead_of_playing_a_stale_next_entry() {
    let scratch = Scratch::new();
    let source = scratch.path("long.wav");
    wav(&source, 96_000);
    let mut player = Player::new(
        vec![source.clone(), source],
        OutputConfig::Capture {
            path: scratch.path("capture.f32"),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    player.play();
    let deadline = Instant::now() + Duration::from_secs(5);
    while player.snapshot().current_id != Some(1) {
        assert!(Instant::now() < deadline);
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    player.remove(1, player.snapshot().queue_revision).unwrap();
    thread::sleep(Duration::from_millis(50));
    player.pump().unwrap();
    let stopped = player.snapshot();
    assert!(stopped.stopped);
    assert_eq!(stopped.current_id, None);
    thread::sleep(Duration::from_millis(50));
    player.pump().unwrap();
    assert_eq!(
        player.snapshot().output.rendered_frames,
        stopped.output.rendered_frames
    );
    assert_eq!(player.started_entries(), &[1]);
    assert_eq!(player.queue_ids(), vec![2]);
}

#[test]
fn seek_resume_waits_for_replacement_pcm_not_invalidated_prefetch() {
    let scratch = Scratch::new();
    let source = scratch.path("seek-resume.wav");
    wav(&source, 144_000);
    let mut player = Player::new(
        vec![source],
        OutputConfig::Capture {
            path: scratch.path("capture.f32"),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    player.play();
    let deadline = Instant::now() + Duration::from_secs(5);
    while player.snapshot().current_id.is_none() {
        assert!(Instant::now() < deadline);
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    player.pause();
    for _ in 0..100 {
        player.pump().unwrap();
    }
    thread::sleep(Duration::from_millis(50));
    player.seek(48_000).unwrap();
    player.play();
    // No replacement PCM has been submitted since seek. A resume request
    // must keep the consumer paused, even if stale blocks await reclamation.
    assert!(player.snapshot().output.paused);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(player.snapshot().position_frames, 48_000);
    assert_eq!(player.snapshot().output.underruns, 0);
    finish(&mut player);
    assert_eq!(player.snapshot().output.underruns, 0);
}

#[test]
fn restored_queue_preserves_identity_and_selection_without_unsolicited_playback() {
    let scratch = Scratch::new();
    let source = scratch.path("restore.wav");
    let missing = scratch.path("missing.wav");
    let capture = scratch.path("capture.f32");
    wav(&source, 24_000);
    let mut player = Player::from_queue(
        vec![(41, missing.clone()), (7, source.clone())],
        19,
        99,
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: false,
        },
        GainMode::Off,
    )
    .unwrap();
    assert_eq!(
        player.queue_entries().collect::<Vec<_>>(),
        vec![(41, missing.as_path()), (7, source.as_path())]
    );
    assert_eq!(player.next_id(), 99);
    for _ in 0..10 {
        player.pump().unwrap();
    }
    let initial = player.snapshot();
    assert_eq!(initial.queue_revision, 19);
    assert!(initial.stopped);
    assert!(!initial.playing);
    assert_eq!(initial.current_id, None);
    assert_eq!(initial.position_frames, 0);
    assert_eq!(initial.output.buffered_frames, 0);
    assert!(player.gains().is_empty());
    assert!(player.error().is_none());
    assert!(player.prepare_selection(41, 0, GainMode::Off).is_err());

    let prepared = player.prepare_selection(7, 1_337, GainMode::Off).unwrap();
    assert!(player.snapshot().stopped);
    assert_eq!(player.snapshot().current_id, None);
    player.commit_selection(prepared);
    for _ in 0..100 {
        player.pump().unwrap();
    }
    let selected = player.snapshot();
    assert!(!selected.stopped);
    assert!(!selected.playing);
    assert!(selected.output.paused);
    assert_eq!(selected.current_id, Some(7));
    assert_eq!(selected.position_frames, 1_337);
    assert_eq!(selected.output.rendered_frames, 0);

    finish(&mut player);
    player.clear_diagnostics();
    assert!(player.gains().is_empty());
    assert!(player.started_entries().is_empty());
    player.clear(19).unwrap();
    assert_eq!(player.snapshot().queue_revision, 20);
    assert_eq!(player.queue_entries().len(), 0);
    assert_eq!(player.snapshot().current_id, None);
    assert_eq!(player.snapshot().position_frames, 0);
    assert_eq!(player.next_id(), 99);
    assert_eq!(player.append(source, 20).unwrap(), 99);
    assert!(player.snapshot().stopped);
    drop(player);
    let actual = pcm(&capture);
    assert_eq!(actual.len(), 24_000 - 1_337);
    for (index, samples) in actual.iter().enumerate() {
        assert_eq!(
            *samples,
            waveform(index + 1_337).map(|value| value as f32 / 32768.0)
        );
    }
}

#[test]
fn stop_and_end_replay_restart_from_the_beginning_without_stale_position() {
    let scratch = Scratch::new();
    let source = scratch.path("replay.wav");
    let capture = scratch.path("capture.f32");
    wav(&source, 4_097);
    let mut player = Player::new(
        vec![source],
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: false,
        },
        GainMode::Off,
    )
    .unwrap();
    finish(&mut player);
    assert_eq!(player.snapshot().position_frames, 4_097);
    player.stop();
    let stopped = player.snapshot();
    assert!(!stopped.playing);
    assert_eq!(stopped.current_id, None);
    assert_eq!(stopped.position_frames, 0);
    player.play();
    let restarting = player.snapshot();
    assert!(restarting.playing);
    assert_eq!(restarting.current_id, None);
    assert_eq!(restarting.position_frames, 0);
    assert!(restarting.output.paused);
    finish(&mut player);
    finish(&mut player);
    drop(player);
    let actual = pcm(&capture);
    assert_eq!(actual.len(), 4_097 * 3);
    for (index, samples) in actual.iter().enumerate() {
        assert_eq!(
            *samples,
            waveform(index % 4_097).map(|value| value as f32 / 32768.0)
        );
    }
}

#[test]
fn prepared_selection_waits_for_its_new_token_despite_old_output_advancing() {
    let scratch = Scratch::new();
    let source = scratch.path("selection.wav");
    wav(&source, 144_000);
    let mut player = Player::new(
        vec![source],
        OutputConfig::Capture {
            path: scratch.path("capture.f32"),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    player.play();
    let deadline = Instant::now() + Duration::from_secs(5);
    while player.snapshot().current_id.is_none() {
        assert!(Instant::now() < deadline);
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    let before = player.snapshot();
    let prepared = player.prepare_selection(1, 48_017, GainMode::Off).unwrap();
    // Models time spent durably committing while the output callback continues.
    while player.snapshot().output.rendered_frames == before.output.rendered_frames {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    player.commit_selection(prepared);
    assert!(player.snapshot().playing);
    for _ in 0..10 {
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(2));
        let selected = player.snapshot();
        assert!(selected.output.paused);
        assert_eq!(selected.current_id, Some(1));
        assert_eq!(selected.position_frames, 48_017);
    }
    player.pause();
    for _ in 0..100 {
        player.pump().unwrap();
    }
    assert_eq!(player.snapshot().position_frames, 48_017);
    player.play();
    while player.snapshot().position_frames == 48_017 {
        assert!(Instant::now() < deadline);
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    assert!(player.snapshot().position_frames > 48_017);
}

#[test]
fn moving_the_active_entry_keeps_its_pcm_and_rebuilds_only_its_new_future() {
    let scratch = Scratch::new();
    let current = scratch.path("current.wav");
    let skipped = scratch.path("skipped.wav");
    let future = scratch.path("future.wav");
    let capture = scratch.path("capture.f32");
    wav(&current, 48_000);
    wav(&skipped, 311);
    wav(&future, 137);
    let mut player = Player::new(
        vec![current, skipped, future],
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    player.play();
    let deadline = Instant::now() + Duration::from_secs(5);
    while player.snapshot().current_id != Some(1) {
        assert!(Instant::now() < deadline);
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    player.pause();
    for _ in 0..100 {
        player.pump().unwrap();
    }
    player.move_before(1, Some(3), 1).unwrap();
    assert_eq!(player.queue_ids(), vec![2, 1, 3]);
    assert_eq!(player.snapshot().current_id, Some(1));
    finish(&mut player);
    assert_eq!(player.started_entries(), &[1, 3]);
    drop(player);
    let actual = pcm(&capture);
    assert_eq!(actual.len(), 48_000 + 137);
    for (index, samples) in actual.iter().enumerate() {
        let source_frame = if index < 48_000 {
            index
        } else {
            index - 48_000
        };
        assert_eq!(
            *samples,
            waveform(source_frame).map(|value| value as f32 / 32768.0)
        );
    }
}

#[test]
fn exhausted_revisions_reject_queue_mutations_without_partial_changes() {
    let scratch = Scratch::new();
    let first = scratch.path("missing-first.wav");
    let second = scratch.path("missing-second.wav");
    let mut player = Player::from_queue(
        vec![(5, first.clone()), (8, second)],
        u64::MAX,
        9,
        OutputConfig::Capture {
            path: scratch.path("capture.f32"),
            clocked: false,
        },
        GainMode::Off,
    )
    .unwrap();
    assert!(player.append(first, u64::MAX).is_err());
    assert!(player.remove(5, u64::MAX).is_err());
    assert!(player.move_before(5, None, u64::MAX).is_err());
    assert!(player.clear(u64::MAX).is_err());
    assert_eq!(player.queue_ids(), vec![5, 8]);
    assert_eq!(player.next_id(), 9);
    assert_eq!(player.snapshot().queue_revision, u64::MAX);
    assert!(player.snapshot().stopped);
}

/// The capture holds whole passes of `waveform`, one per length, in order.
fn assert_passes(actual: &[[f32; 2]], lengths: &[usize]) {
    assert_eq!(actual.len(), lengths.iter().sum::<usize>());
    let mut frames = actual.iter();
    for &length in lengths {
        for frame in 0..length {
            assert_eq!(
                *frames.next().unwrap(),
                waveform(frame).map(|value| value as f32 / 32768.0)
            );
        }
    }
}

#[test]
fn repeat_one_replays_the_entry_gaplessly_with_correct_position() {
    let scratch = Scratch::new();
    let source = scratch.path("repeat.wav");
    let capture = scratch.path("capture.f32");
    wav(&source, 9_600);
    let mut player = Player::new(
        vec![source],
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    player.set_repeat(RepeatMode::One);
    player.play();
    let deadline = Instant::now() + Duration::from_secs(5);
    // Pass 1 then pass 2: the entry stays current and the position restarts.
    loop {
        assert!(
            Instant::now() < deadline,
            "pass 2 was not heard: {:?}",
            player.snapshot()
        );
        player.pump().unwrap();
        let state = player.snapshot();
        let rendered = state.output.rendered_frames;
        if rendered > 0 {
            assert_eq!(state.current_id, Some(1), "{state:?}");
            assert_eq!(state.next_id, Some(1), "{state:?}");
        }
        if rendered > 9_600 {
            assert!(rendered < 19_200, "{state:?}");
            assert_eq!(state.position_frames, rendered - 9_600, "{state:?}");
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    // Mid-pass-2, Repeat Off ends the queue after this pass.
    player.set_repeat(RepeatMode::Off);
    let state = player.snapshot();
    assert_eq!(state.current_id, Some(1));
    assert!(state.position_frames < 9_600, "{state:?}");
    assert_eq!(state.next_id, None);
    finish(&mut player);
    assert_eq!(player.snapshot().output.underruns, 0);
    assert_eq!(player.started_entries(), &[1]);
    drop(player);
    assert_passes(&pcm(&capture), &[9_600, 9_600]);
}

#[test]
fn repeat_all_album_run_wraps_without_reaching_following_entries() {
    let scratch = Scratch::new();
    let capture = scratch.path("capture.f32");
    let lengths = [3_001, 4_801, 7_207, 2_003];
    let paths: Vec<PathBuf> = lengths
        .iter()
        .enumerate()
        .map(|(index, frames)| {
            let path = scratch.path(&format!("{index}.wav"));
            wav(&path, *frames);
            path
        })
        .collect();
    // A loose entry, an album run of two entries, then a following entry.
    let mut player = Player::new(
        vec![paths[0].clone()],
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    let run = player
        .append_run(paths[1..3].to_vec(), 1, RepeatScope::Run)
        .unwrap();
    assert_eq!(run, 2);
    assert_eq!(player.append(paths[3].clone(), 2).unwrap(), 4);
    player.set_repeat(RepeatMode::All);
    let prepared = player.prepare_selection(2, 0, GainMode::Off).unwrap();
    player.commit_selection(prepared);
    player.play();
    let deadline = Instant::now() + Duration::from_secs(5);
    // The run wraps twice, and never reaches entry 1 or entry 4.
    while player.started_entries().len() < 5 {
        assert!(
            Instant::now() < deadline,
            "run did not wrap: {:?}",
            player.started_entries()
        );
        player.pump().unwrap();
        let state = player.snapshot();
        assert!(matches!(state.current_id, None | Some(2 | 3)), "{state:?}");
        assert!(matches!(state.next_id, None | Some(2 | 3)), "{state:?}");
        thread::sleep(Duration::from_millis(1));
    }
    // Repeat Off plays out the run, then the following entry.
    player.set_repeat(RepeatMode::Off);
    finish(&mut player);
    let started = player.started_entries().to_vec();
    let (last, wrapped) = started.split_last().unwrap();
    assert_eq!(*last, 4, "{started:?}");
    assert!(wrapped.len() >= 6 && wrapped.len() % 2 == 0, "{started:?}");
    for (index, id) in wrapped.iter().enumerate() {
        assert_eq!(*id, 2 + index as u64 % 2, "{started:?}");
    }
    drop(player);
    let heard: Vec<usize> = started.iter().map(|id| lengths[*id as usize - 1]).collect();
    assert_passes(&pcm(&capture), &heard);
}

#[test]
fn shuffle_plays_each_upcoming_entry_once_then_stops_under_off() {
    let scratch = Scratch::new();
    let capture = scratch.path("capture.f32");
    let lengths: Vec<usize> = (0..6).map(|index| 4_800 + 480 * index).collect();
    let paths = lengths
        .iter()
        .enumerate()
        .map(|(index, frames)| {
            let path = scratch.path(&format!("{index}.wav"));
            wav(&path, *frames);
            path
        })
        .collect();
    let mut player = Player::new(
        paths,
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    player.set_shuffle(Some(Box::new(UniformShuffle::seeded(7))));
    player.play();
    let deadline = Instant::now() + Duration::from_secs(10);
    // What the snapshot said would follow each current entry.
    let mut following = std::collections::HashMap::new();
    while !player.completed() {
        assert!(Instant::now() < deadline, "playback did not finish");
        player.pump().unwrap();
        let state = player.snapshot();
        if let Some(current) = state.current_id {
            let next = *following.entry(current).or_insert(state.next_id);
            assert_eq!(next, state.next_id, "next of {current} changed");
        }
        thread::sleep(Duration::from_millis(1));
    }
    player.pump().unwrap();
    let started = player.started_entries().to_vec();
    let mut sorted = started.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, [1, 2, 3, 4, 5, 6]);
    assert_eq!(started[0], 1);
    assert_ne!(started, sorted, "seed 7 should not keep queue order");
    // The decided next is the entry that played next.
    for pair in started.windows(2) {
        if let Some(next) = following.get(&pair[0]) {
            assert_eq!(*next, Some(pair[1]), "{started:?}");
        }
    }
    assert_eq!(following.get(started.last().unwrap()), Some(&None));
    drop(player);
    let heard: Vec<usize> = started.iter().map(|id| lengths[*id as usize - 1]).collect();
    assert_passes(&pcm(&capture), &heard);
}

#[test]
fn seek_and_gain_reselection_keep_the_shuffled_next() {
    let scratch = Scratch::new();
    let paths = (0..8)
        .map(|index| {
            let path = scratch.path(&format!("{index}.wav"));
            wav(&path, 96_000);
            path
        })
        .collect();
    let mut player = Player::new(
        paths,
        OutputConfig::Capture {
            path: scratch.path("capture.f32"),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    player.set_shuffle(Some(Box::new(UniformShuffle::seeded(11))));
    player.play();
    let deadline = Instant::now() + Duration::from_secs(5);
    while player.snapshot().current_id != Some(1) {
        assert!(Instant::now() < deadline, "playback did not start");
        player.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    let next = player.snapshot().next_id;
    assert!(next.is_some());
    // Re-selecting the current entry never rerolls the shuffled next.
    for frame in [4_800, 24_000, 1_000, 48_000, 2_400] {
        player.seek(frame).unwrap();
        assert_eq!(player.snapshot().next_id, next);
        let prepared = player.prepare_selection(1, frame, GainMode::Track).unwrap();
        player.commit_selection(prepared);
        assert_eq!(player.snapshot().next_id, next);
        player.pump().unwrap();
    }
}

#[test]
fn repeat_one_counts_each_pass() {
    let scratch = Scratch::new();
    let source = scratch.path("repeat.wav");
    let capture = scratch.path("capture.f32");
    wav(&source, 4_800);
    let mut player = Player::new(
        vec![source],
        OutputConfig::Capture {
            path: capture.clone(),
            clocked: true,
        },
        GainMode::Off,
    )
    .unwrap();
    player.set_repeat(RepeatMode::One);
    player.play();
    let mut counted = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while counted.len() < 3 {
        assert!(Instant::now() < deadline, "{counted:?}");
        player.pump().unwrap();
        counted.extend(player.take_counted_plays());
        thread::sleep(Duration::from_millis(1));
    }
    // Repeat Off ends the queue after the pass being heard.
    player.set_repeat(RepeatMode::Off);
    while !player.completed() {
        assert!(Instant::now() < deadline, "{:?}", player.snapshot());
        player.pump().unwrap();
        counted.extend(player.take_counted_plays());
        thread::sleep(Duration::from_millis(1));
    }
    player.pump().unwrap();
    counted.extend(player.take_counted_plays());
    drop(player);
    let heard = pcm(&capture);
    assert_eq!(heard.len() % 4_800, 0);
    // Every pass counts, each once.
    assert_eq!(counted, vec![1; heard.len() / 4_800]);
}

#[cfg(target_os = "macos")]
mod coreaudio_hardware {
    use super::*;
    use std::process::Command;
    use tyranny::{
        BLOCK_FRAMES,
        output::{Output, TrackToken},
    };

    fn default_output_device() -> (String, u64) {
        let result = Command::new("/usr/sbin/system_profiler")
            .args(["SPAudioDataType", "-json"])
            .output()
            .expect("reading the default output device without changing its settings");
        assert!(
            result.status.success(),
            "system_profiler failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let profile: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let mut defaults = profile["SPAudioDataType"]
            .as_array()
            .expect("audio device groups")
            .iter()
            .flat_map(|group| group["_items"].as_array().expect("audio devices"))
            .filter(|device| device["coreaudio_default_audio_output_device"] == "spaudio_yes");
        let device = defaults.next().expect("a default audio output device");
        assert!(defaults.next().is_none(), "multiple default output devices");
        (
            device["_name"]
                .as_str()
                .expect("default output device name")
                .to_owned(),
            device["coreaudio_device_srate"]
                .as_u64()
                .expect("default output device nominal sample rate"),
        )
    }

    #[test]
    #[ignore = "requires a macOS audio device; plays only silence; use a non-48 kHz default output to exercise conversion"]
    fn hardware_output_preserves_device_rate_and_48khz_timeline() {
        let hardware_before = default_output_device();
        let mut output = Output::open(OutputConfig::CoreAudio)
            .expect("opening 48 kHz application PCM on the unchanged default output device");
        assert_eq!(
            default_output_device(),
            hardware_before,
            "opening output changed the default device or its nominal sample rate"
        );

        let first = TrackToken::new(11);
        let second = TrackToken::new(22);
        let silence = [[0.0; 2]; BLOCK_FRAMES];
        // Two half-second tracks in the application's 48 kHz timeline. The
        // second begins at a one-second seek offset, not a hardware-rate offset.
        for (token, start_frame) in [(&first, 0), (&second, 48_000)] {
            for offset in (0..24_000).step_by(BLOCK_FRAMES) {
                let count = (24_000 - offset).min(BLOCK_FRAMES);
                assert!(
                    output
                        .try_submit(token, start_frame + offset as u64, &silence[..count])
                        .unwrap(),
                    "one second of PCM must fit in the initially paused pool"
                );
            }
        }
        output.mark_end(true);
        thread::sleep(Duration::from_millis(100));
        output.check_error().unwrap();
        let initial = output.snapshot();
        assert!(initial.paused);
        assert!(!initial.drained);
        assert_eq!(initial.entry_id, 0);
        assert_eq!(initial.position_frames, 0);
        assert_eq!(initial.rendered_frames, 0);
        assert_eq!(initial.buffered_frames, 48_000);
        assert_eq!(output.take_started(), None);

        output.set_paused(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while output.snapshot().rendered_frames == 0 {
            output.check_error().unwrap();
            assert!(
                Instant::now() < deadline,
                "the hardware never consumed PCM: {:?}",
                output.snapshot()
            );
            thread::sleep(Duration::from_millis(1));
        }
        output.set_paused(true);
        // Pause cannot recall a buffer already handed to the device. Let any
        // in-flight callback publish before checking that consumption stops.
        thread::sleep(Duration::from_millis(100));
        output.check_error().unwrap();
        let paused = output.snapshot();
        assert!(paused.paused);
        assert!(!paused.drained);
        assert!(paused.rendered_frames > 0);
        assert!(paused.buffered_frames > 0, "PCM ended before the pause");
        thread::sleep(Duration::from_millis(150));
        output.check_error().unwrap();
        let still_paused = output.snapshot();
        assert!(still_paused.paused);
        assert!(!still_paused.drained);
        assert_eq!(still_paused.entry_id, paused.entry_id);
        assert_eq!(still_paused.position_frames, paused.position_frames);
        assert_eq!(still_paused.rendered_frames, paused.rendered_frames);
        assert_eq!(still_paused.buffered_frames, paused.buffered_frames);

        output.set_paused(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        let finished = loop {
            output.check_error().unwrap();
            let snapshot = output.snapshot();
            if snapshot.drained {
                break snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "hardware output never drained: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(1));
        };
        assert!(!finished.paused);
        assert_eq!(finished.entry_id, 22);
        assert_eq!(finished.position_frames, 72_000);
        assert_eq!(finished.rendered_frames, 48_000);
        assert_eq!(finished.buffered_frames, 0);
        assert_eq!(finished.underruns, 0);
        assert_eq!(output.take_started(), Some(11));
        assert_eq!(output.take_started(), Some(22));
        assert_eq!(output.take_started(), None);
        assert_eq!(
            default_output_device(),
            hardware_before,
            "playback changed the default device or its nominal sample rate"
        );
        drop(output);
        assert_eq!(
            default_output_device(),
            hardware_before,
            "teardown changed the default device or its nominal sample rate"
        );
    }
}
