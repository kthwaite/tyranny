use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message as Frame, protocol::WebSocketConfig},
};
use tyranny::{
    protocol::{Message, Reply, Request, Response, VERSION},
    server::{
        MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, client_request, default_state_dir, read_token,
    },
};

const USAGE: &str = "tyrannyctl [--url ws://127.0.0.1:9865/ws] [--token-file PATH] [--watch] JSON_REQUEST\n\
Example: tyrannyctl '{\"id\":1,\"command\":\"snapshot\"}'\n\
Transport: play, pause, toggle_play_pause, stop, next, previous.\n\
Live stream (protocol 10): {\"id\":2,\"command\":\"append_stream\",\"expected_revision\":1,\"url\":\"https://radio.example/live.mp3\",\"title\":\"Radio\"}\n\
Append does not play; play the returned entry_id separately. Streams cannot seek; blank titles default to the host.\n\
Seek requires the current occurrence: {\"id\":2,\"command\":\"seek\",\"entry_id\":1,\"position_frames\":48000}\n\
Artwork (protocol 8, read-only): {\"id\":3,\"command\":\"artwork\",\"album_id\":1,\"entry_id\":null,\"library_revision\":2,\"size\":\"thumbnail\"}\n\
Use exactly one album_id or durable queue entry_id; size is thumbnail (128px) or display (512px). Ready data is base64 JPEG.\n\
Saved playlists: saved_playlists, playlist_entries, create_playlist, rename_playlist, delete_playlist,\n\
playlist_add, playlist_remove, playlist_move, enqueue_playlist. Names are unique ignoring Unicode case.\n\
Create: {\"id\":3,\"command\":\"create_playlist\",\"expected_revision\":1,\"name\":\"Favorites\"}\n\
Browse: {\"id\":4,\"command\":\"saved_playlists\",\"revision\":null,\"offset\":0,\"limit\":100,\"query\":\"\"}\n\
Entries: {\"id\":5,\"command\":\"playlist_entries\",\"playlist_id\":1,\"revision\":null,\"library_revision\":null,\"offset\":0,\"limit\":100,\"query\":\"\"}\n\
Add: {\"id\":6,\"command\":\"playlist_add\",\"playlist_id\":1,\"expected_revision\":1,\"library_revision\":2,\"track_ids\":[1,1],\"before\":null}\n\
Enqueue/play: {\"id\":7,\"command\":\"enqueue_playlist\",\"playlist_id\":1,\"playlist_revision\":2,\"library_revision\":2,\"expected_revision\":1,\"play\":true}\n\
Smart: create_smart_playlist takes expected_revision (catalog), name and query; update_smart_playlist takes playlist_id, expected_revision (playlist) and query.\n\
Smart results are live and read-only. saved_playlists accepts optional kind (regular or smart); smart entry_count is null.\n\
snapshot_smart_playlist takes playlist_id, playlist_revision, library_revision, expected_revision (catalog) and name; it copies ALL matches into an editable regular playlist.\n\
Enqueue also snapshots ALL rule results, independent of the temporary entry filter; later rule/library changes never rewrite the queue.\n\
Create binds snapshot.playlists_revision; edits bind the playlist revision; enqueue also binds queue/library revisions.\n\
Entry pages after offset zero bind both returned revisions. Playlist/queue occurrence IDs are distinct; duplicate tracks are allowed.\n\
Protocol 5 batch reorder: move/playlist_move use entry_ids (1..500 distinct occurrences) and before (null=end).\n\
Queue copy: insert_tracks binds expected_revision/library_revision, track_ids and before; playback is unchanged.\n\
Moves preserve source order; inserts preserve track_ids order, including duplicates. Selected anchors are no-ops.\n\
Saved radio stations (protocol 12): stations, add_stations, update_station, remove_station. Edits bind snapshot.stations_revision.\n\
Add: {\"id\":8,\"command\":\"add_stations\",\"expected_revision\":0,\"stations\":[{\"name\":\"\",\"url\":\"https://radio.example/live.mp3\"}]}\n\
Station directory (protocol 16): search_directory, count_directory_click.\n\
Search: {\"id\":9,\"command\":\"search_directory\",\"query\":\"jazz\"} (query 1..100 bytes, no control characters)\n\
Count: {\"id\":10,\"command\":\"count_directory_click\",\"station_uuid\":\"<uuid>\"} (replies at once; the click is best effort)\n\
Listening stats (protocol 18): stats {period:{kind,anchor}}, wrapped {year}, stats_years.\n\
Parametric EQ: set_equalizer {equalizer:{enabled,preamp_db,bands}} replaces the complete curve (0..16 bands).\n\
Each band requires enabled,kind,frequency_hz,gain_db,q,shelf_slope,slope_db; kinds: bell,low_shelf,high_shelf,high_pass,low_pass,notch.\n\
Named EQ presets: save_equalizer_preset {name,equalizer}, rename_equalizer_preset {name,new_name}, remove_equalizer_preset {name}.\n\
Preset edits persist without recalling; recall with set_equalizer while preserving your current enabled choice.\n\
Control --url must use a literal loopback IP; stream URLs may use remote HTTP(S) hosts. --watch prints state events until interrupted.\n\
Commands are never automatically retried, including when a reply is lost.";
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

struct Options {
    url: String,
    token_file: PathBuf,
    watch: bool,
    request_id: u64,
    json: String,
}

fn options() -> Result<Option<Options>> {
    let mut args = env::args_os().skip(1);
    let (mut url, mut token_file, mut json) = (None, None, None);
    let mut watch = false;
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("{USAGE}");
            return Ok(None);
        } else if arg == "--url" {
            ensure!(url.is_none(), "--url specified twice");
            url = Some(
                args.next()
                    .context("--url needs a value")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("URL must be UTF-8"))?,
            );
        } else if arg == "--token-file" {
            ensure!(token_file.is_none(), "--token-file specified twice");
            token_file = Some(PathBuf::from(
                args.next().context("--token-file needs a path")?,
            ));
        } else if arg == "--watch" {
            ensure!(!watch, "--watch specified twice");
            watch = true;
        } else {
            ensure!(json.is_none(), "supply exactly one JSON request; {USAGE}");
            json = Some(
                arg.into_string()
                    .map_err(|_| anyhow::anyhow!("JSON request must be UTF-8"))?,
            );
        }
    }
    let json = json.context("missing JSON request; use --help for usage")?;
    ensure!(json.len() <= MAX_REQUEST_BYTES, "request exceeds 16 KiB");
    let request: Request = serde_json::from_str(&json).context("invalid request JSON")?;
    Ok(Some(Options {
        url: url.unwrap_or_else(|| "ws://127.0.0.1:9865/ws".to_owned()),
        token_file: match token_file {
            Some(path) => path,
            None => default_state_dir()?.join("token"),
        },
        watch,
        request_id: request.id,
        json,
    }))
}

fn print_frame(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{text}")?;
    stdout.flush()?;
    Ok(())
}

async fn run() -> Result<()> {
    let Some(options) = options()? else {
        return Ok(());
    };
    let token = read_token(&options.token_file)?;
    let upgrade = client_request(&options.url, &token)?;
    let configuration = WebSocketConfig::default()
        .read_buffer_size(MAX_REQUEST_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_REQUEST_BYTES + 1024)
        .max_message_size(Some(MAX_RESPONSE_BYTES))
        .max_frame_size(Some(MAX_RESPONSE_BYTES));
    let (mut socket, _) = timeout(
        IO_TIMEOUT,
        connect_async_with_config(upgrade, Some(configuration), true),
    )
    .await
    .context("WebSocket connection timed out")?
    .map_err(|_| anyhow::anyhow!("WebSocket connection or authentication failed"))?;
    let hello = timeout(IO_TIMEOUT, socket.next())
        .await
        .context("initial Hello timed out")?
        .context("server disconnected before Hello")?
        .map_err(|_| anyhow::anyhow!("invalid WebSocket handshake response"))?;
    let Frame::Text(text) = hello else {
        bail!("server did not send Hello first");
    };
    let message: Message = serde_json::from_str(&text).context("invalid Hello JSON")?;
    let Message::Hello {
        protocol_version,
        instance_id,
        snapshot,
    } = message
    else {
        bail!("server did not send Hello first");
    };
    ensure!(
        protocol_version == VERSION,
        "unsupported server protocol version {protocol_version}"
    );
    ensure!(
        uuid::Uuid::parse_str(&instance_id).is_ok(),
        "invalid server instance ID"
    );
    let mut sequence = snapshot.sequence;
    print_frame(&text)?;
    timeout(IO_TIMEOUT, socket.send(Frame::Text(options.json.into())))
        .await
        .context("request send timed out; outcome uncertain, do not automatically retry")?
        .map_err(|_| {
            anyhow::anyhow!("request send failed; outcome uncertain, do not automatically retry")
        })?;
    let deadline = Instant::now() + REPLY_TIMEOUT;
    let mut replied = false;
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    loop {
        let incoming = tokio::select! {
            signal = &mut interrupted => {
                signal.context("receive interrupt")?;
                let _ = timeout(IO_TIMEOUT, socket.close(None)).await;
                ensure!(replied, "interrupted before reply; outcome uncertain, do not automatically retry");
                return Ok(());
            }
            incoming = async {
                if replied {
                    Ok(socket.next().await)
                } else {
                    timeout_at(deadline, socket.next()).await
                }
            } => incoming.context("reply timed out; outcome uncertain, do not automatically retry")?,
        };
        let frame = incoming
            .context("server disconnected; no automatic command retry")?
            .map_err(|_| {
                anyhow::anyhow!(
                    "WebSocket protocol or connection failure; no automatic command retry"
                )
            })?;
        match frame {
            Frame::Text(text) => {
                let message: Message =
                    serde_json::from_str(&text).context("invalid server message JSON")?;
                match message {
                    Message::Hello { .. } => bail!("unexpected second Hello"),
                    Message::State { snapshot } => {
                        ensure!(
                            snapshot.sequence > sequence,
                            "server state sequence did not advance"
                        );
                        sequence = snapshot.sequence;
                        if options.watch {
                            print_frame(&text)?;
                        }
                    }
                    Message::Reply { id, reply } => {
                        ensure!(
                            !replied && id == options.request_id,
                            "unexpected reply ID or duplicate reply"
                        );
                        if let Reply::Ok {
                            result:
                                Response::Snapshot { snapshot }
                                | Response::Applied { snapshot, .. }
                                | Response::PlaylistApplied { snapshot, .. }
                                | Response::StationsApplied { snapshot, .. },
                        } = &reply
                        {
                            ensure!(
                                snapshot.sequence >= sequence,
                                "reply moved state sequence backwards"
                            );
                            sequence = snapshot.sequence;
                        }
                        print_frame(&text)?;
                        replied = true;
                        if matches!(reply, Reply::Error { .. }) {
                            let _ = timeout(IO_TIMEOUT, socket.close(None)).await;
                            bail!("server rejected the command (see reply)");
                        }
                        if !options.watch {
                            let _ = timeout(IO_TIMEOUT, socket.close(None)).await;
                            return Ok(());
                        }
                    }
                }
            }
            Frame::Ping(_) => {
                timeout(IO_TIMEOUT, socket.flush())
                    .await
                    .context("pong timed out")?
                    .map_err(|_| anyhow::anyhow!("pong failed"))?;
            }
            Frame::Pong(_) => {}
            Frame::Close(_) => bail!("server closed the connection; no automatic command retry"),
            _ => bail!("server sent a non-text protocol message"),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tyrannyctl: {error:#}");
            ExitCode::FAILURE
        }
    }
}
