use anyhow::{Context, Result, bail, ensure};
use std::{
    env,
    io::{self, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
};
use tokio::signal::unix::{SignalKind, signal};
use tyranny::{
    output::OutputConfig,
    server::{Server, ServerConfig, default_state_dir},
};

const USAGE: &str = "tyranny-server [--state-dir PATH] [--bind 127.0.0.1:9865] [--capture PATH]\n\
Default output is the real CoreAudio device; --capture writes clocked stereo f32 PCM.\n\
Only literal loopback bind addresses are accepted. The ready JSON never contains the token.";

#[derive(serde::Serialize)]
struct Ready<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    address: SocketAddr,
    instance_id: &'a str,
    token_path: &'a Path,
}

fn options() -> Result<Option<ServerConfig>> {
    let mut args = env::args_os().skip(1);
    let (mut state_dir, mut bind, mut capture) = (None, None, None);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("{USAGE}");
            return Ok(None);
        } else if arg == "--state-dir" {
            ensure!(state_dir.is_none(), "--state-dir specified twice");
            state_dir = Some(PathBuf::from(
                args.next().context("--state-dir needs a path")?,
            ));
        } else if arg == "--bind" {
            ensure!(bind.is_none(), "--bind specified twice");
            let value = args.next().context("--bind needs a socket address")?;
            bind = Some(
                value
                    .to_str()
                    .context("--bind must be UTF-8")?
                    .parse::<SocketAddr>()
                    .context("invalid bind socket address")?,
            );
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
        bind: bind.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 9865))),
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
        address: server.address(),
        instance_id: server.instance_id(),
        token_path: server.token_path(),
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
            eprintln!("tyranny-server: {error:#}");
            ExitCode::FAILURE
        }
    }
}
