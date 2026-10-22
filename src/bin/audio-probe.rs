//! Native FFmpeg/CoreAudio gate. Capture and device playback share the same
//! bounded output consumer. Gapless is pinned by PCM comparison in tests/gapless.rs.
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    env,
    ffi::OsStr,
    path::PathBuf,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};
use tyranny_likes_smoke::{SAMPLE_RATE, decoder::GainMode, output::OutputConfig, playback::Player};

const USAGE: &str = "audio-probe --output <coreaudio|/absolute/capture.f32> --gain <off|track|album> [--exercise-transport] [--clocked] [--exercise-queue <remove-next|move-next>] <absolute media paths...>";

#[derive(Clone, Copy)]
enum QueueExercise {
    Remove,
    Move,
}
struct Options {
    output: Option<PathBuf>,
    gain: GainMode,
    transport: bool,
    clocked: bool,
    queue: Option<QueueExercise>,
    paths: Vec<PathBuf>,
}
impl Options {
    fn parse() -> Result<Self> {
        let mut args = env::args_os().skip(1);
        let (mut output, mut gain, mut queue) = (None, None, None);
        let (mut transport, mut clocked) = (false, false);
        let mut paths = Vec::new();
        while let Some(arg) = args.next() {
            if arg == "--output" {
                ensure!(output.is_none(), "--output specified twice");
                let value = args.next().context("--output needs a value")?;
                output = Some(if value == "coreaudio" {
                    None
                } else {
                    let path = PathBuf::from(value);
                    ensure!(path.is_absolute(), "capture output must be absolute");
                    Some(path)
                });
            } else if arg == "--gain" {
                ensure!(gain.is_none(), "--gain specified twice");
                gain = Some(match args.next().as_deref().and_then(OsStr::to_str) {
                    Some("off") => GainMode::Off,
                    Some("track") => GainMode::Track,
                    Some("album") => GainMode::Album,
                    _ => bail!("--gain must be off, track, or album"),
                });
            } else if arg == "--exercise-transport" {
                transport = true;
            } else if arg == "--clocked" {
                clocked = true;
            } else if arg == "--exercise-queue" {
                ensure!(queue.is_none(), "--exercise-queue specified twice");
                queue = Some(match args.next().as_deref().and_then(OsStr::to_str) {
                    Some("remove-next") => QueueExercise::Remove,
                    Some("move-next") => QueueExercise::Move,
                    _ => bail!("--exercise-queue must be remove-next or move-next"),
                });
            } else {
                let path = PathBuf::from(arg);
                ensure!(
                    path.is_absolute() && path.is_file(),
                    "media must be an existing absolute file: {}",
                    path.display()
                );
                paths.push(path);
            }
        }
        ensure!(!paths.is_empty(), "at least one media path is required");
        ensure!(
            queue.is_none() || paths.len() == 3,
            "queue exercise requires exactly three tracks"
        );
        ensure!(
            !(transport && queue.is_some()),
            "transport and queue exercises must run separately"
        );
        let output = output.context("missing --output")?;
        if let Some(capture) = &output
            && let Ok(canonical) = capture.canonicalize()
        {
            for input in &paths {
                ensure!(
                    input.canonicalize()? != canonical,
                    "capture output is also an input file"
                );
            }
        }
        Ok(Self {
            output,
            gain: gain.context("missing --gain")?,
            transport,
            clocked,
            queue,
            paths,
        })
    }
}

#[derive(Default, Serialize)]
struct Summary {
    engine: &'static str,
    streams: usize,
    eos: bool,
    errors: Vec<String>,
    transport_exercised: bool,
    queue_edit_exercised: bool,
    started_entries: Vec<u64>,
    gain_db: Vec<f64>,
    underruns: u64,
    max_buffered_frames: usize,
}

enum Transport {
    Waiting,
    Settling(Instant),
    Paused(Instant, u64),
    Seeking,
    Resuming(Instant),
    Done,
}
impl Transport {
    fn tick(&mut self, player: &mut Player) -> Result<bool> {
        let state = player.snapshot();
        match *self {
            Self::Waiting
                if state.current_id.is_some()
                    && (SAMPLE_RATE as u64 / 4..SAMPLE_RATE as u64 * 3 / 4)
                        .contains(&state.position_frames) =>
            {
                player.pause();
                *self = Self::Settling(Instant::now());
            }
            Self::Settling(start) if start.elapsed() >= Duration::from_millis(50) => {
                ensure!(state.output.paused, "output failed to pause");
                *self = Self::Paused(Instant::now(), state.position_frames);
            }
            Self::Paused(start, frame) if start.elapsed() >= Duration::from_millis(200) => {
                ensure!(
                    state.output.paused && state.position_frames == frame,
                    "position advanced while paused"
                );
                player.seek(SAMPLE_RATE as u64)?;
                eprintln!("transport: paused position stable; sought to frame {SAMPLE_RATE}");
                *self = Self::Seeking;
            }
            Self::Seeking
                if state.output.buffered_frames >= SAMPLE_RATE as usize / 2
                    || state.decoding_finished =>
            {
                ensure!(
                    state.position_frames == SAMPLE_RATE as u64,
                    "seek did not establish requested position"
                );
                player.play();
                *self = Self::Resuming(Instant::now());
            }
            Self::Resuming(start) => {
                ensure!(
                    start.elapsed() < Duration::from_secs(5),
                    "resume failed to advance within five seconds"
                );
                if !state.output.paused && state.position_frames >= SAMPLE_RATE as u64 * 6 / 5 {
                    eprintln!("transport: consumption advanced after resume");
                    *self = Self::Done;
                }
            }
            _ => {}
        }
        Ok(matches!(self, Self::Done))
    }
}

fn run(summary: &mut Summary) -> Result<()> {
    let options = Options::parse().with_context(|| USAGE)?;
    let expected: Vec<u64> = match options.queue {
        Some(QueueExercise::Remove) => vec![1, 3],
        Some(QueueExercise::Move) => vec![1, 3, 2],
        None => (1..=options.paths.len() as u64).collect(),
    };
    let config = match options.output {
        Some(path) => OutputConfig::Capture {
            path,
            clocked: options.clocked || options.transport || options.queue.is_some(),
        },
        None => OutputConfig::CoreAudio,
    };
    let mut player = Player::new(options.paths, config, options.gain)?;
    player.play();
    let mut transport = Transport::Waiting;
    let start = Instant::now();
    let playback = (|| -> Result<()> {
        loop {
            ensure!(
                start.elapsed() < Duration::from_secs(60),
                "playback exceeded 60-second probe deadline"
            );
            player.pump()?;
            let state = player.snapshot();
            summary.max_buffered_frames = summary
                .max_buffered_frames
                .max(state.output.buffered_frames);
            if options.transport && !summary.transport_exercised {
                summary.transport_exercised = transport.tick(&mut player)?;
            }
            if let Some(exercise) = options.queue
                && !summary.queue_edit_exercised
                && state.current_id == Some(1)
                && state.prepared_id == Some(2)
            {
                match exercise {
                    QueueExercise::Remove => player.remove(2, state.queue_revision)?,
                    QueueExercise::Move => player.move_before(2, None, state.queue_revision)?,
                }
                summary.queue_edit_exercised = true;
                eprintln!("queue: invalidated prepared entry 2 while entry 1 was playing");
            }
            if player.completed() {
                player.pump()?;
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        if let Some(error) = player.error() {
            bail!("{error}");
        }
        ensure!(
            player.started_entries() == expected,
            "wrong playback sequence: {:?}, expected {expected:?}",
            player.started_entries()
        );
        ensure!(
            !options.transport || summary.transport_exercised,
            "transport exercise incomplete; provide a seekable track at least six seconds long"
        );
        ensure!(
            options.queue.is_none() || summary.queue_edit_exercised,
            "queue exercise did not reach prepared-next state; use a first track around one second long"
        );
        summary.eos = true;
        Ok(())
    })();
    summary.started_entries = player.started_entries().to_vec();
    summary.streams = summary.started_entries.len();
    summary.gain_db = player.gains().to_vec();
    summary.underruns = player.snapshot().output.underruns;
    playback
}

fn main() -> ExitCode {
    let mut summary = Summary {
        engine: "ffmpeg-coreaudio",
        ..Summary::default()
    };
    if let Err(error) = run(&mut summary) {
        summary.errors.push(format!("{error:#}"));
    }
    for error in &summary.errors {
        eprintln!("audio-probe: {error}");
    }
    let failed = !summary.errors.is_empty();
    match serde_json::to_string(&summary) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("cannot serialize probe summary: {error}");
            return ExitCode::FAILURE;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
