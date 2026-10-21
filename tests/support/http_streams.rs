use super::*;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream as HttpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Instant,
};

/// A real HTTP source, including ICY framing, a transient 503, and a stalled body.
struct Radio {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    changed: Arc<AtomicBool>,
    stalled: Arc<AtomicBool>,
    attempts: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Radio {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let changed = Arc::new(AtomicBool::new(false));
        let stalled = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let (halt, title, count) = (stop.clone(), changed.clone(), attempts.clone());
        let stall = stalled.clone();
        let thread = thread::spawn(move || {
            let mut clients = Vec::new();
            while !halt.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let (halt, title, count) = (halt.clone(), title.clone(), count.clone());
                        let stall = stall.clone();
                        clients.push(thread::spawn(move || {
                            let _ = serve(socket, halt, title, count, stall);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("HTTP fixture accept: {error}"),
                }
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        Self {
            address,
            stop,
            changed,
            stalled,
            attempts,
            thread: Some(thread),
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}
impl Drop for Radio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn serve(
    mut socket: HttpSocket,
    stop: Arc<AtomicBool>,
    changed: Arc<AtomicBool>,
    attempts: Arc<AtomicUsize>,
    stalled: Arc<AtomicBool>,
) -> std::io::Result<()> {
    // Darwin can inherit nonblocking from the listener.
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    socket.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut request = Vec::new();
    let mut byte = [0];
    while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
        socket.read_exact(&mut byte)?;
        request.push(byte[0]);
    }
    let request = String::from_utf8_lossy(&request);
    if request.starts_with("GET /stall ") {
        socket.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n\r\n",
        )?;
        while !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(20));
        }
        return Ok(());
    }
    if request.starts_with("GET /ogg ") {
        socket.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: audio/ogg\r\nConnection: close\r\n\r\n",
        )?;
        for block in include_bytes!("../fixtures/streams/live.ogg").chunks(2048) {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            socket.write_all(block)?;
            thread::sleep(Duration::from_millis(10));
        }
        while !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(20));
        }
        return Ok(());
    }
    let attempt = attempts.fetch_add(1, Ordering::AcqRel);
    if attempt == 0 {
        socket.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return Ok(());
    }
    assert!(
        request.to_ascii_lowercase().contains("icy-metadata: 1"),
        "client did not request ICY metadata"
    );
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nicy-name: Fixture Radio\r\nicy-metaint: 1024\r\nConnection: close\r\n\r\n")?;
    let audio = include_bytes!("../fixtures/streams/live.mp3");
    let mut position = 0;
    while !stop.load(Ordering::Acquire) {
        let mut remaining = 1024;
        while remaining > 0 {
            let count = remaining.min(audio.len() - position);
            socket.write_all(&audio[position..position + count])?;
            position = (position + count) % audio.len();
            remaining -= count;
        }
        let title = if attempt > 1 {
            "Recovered Live Title"
        } else if changed.load(Ordering::Acquire) {
            "Second Live Title"
        } else {
            "First Live Title"
        };
        let text = format!("StreamTitle='{title}';");
        let blocks = text.len().div_ceil(16);
        let mut metadata = vec![0; blocks * 16 + 1];
        metadata[0] = blocks as u8;
        metadata[1..text.len() + 1].copy_from_slice(text.as_bytes());
        socket.write_all(&metadata)?;
        if attempt == 1 && stalled.load(Ordering::Acquire) {
            // Let the established stream hit its read deadline while controls
            // remain usable; the reconnect must keep the same queue occurrence.
            while !stop.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(20));
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(80));
    }
    Ok(())
}

async fn command(
    socket: &mut Socket,
    id: u64,
    command: &str,
    fields: serde_json::Value,
) -> serde_json::Value {
    let mut request = fields.as_object().unwrap().clone();
    request.insert("id".into(), id.into());
    request.insert("command".into(), command.into());
    socket
        .send(serde_json::Value::Object(request).to_string())
        .await
        .unwrap();
    loop {
        if let Message::Reply {
            id: reply_id,
            reply,
        } = receive(socket).await
        {
            assert_eq!(reply_id, id);
            return serde_json::to_value(reply).unwrap();
        }
    }
}
async fn state(socket: &mut Socket, id: u64) -> serde_json::Value {
    command(socket, id, "snapshot", serde_json::json!({})).await["result"]["snapshot"].clone()
}
async fn wait_title(socket: &mut Socket, title: &str) -> ControlState {
    timeout(Duration::from_secs(20), async {
        loop {
            if let Message::State { snapshot } = receive(socket).await {
                assert_ne!(snapshot.transport, Transport::Error, "{:?}", snapshot.error);
                if snapshot.transport == Transport::Playing
                    && snapshot
                        .now_playing
                        .as_ref()
                        .is_some_and(|track| track.title == title)
                {
                    return snapshot;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("live title {title:?} did not arrive"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_streams_recover_publish_icy_persist_and_keep_controls_responsive() {
    let scratch = Scratch::new();
    let radio = Radio::start();
    let running = Running::start(&scratch).await;
    let mut client = running.connect().await;
    let initial = hello(&mut client, &running.instance_id).await;
    let appended = command(&mut client, 1, "append_stream", serde_json::json!({
        "expected_revision": initial.queue_revision, "url": radio.url("/icy"), "title": "Fixture Radio"
    })).await;
    assert_eq!(appended["status"], "ok", "{appended}");
    let entry = appended["result"]["entry_id"].as_u64().unwrap();
    assert_eq!(
        command(
            &mut client,
            2,
            "play",
            serde_json::json!({"entry_id":entry})
        )
        .await["status"],
        "ok"
    );
    let first = wait_title(&mut client, "First Live Title").await;
    assert_eq!(first.current_id, Some(entry));
    let first_now = first.now_playing.unwrap();
    assert_eq!(first_now.duration_frames, None);
    assert_eq!(first_now.station.as_deref(), Some("Fixture Radio"));
    assert!(
        radio.attempts.load(Ordering::Acquire) >= 2,
        "503 was not retried"
    );
    radio.changed.store(true, Ordering::Release);
    let second = wait_title(&mut client, "Second Live Title").await;
    assert_eq!(
        second.current_id,
        Some(entry),
        "ICY title replaced queue identity"
    );
    assert!(second.position_frames > first.position_frames);
    radio.stalled.store(true, Ordering::Release);
    let recovered = timeout(Duration::from_secs(15), async {
        let mut id = 1000;
        loop {
            let snapshot = state(&mut client, id).await;
            assert_ne!(snapshot["transport"], "error");
            if snapshot["now_playing"]["title"] == "Recovered Live Title" {
                break snapshot;
            }
            id += 1;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("established stream did not recover from its read deadline");
    assert_eq!(recovered["current_id"], entry);
    assert!(radio.attempts.load(Ordering::Acquire) >= 3);
    let resumed_position = recovered["position_frames"].as_u64().unwrap();
    timeout(DEADLINE, async {
        loop {
            if let Message::State { snapshot } = receive(&mut client).await
                && snapshot.current_id == Some(entry)
                && snapshot.position_frames >= resumed_position + 24_000
            {
                break;
            }
        }
    })
    .await
    .expect("stream did not render half a second");
    let seek = command(
        &mut client,
        3,
        "seek",
        serde_json::json!({"entry_id":entry,"position_frames":0}),
    )
    .await;
    assert_eq!(seek["status"], "error", "live seek must be rejected");
    assert_eq!(
        command(&mut client, 4, "pause", serde_json::json!({})).await["status"],
        "ok"
    );
    assert_eq!(state(&mut client, 5).await["transport"], "paused");
    drop(client);
    running.stop().await;
    let pcm = fs::read(scratch.path("capture.f32")).unwrap();
    let samples: Vec<f32> = pcm
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    assert!(
        samples.len() >= 48_000,
        "stream failed to render half a second of stereo PCM"
    );
    let power = samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / samples.len() as f64;
    assert!(power > 0.0001 && power < 0.02, "decoded tone power {power}");

    let running = Running::start(&scratch).await;
    let mut client = running.connect().await;
    let restored = hello(&mut client, &running.instance_id).await;
    assert_eq!(restored.transport, Transport::Paused);
    let page = command(
        &mut client,
        6,
        "queue_page",
        serde_json::json!({"revision":restored.queue_revision,"offset":0,"limit":10}),
    )
    .await;
    assert_eq!(
        page["result"]["entries"][0]["stream"]["url"],
        radio.url("/icy")
    );
    assert_eq!(
        page["result"]["entries"][0]["stream"]["title"],
        "Fixture Radio"
    );

    let appended = command(&mut client, 7, "append_stream", serde_json::json!({
        "expected_revision": restored.queue_revision, "url": radio.url("/stall"), "title":"Stalled Radio"
    })).await;
    let stalled = appended["result"]["entry_id"].as_u64().unwrap();
    let start = Instant::now();
    assert_eq!(
        command(
            &mut client,
            8,
            "play",
            serde_json::json!({"entry_id":stalled})
        )
        .await["status"],
        "ok"
    );
    assert_eq!(state(&mut client, 9).await["transport"], "buffering");
    assert_eq!(
        command(&mut client, 10, "pause", serde_json::json!({})).await["status"],
        "ok"
    );
    assert_eq!(
        command(&mut client, 11, "stop", serde_json::json!({})).await["status"],
        "ok"
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "HTTP body stall blocked commands"
    );
    let current = state(&mut client, 12).await;
    let appended = command(&mut client, 13, "append_stream", serde_json::json!({
        "expected_revision":current["queue_revision"],"url":radio.url("/ogg"),"title":"Ogg Radio"
    })).await;
    let ogg = appended["result"]["entry_id"].as_u64().unwrap();
    assert_eq!(
        command(&mut client, 14, "play", serde_json::json!({"entry_id":ogg})).await["status"],
        "ok"
    );
    let playing = wait_title(&mut client, "Ogg Radio").await;
    assert_eq!(playing.current_id, Some(ogg));
    assert_eq!(playing.now_playing.unwrap().duration_frames, None);
    assert_eq!(
        command(&mut client, 15, "stop", serde_json::json!({})).await["status"],
        "ok"
    );
    drop(client);
    running.stop().await;
}
