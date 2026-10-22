use anyhow::{Context, Result, bail, ensure};
use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};
use tokio::signal::unix::{SignalKind, signal};
use tyranny_likes_smoke::{
    output::OutputConfig,
    server::{Server, ServerConfig, default_state_dir},
};

const USAGE: &str = "tyranny-likes-smoke-server [--state-dir PATH] [--capture PATH]\n\
Default output is the real CoreAudio device; --capture writes clocked stereo f32 PCM.\n\
Control uses the private Unix socket <state-dir>/control.sock.";

#[derive(serde::Serialize)]
struct Ready<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    socket_path: &'a Path,
    instance_id: &'a str,
}

fn options() -> Result<Option<ServerConfig>> {
    let mut args = env::args_os().skip(1);
    let (mut state_dir, mut capture) = (None, None);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("{USAGE}");
            return Ok(None);
        } else if arg == "--state-dir" {
            ensure!(state_dir.is_none(), "--state-dir specified twice");
            state_dir = Some(PathBuf::from(
                args.next().context("--state-dir needs a path")?,
            ));
        } else if arg == "--capture" {
            ensure!(capture.is_none(), "--capture specified twice");
            capture = Some(PathBuf::from(
                args.next().context("--capture needs a path")?,
            ));
        } else {
            bail!("unknown argument; {USAGE}");
        }
    }
    Ok(Some(ServerConfig {
        state_dir: match state_dir {
            Some(path) => path,
            None => default_state_dir()?,
        },
        output: match capture {
            Some(path) => OutputConfig::Capture {
                path,
                clocked: true,
            },
            None => OutputConfig::CoreAudio,
        },
    }))
}

async fn run() -> Result<()> {
    let Some(config) = options()? else {
        return Ok(());
    };
    // Register both handlers before starting the owner, including the ready-line
    // interval, so neither signal bypasses checkpointing once startup completes.
    let mut interrupt = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    let mut terminate = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let server = Server::start(config).await?;
    let ready = Ready {
        kind: "ready",
        socket_path: server.socket_path(),
        instance_id: server.instance_id(),
    };
    let ready_result = (|| -> Result<()> {
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &ready)?;
        writeln!(stdout)?;
        stdout.flush()?;
        Ok(())
    })();
    if let Err(error) = ready_result {
        server
            .run(async {})
            .await
            .context("shutdown after ready output failure")?;
        return Err(error).context("write server ready line");
    }
    server
        .run(async {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
        })
        .await
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tyranny-likes-smoke-server: {error:#}");
            ExitCode::FAILURE
        }
    }
}
