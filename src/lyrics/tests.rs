//! Real FFmpeg-tagged fixtures and loopback HTTP, never the public lyrics service.
use super::*;
use crate::store::Store;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    process::Command,
};

struct Fixture {
    root: PathBuf,
    database: PathBuf,
    song: PathBuf,
}
impl Fixture {
    fn new(extension: &str, embedded: Option<&str>) -> Self {
        let root = std::env::temp_dir().join(format!("tyranny-lyrics-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let database = root.join("state.sqlite");
        drop(Store::open(&database).unwrap());
        let song = root.join(format!("song.{extension}"));
        tagged(&song, embedded);
        let db = Connection::open(&database).unwrap();
        db.execute(
            "INSERT INTO queue(id,ordinal,path) VALUES(1,0,?1)",
            [song.to_str().unwrap()],
        )
        .unwrap();
        db.execute("UPDATE control SET current_id=1,next_id=2", [])
            .unwrap();
        Self {
            root,
            database,
            song,
        }
    }
    fn engine(&self, provider: std::result::Result<Provider, String>) -> Engine {
        Engine::open(&self.database, provider, Arc::new(Mutex::new(()))).unwrap()
    }
    fn mutate(&self, sql: &str) {
        Connection::open(&self.database)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn tagged(path: &Path, embedded: Option<&str>) {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=48000:cl=stereo",
        "-t",
        "2",
        "-metadata",
        "title=Fixture Title",
        "-metadata",
        "artist=Fixture Artist",
        "-metadata",
        "album=Fixture Album",
    ]);
    let mp3 = path.extension().is_some_and(|extension| extension == "mp3");
    if mp3 {
        command.args(["-id3v2_version", "4"]);
    } else if let Some(text) = embedded {
        command.args(["-metadata", &format!("lyrics={text}")]);
    }
    let output = command.arg(path).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if mp3 && let Some(text) = embedded {
        insert_uslt(path, text);
    }
}
fn insert_uslt(path: &Path, text: &str) {
    fn synchsafe(size: usize) -> [u8; 4] {
        assert!(size < 1 << 28);
        [
            (size >> 21) as u8 & 0x7f,
            (size >> 14) as u8 & 0x7f,
            (size >> 7) as u8 & 0x7f,
            size as u8 & 0x7f,
        ]
    }
    let mut bytes = fs::read(path).unwrap();
    assert!(bytes.len() >= 10 && &bytes[..3] == b"ID3" && bytes[3] == 4 && bytes[5] == 0);
    let tag_size = bytes[6..10].iter().fold(0usize, |size, byte| {
        assert!(byte & 0x80 == 0);
        (size << 7) | usize::from(*byte)
    });
    assert!(10 + tag_size <= bytes.len());
    // FFmpeg's generic lyrics tag writes TXXX; construct an actual USLT frame:
    // UTF-8 encoding, ISO-639 language, empty descriptor, then unsynced text.
    let payload_size = 5 + text.len();
    let mut frame = Vec::with_capacity(10 + payload_size);
    frame.extend_from_slice(b"USLT");
    frame.extend_from_slice(&synchsafe(payload_size));
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(b"\x03eng\0");
    frame.extend_from_slice(text.as_bytes());
    bytes[6..10].copy_from_slice(&synchsafe(tag_size + frame.len()));
    drop(bytes.splice(10..10, frame));
    assert_eq!(&bytes[10..14], b"USLT");
    fs::write(path, bytes).unwrap();
}
fn request() -> Request {
    Request {
        entry_id: 1,
        library_revision: 1,
    }
}
fn lyrics(reply: Reply) -> Lyrics {
    match reply {
        Reply::Ok {
            result: Response::Lyrics { lyrics },
        } => lyrics,
        other => panic!("unexpected reply: {other:?}"),
    }
}
fn fixture_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn record() -> String {
    serde_json::json!({"id":12,"trackName":"Fixture Title","artistName":"Fixture Artist","albumName":"Fixture Album","duration":2.0,"instrumental":false,"plainLyrics":"Provider words","syncedLyrics":"[00:00.50]Provider words"}).to_string()
}
fn serve(response: String) -> (Provider, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let provider =
        Provider::for_test(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let thread = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = socket.read(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..n]);
        }
        let _ = socket.write_all(response.as_bytes());
        String::from_utf8(request).unwrap()
    });
    (provider, thread)
}
#[test]
fn embedded_uslt_and_lyrics_precede_provider_and_invalidate_on_file_change() {
    for extension in ["mp3", "flac"] {
        let fixture = Fixture::new(extension, Some("Embedded first"));
        let mut engine = fixture.engine(Err("provider must not run".into()));
        let first = lyrics(engine.request(request(), &Cancellation::new()));
        assert_eq!(first.source, Some(LyricsSource::Embedded));
        assert_eq!(first.plain_text.as_deref(), Some("Embedded first"));
        tagged(&fixture.song, Some("[00:00.20]Updated synchronized words"));
        let updated = lyrics(engine.request(request(), &Cancellation::new()));
        assert_eq!(updated.source, Some(LyricsSource::Embedded));
        assert_eq!(
            updated.synced_lrc.as_deref(),
            Some("[00:00.20]Updated synchronized words")
        );
        fixture.mutate("UPDATE library_state SET revision=2");
        assert!(matches!(
            engine.request(request(), &Cancellation::new()),
            Reply::Error {
                code: ErrorCode::StaleLibraryRevision,
                ..
            }
        ));
        let updated = lyrics(engine.request(
            Request {
                library_revision: 2,
                ..request()
            },
            &Cancellation::new(),
        ));
        assert_eq!(updated.source, Some(LyricsSource::Embedded));
    }
}
#[test]
fn provider_results_persist_offline_without_confusing_missing_and_errors() {
    for (status, body, expected) in [
        ("200 OK", record(), LyricsStatus::Ready),
        ("404 Not Found", String::new(), LyricsStatus::Missing),
        (
            "500 Internal Server Error",
            String::new(),
            LyricsStatus::Unavailable,
        ),
    ] {
        let fixture = Fixture::new("flac", None);
        let (provider, server) = serve(fixture_response(status, &body));
        let mut engine = fixture.engine(Ok(provider));
        let first = lyrics(engine.request(request(), &Cancellation::new()));
        assert_eq!(first.status, expected);
        let request_text = server.join().unwrap();
        assert!(request_text.contains("track_name=Fixture+Title&artist_name=Fixture+Artist&album_name=Fixture+Album&duration=2"));
        drop(engine);
        let mut offline = fixture.engine(Err("offline provider must not run".into()));
        assert_eq!(
            lyrics(offline.request(request(), &Cancellation::new())),
            first
        );
        // Expired online results cannot be mistaken for persistent negative hits.
        if expected != LyricsStatus::Ready {
            Connection::open(fixture.database.with_extension("lyrics.sqlite"))
                .unwrap()
                .execute(
                    "UPDATE lyrics_cache SET expires=0 WHERE key LIKE 'online:%'",
                    [],
                )
                .unwrap();
            assert_eq!(
                lyrics(offline.request(request(), &Cancellation::new()))
                    .message
                    .as_deref(),
                Some("offline provider must not run")
            );
        }
        // A newly embedded tag wins even with a successful remote cache entry.
        tagged(&fixture.song, Some("New local lyrics"));
        assert_eq!(
            lyrics(offline.request(request(), &Cancellation::new())).source,
            Some(LyricsSource::Embedded)
        );
    }
}
#[test]
fn online_identity_survives_rescans_and_duplicate_paths_but_embedded_still_wins() {
    let fixture = Fixture::new("flac", None);
    let (provider, server) = serve(fixture_response("200 OK", &record()));
    let mut engine = fixture.engine(Ok(provider));
    let first = lyrics(engine.request(request(), &Cancellation::new()));
    assert_eq!(first.source, Some(LyricsSource::Lrclib));
    server.join().unwrap();
    drop(engine);
    fixture.mutate(
        "UPDATE library_state SET revision=2;
        INSERT INTO library_roots(path,search,completed_scan) VALUES('/unrelated','/unrelated',42)",
    );
    let current = Request {
        library_revision: 2,
        ..request()
    };
    let mut offline = fixture.engine(Err("offline provider must not run".into()));
    assert_eq!(
        lyrics(offline.request(current, &Cancellation::new())),
        first
    );
    let duplicate = fixture.root.join("duplicate.flac");
    fs::copy(&fixture.song, &duplicate).unwrap();
    Connection::open(&fixture.database)
        .unwrap()
        .execute(
            "INSERT INTO queue(id,ordinal,path) VALUES(2,1,?1)",
            [duplicate.to_str().unwrap()],
        )
        .unwrap();
    assert_eq!(
        lyrics(offline.request(
            Request {
                entry_id: 2,
                ..current
            },
            &Cancellation::new()
        )),
        first
    );
    tagged(&fixture.song, Some("Embedded after rescan"));
    let updated = lyrics(offline.request(current, &Cancellation::new()));
    assert_eq!(updated.source, Some(LyricsSource::Embedded));
    assert_eq!(updated.plain_text.as_deref(), Some("Embedded after rescan"));
}
#[test]
fn invalid_stale_stream_and_nonregular_targets_never_fetch() {
    let fixture = Fixture::new("flac", None);
    let mut engine = fixture.engine(Err("network forbidden".into()));
    for id in [0, 2, u64::MAX] {
        assert!(matches!(
            engine.request(
                Request {
                    entry_id: id,
                    ..request()
                },
                &Cancellation::new()
            ),
            Reply::Error {
                code: ErrorCode::InvalidCommand,
                ..
            }
        ));
    }
    assert!(matches!(
        engine.request(
            Request {
                library_revision: 0,
                ..request()
            },
            &Cancellation::new()
        ),
        Reply::Error {
            code: ErrorCode::StaleLibraryRevision,
            ..
        }
    ));
    fixture.mutate(
        "UPDATE queue SET stream_title='Station',path='https://radio.invalid/live' WHERE id=1",
    );
    assert_eq!(
        lyrics(engine.request(request(), &Cancellation::new())).status,
        LyricsStatus::Missing
    );
    fixture.mutate("UPDATE control SET current_id=NULL");
    assert_eq!(
        lyrics(engine.request(request(), &Cancellation::new())).status,
        LyricsStatus::Missing
    );
    fixture.mutate(
        "UPDATE control SET current_id=1; UPDATE queue SET stream_title=NULL,path='/' WHERE id=1",
    );
    assert_eq!(
        lyrics(engine.request(request(), &Cancellation::new())).status,
        LyricsStatus::Unavailable
    );
}
#[test]
fn worker_revalidates_queue_occurrence_after_slow_io() {
    let fixture = Fixture::new("flac", None);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let provider =
        Provider::for_test(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let (started, receive) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut buffer = [0; 4096];
        let _ = socket.read(&mut buffer).unwrap();
        started.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        let _ = socket.write_all(fixture_response("200 OK", &record()).as_bytes());
    });
    let workers =
        LyricsWorkers::with_provider(fixture.database.clone(), OnceLock::from(Ok(provider)));
    let handle = workers.handle();
    let request_thread = thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle.request(request()))
    });
    receive.recv_timeout(Duration::from_secs(5)).unwrap();
    fixture.mutate("UPDATE control SET current_id=NULL; DELETE FROM queue WHERE id=1");
    release.send(()).unwrap();
    assert!(matches!(
        request_thread.join().unwrap(),
        Reply::Error {
            code: ErrorCode::InvalidCommand,
            ..
        }
    ));
    server.join().unwrap();
    workers.shutdown();
}
#[test]
fn cancelled_work_skips_discovery_and_stopped_workers_reply_promptly() {
    let fixture = Fixture::new("flac", Some("Words"));
    let workers = LyricsWorkers::with_provider(
        fixture.database.clone(),
        OnceLock::from(Err("network forbidden".into())),
    );
    let handle = workers.handle();
    workers.stop();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(handle.request(request()));
    assert_eq!(lyrics(result).status, LyricsStatus::Unavailable);
    workers.shutdown();
    let cancel = Cancellation::new();
    cancel.abandoned.store(true, Ordering::Release);
    assert!(discovery::probe(&fixture.song, &cancel, Instant::now() + DISCOVERY_TIMEOUT).is_err());
}
