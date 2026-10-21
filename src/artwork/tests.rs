//! Exercise artwork through its bounded worker/request seam with real files,
//! actual library scans and a loopback HTTP provider, never mocked discovery.
use super::*;
use crate::{
    library::Library,
    library_types::{LibraryCommand, LibraryReply, PageRequest, ScanPhase},
    protocol::{ArtworkSource, ArtworkStatus},
    store::Store,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    process::Command,
    sync::atomic::AtomicUsize,
};

const RELEASE: &str = "76df3287-6cda-33eb-8e9a-044b5e15ffdd";
struct Fixture {
    root: PathBuf,
    database: PathBuf,
    library: Option<Library>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("tyranny-artwork-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("music")).unwrap();
        let database = root.join("state.sqlite");
        Store::open(&database).unwrap();
        let library = Some(Library::start(database.clone()).unwrap());
        Self {
            root,
            database,
            library,
        }
    }
    fn music(&self, name: &str) -> PathBuf {
        self.root.join("music").join(name)
    }
    fn workers(&self) -> ArtworkWorkers {
        ArtworkWorkers::start(self.database.clone())
    }
    fn provider_workers(&self, provider: Provider) -> ArtworkWorkers {
        ArtworkWorkers::with_provider(self.database.clone(), OnceLock::from(Ok(provider)))
    }
    async fn scan(&self) -> (u64, u64, u64) {
        let library = self.library.as_ref().unwrap();
        let root = match library
            .handle()
            .request(LibraryCommand::AddRoot {
                path: self.music("").to_str().unwrap().into(),
            })
            .await
            .unwrap()
        {
            LibraryReply::RootAdded(root) => root.id,
            other => panic!("unexpected response {other:?}"),
        };
        self.finished().await;
        let (album, revision) = self.album().await;
        (root, album, revision)
    }
    async fn rescan(&self, root: u64) {
        let library = self.library.as_ref().unwrap();
        library
            .handle()
            .request(LibraryCommand::Rescan { root_id: root })
            .await
            .unwrap();
        self.finished().await;
    }
    async fn finished(&self) {
        let library = self.library.as_ref().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = library.state();
            if !matches!(
                state.scan.phase,
                ScanPhase::Discovering | ScanPhase::Indexing
            ) {
                assert_eq!(state.scan.phase, ScanPhase::Complete);
                break;
            }
            assert!(Instant::now() < deadline, "scan deadline exceeded");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    async fn album(&self) -> (u64, u64) {
        match self
            .library
            .as_ref()
            .unwrap()
            .handle()
            .request(LibraryCommand::Albums {
                artist_id: None,
                page: PageRequest {
                    revision: None,
                    offset: 0,
                    limit: 100,
                    query: String::new(),
                },
            })
            .await
            .unwrap()
        {
            LibraryReply::Albums(page) => {
                assert_eq!(page.entries.len(), 1);
                (page.entries[0].id, page.revision)
            }
            other => panic!("unexpected response {other:?}"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(library) = self.library.take() {
            library.shutdown().unwrap();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn wav(path: &Path) {
    let frames = 48u32;
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
fn png(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb(color));
    let mut output = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut output, image::ImageFormat::Png)
        .unwrap();
    output.into_inner()
}
fn tagged(path: &Path, cover: Option<&Path>, release: bool) {
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
    ]);
    if let Some(cover) = cover {
        command.arg("-i").arg(cover);
    }
    command.args(["-t", "0.01", "-map", "0:a", "-c:a", "flac"]);
    if cover.is_some() {
        command.args([
            "-map",
            "1:v",
            "-c:v",
            "copy",
            "-disposition:v",
            "attached_pic",
        ]);
    }
    command.args([
        "-metadata",
        "album=Artwork Fixture",
        "-metadata",
        "album_artist=Artwork Artist",
        "-metadata",
        "artist=Artwork Artist",
    ]);
    if release {
        command
            .arg("-metadata")
            .arg(format!("MUSICBRAINZ_ALBUMID={RELEASE}"));
    }
    let output = command
        .arg(path)
        .output()
        .expect("FFmpeg creates test inputs only");
    assert!(
        output.status.success(),
        "fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn album_request(album: u64, revision: u64, size: ArtworkSize) -> Request {
    Request {
        album_id: Some(album),
        entry_id: None,
        library_revision: revision,
        size,
        providers: vec![ArtworkProvider::CoverArtArchive],
        discogs_token: None,
    }
}
async fn artwork(handle: &Handle, request: Request) -> Artwork {
    match tokio::time::timeout(Duration::from_secs(20), handle.request(request))
        .await
        .unwrap()
    {
        Reply::Ok {
            result: Response::Artwork { artwork },
        } => artwork,
        other => panic!("unexpected response {other:?}"),
    }
}
fn color(art: &Artwork) -> [u8; 3] {
    let bytes = STANDARD.decode(art.data.as_ref().unwrap()).unwrap();
    let image = image::load_from_memory(&bytes).unwrap().into_rgb8();
    image.get_pixel(0, 0).0
}
async fn shutdown(workers: ArtworkWorkers) {
    tokio::task::spawn_blocking(move || workers.shutdown())
        .await
        .unwrap();
}

#[tokio::test]
async fn folder_variants_are_normalized_and_folder_only_rescan_invalidates() {
    let fixture = Fixture::new();
    wav(&fixture.music("track.wav"));
    fs::write(fixture.music("CoVeR.PNG"), png(1024, 512, [240, 0, 0])).unwrap();
    let (root, album, revision) = fixture.scan().await;
    let workers = fixture.workers();
    let handle = workers.handle();
    let display = artwork(
        &handle,
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(display.status, ArtworkStatus::Ready);
    assert_eq!(display.source, Some(ArtworkSource::Folder));
    assert_eq!((display.width, display.height), (512, 256));
    assert_eq!(display.mime_type.as_deref(), Some("image/jpeg"));
    let thumbnail = artwork(
        &handle,
        album_request(album, revision, ArtworkSize::Thumbnail),
    )
    .await;
    assert_eq!((thumbnail.width, thumbnail.height), (128, 64));
    fs::write(fixture.music("CoVeR.PNG"), png(1024, 512, [0, 0, 240])).unwrap();
    let cached = artwork(
        &handle,
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(
        display.cache_key, cached.cache_key,
        "local discoveries persist until explicit rescan"
    );
    fixture.rescan(root).await;
    let (new_album, new_revision) = fixture.album().await;
    assert_eq!(new_album, album);
    // A folder-only rescan changes no list rows, so the content revision holds;
    // the completed scan watermark in the cache key does the invalidating.
    assert_eq!(new_revision, revision);
    let refreshed = artwork(
        &handle,
        album_request(album, new_revision, ArtworkSize::Display),
    )
    .await;
    assert_ne!(display.cache_key, refreshed.cache_key);
    assert!(color(&refreshed)[2] > 220);
    shutdown(workers).await;
}

#[tokio::test]
async fn whole_album_embedded_precedes_folder_and_malformed_embedded_falls_through() {
    let fixture = Fixture::new();
    let embedded = fixture.root.join("embedded.png");
    fs::write(&embedded, png(40, 20, [0, 230, 0])).unwrap();
    tagged(&fixture.music("01-no-cover.flac"), None, false);
    tagged(&fixture.music("02-cover.flac"), Some(&embedded), false);
    fs::write(fixture.music("cover.png"), png(100, 100, [230, 0, 0])).unwrap();
    let (root, album, revision) = fixture.scan().await;
    let workers = fixture.workers();
    let art = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(art.source, Some(ArtworkSource::Embedded));
    assert_eq!(
        (art.width, art.height),
        (40, 20),
        "small images are not upscaled"
    );
    assert!(color(&art)[1] > 200);
    let path = fixture.music("02-cover.flac");
    let mut bytes = fs::read(&path).unwrap();
    let picture = bytes
        .windows(8)
        .position(|bytes| bytes == b"\x89PNG\r\n\x1a\n")
        .expect("FLAC contains embedded PNG");
    bytes[picture] = 0;
    fs::write(path, bytes).unwrap();
    fixture.rescan(root).await;
    let (album, revision) = fixture.album().await;
    let fallback = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(fallback.source, Some(ArtworkSource::Folder));
    assert!(color(&fallback)[0] > 200);
    shutdown(workers).await;
}

#[tokio::test]
async fn local_missing_is_cached_and_no_cover_does_not_change_queue() {
    let fixture = Fixture::new();
    wav(&fixture.music("track.wav"));
    let (root, album, revision) = fixture.scan().await;
    let workers = fixture.workers();
    let request = album_request(album, revision, ArtworkSize::Display);
    assert_eq!(
        artwork(&workers.handle(), request.clone()).await.status,
        ArtworkStatus::Missing
    );
    fs::write(fixture.music("cover.png"), png(10, 10, [0, 0, 230])).unwrap();
    assert_eq!(
        artwork(&workers.handle(), request.clone()).await.status,
        ArtworkStatus::Missing
    );
    shutdown(workers).await;
    let workers = fixture.workers();
    assert_eq!(
        artwork(&workers.handle(), request).await.status,
        ArtworkStatus::Missing,
        "negative cache survives reopen"
    );
    fixture.rescan(root).await;
    let (album, revision) = fixture.album().await;
    assert_eq!(
        artwork(
            &workers.handle(),
            album_request(album, revision, ArtworkSize::Display)
        )
        .await
        .status,
        ArtworkStatus::Ready
    );
    let state = Store::open(&fixture.database).unwrap().load().unwrap();
    assert!(state.entries.is_empty());
    assert_eq!(state.revision, 1);
    assert!(state.checkpoint.current_id.is_none());
    shutdown(workers).await;
}

#[tokio::test]
async fn live_queue_occurrences_have_neutral_artwork_without_local_discovery() {
    let fixture = Fixture::new();
    let db = Connection::open(&fixture.database).unwrap();
    db.execute_batch(
        "INSERT INTO queue(id,ordinal,path,stream_title,source_kind)
         VALUES(7,0,'https://radio.example/live.mp3','Radio','stream');
         UPDATE control SET next_id=8,revision=2;",
    )
    .unwrap();
    let workers = fixture.workers();
    let art = artwork(
        &workers.handle(),
        Request {
            album_id: None,
            entry_id: Some(7),
            library_revision: 1,
            size: ArtworkSize::Display,
            providers: vec![ArtworkProvider::CoverArtArchive],
            discogs_token: None,
        },
    )
    .await;
    assert_eq!(art.status, ArtworkStatus::Missing);
    assert!(art.source.is_none());
    shutdown(workers).await;
}

#[tokio::test]
async fn durable_direct_queue_occurrence_is_authoritative_and_invalid_targets_are_typed() {
    let fixture = Fixture::new();
    let path = fixture.music("direct.wav");
    wav(&path);
    fs::write(fixture.music("folder.png"), png(30, 20, [200, 20, 10])).unwrap();
    let db = Connection::open(&fixture.database).unwrap();
    db.execute(
        "INSERT INTO queue(id,ordinal,path) VALUES(7,0,?1)",
        [path.to_str().unwrap()],
    )
    .unwrap();
    db.execute("UPDATE control SET next_id=8,revision=2", [])
        .unwrap();
    let workers = fixture.workers();
    let request = Request {
        album_id: None,
        entry_id: Some(7),
        library_revision: 1,
        size: ArtworkSize::Display,
        providers: vec![ArtworkProvider::CoverArtArchive],
        discogs_token: None,
    };
    let art = artwork(&workers.handle(), request.clone()).await;
    assert_eq!(art.source, Some(ArtworkSource::Folder));
    for invalid in [
        Request {
            album_id: Some(1),
            ..request.clone()
        },
        Request {
            entry_id: None,
            ..request.clone()
        },
        Request {
            entry_id: Some(8),
            ..request.clone()
        },
    ] {
        assert!(matches!(
            workers.handle().request(invalid).await,
            Reply::Error {
                code: ErrorCode::InvalidCommand,
                queue_revision: 2,
                ..
            }
        ));
    }
    db.execute("DELETE FROM queue WHERE id=7", []).unwrap();
    assert!(matches!(
        workers.handle().request(request).await,
        Reply::Error {
            code: ErrorCode::InvalidCommand,
            ..
        }
    ));
    shutdown(workers).await;
}

#[tokio::test]
async fn oversized_and_malformed_folder_sources_do_not_hide_a_valid_lower_priority_cover() {
    let fixture = Fixture::new();
    wav(&fixture.music("track.wav"));
    fs::File::create(fixture.music("cover.jpg"))
        .unwrap()
        .set_len(provider::MAX_SOURCE_BYTES as u64 + 1)
        .unwrap();
    fs::write(fixture.music("cover.jpeg"), b"not an image").unwrap();
    fs::write(fixture.music("cover.png"), png(8193, 1, [250, 0, 0])).unwrap();
    fs::write(fixture.music("folder.png"), png(31, 17, [0, 0, 230])).unwrap();
    let (_, album, revision) = fixture.scan().await;
    let workers = fixture.workers();
    let art = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(art.source, Some(ArtworkSource::Folder));
    assert_eq!((art.width, art.height), (31, 17));
    assert!(color(&art)[2] > 200);
    shutdown(workers).await;
}

struct HttpFixture {
    origin: String,
    count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl HttpFixture {
    fn new(status: u16, body: Vec<u8>) -> Self {
        Self::delayed(status, body, Duration::ZERO)
    }
    fn delayed(status: u16, body: Vec<u8>, delay: Duration) -> Self {
        Self::routed(move |request, _| {
            assert!(request.starts_with(&format!("GET /release/{RELEASE}/front-500 ")));
            thread::sleep(delay);
            (status, body.clone())
        })
    }
    fn routed(handler: impl Fn(&str, &str) -> (u16, Vec<u8>) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_count = Arc::clone(&count);
        let thread_stop = Arc::clone(&stop);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let fixture_origin = origin.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        // Accepted sockets may inherit O_NONBLOCK on Darwin.
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut reader = BufReader::new(&mut socket);
                        let mut request = String::new();
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                                break;
                            }
                            request.push_str(&line);
                        }
                        recorded.lock().unwrap().push(request.clone());
                        thread_count.fetch_add(1, Ordering::AcqRel);
                        let (status, body) = handler(&request, &fixture_origin);
                        let header = format!(
                            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: image/png\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket
                            .write_all(header.as_bytes())
                            .and_then(|()| socket.write_all(&body));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("HTTP fixture failed: {error}"),
                }
            }
        });
        Self {
            origin,
            count,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn provider(&self) -> Provider {
        let origin = self.origin.clone();
        thread::spawn(move || Provider::for_test(&origin).unwrap())
            .join()
            .unwrap()
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

#[tokio::test]
async fn provider_positive_reopens_offline_but_new_local_cover_wins_after_rescan() {
    let fixture = Fixture::new();
    tagged(&fixture.music("online.flac"), None, true);
    let (root, album, revision) = fixture.scan().await;
    let server = HttpFixture::new(200, png(800, 400, [230, 0, 0]));
    let provider = server.provider();
    let workers = fixture.provider_workers(provider.clone());
    let first = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(first.source, Some(ArtworkSource::CoverArtArchive));
    assert_eq!((first.width, first.height), (512, 256));
    assert_eq!(server.count.load(Ordering::Acquire), 1);
    shutdown(workers).await;
    drop(server);
    let workers = fixture.provider_workers(provider);
    let offline = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(offline.cache_key, first.cache_key);
    fixture.rescan(root).await;
    let (_, revision) = fixture.album().await;
    let after_scan = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(
        after_scan.cache_key, first.cache_key,
        "release cache survives unrelated scans"
    );
    fs::write(fixture.music("cover.png"), png(90, 50, [0, 230, 0])).unwrap();
    fixture.rescan(root).await;
    let (_, revision) = fixture.album().await;
    let local = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(local.source, Some(ArtworkSource::Folder));
    assert!(color(&local)[1] > 200);
    shutdown(workers).await;
}

#[tokio::test]
async fn provider_missing_and_transient_failure_are_persistent_but_expiring() {
    for (status, expected, ttl) in [
        (404, ArtworkStatus::Missing, NEGATIVE_TTL),
        (503, ArtworkStatus::Unavailable, FAILURE_TTL),
    ] {
        let fixture = Fixture::new();
        tagged(&fixture.music("online.flac"), None, true);
        let (_, album, revision) = fixture.scan().await;
        let server = HttpFixture::new(status, vec![]);
        let provider = server.provider();
        let workers = fixture.provider_workers(provider.clone());
        let request = album_request(album, revision, ArtworkSize::Display);
        assert_eq!(
            artwork(&workers.handle(), request.clone()).await.status,
            expected
        );
        assert_eq!(
            artwork(&workers.handle(), request.clone()).await.status,
            expected
        );
        assert_eq!(server.count.load(Ordering::Acquire), 1);
        shutdown(workers).await;
        // Move the persisted expiry into the past rather than sleeping 24h. The
        // assertion remains at the request seam: an expired miss re-fetches.
        let cache = Connection::open(fixture.database.with_extension("artwork.sqlite")).unwrap();
        let expires: u64 = cache
            .query_row(
                "SELECT expires FROM artwork_cache WHERE key LIKE 'remote:%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(expires > unix_time() && expires <= unix_time() + ttl);
        let workers = fixture.provider_workers(provider.clone());
        assert_eq!(
            artwork(&workers.handle(), request.clone()).await.status,
            expected
        );
        assert_eq!(server.count.load(Ordering::Acquire), 1);
        cache
            .execute(
                "UPDATE artwork_cache SET expires=0 WHERE key LIKE 'remote:%'",
                [],
            )
            .unwrap();
        assert_eq!(artwork(&workers.handle(), request).await.status, expected);
        assert_eq!(server.count.load(Ordering::Acquire), 2);
        shutdown(workers).await;
    }
}

#[tokio::test]
async fn delayed_provider_does_not_block_main_controls_or_mutate_playback() {
    use crate::{
        output::OutputConfig,
        owner::{Owner, OwnerConfig},
        protocol::{Command, Message, Request as WireRequest},
    };
    let fixture = Fixture::new();
    tagged(&fixture.music("online.flac"), None, true);
    let (_, album, revision) = fixture.scan().await;
    let server = HttpFixture::delayed(200, png(30, 30, [200, 0, 0]), Duration::from_millis(1500));
    let workers = fixture.provider_workers(server.provider());
    let owner = Owner::start_with_artwork(
        OwnerConfig {
            database_path: fixture.database.clone(),
            output: OutputConfig::Capture {
                path: fixture.root.join("capture.f32"),
                clocked: true,
            },
        },
        workers,
    )
    .unwrap();
    let control = owner.controller();
    let before = control.subscribe().await.unwrap().snapshot;
    let art_control = control.clone();
    let pending = tokio::spawn(async move {
        art_control
            .request(WireRequest {
                id: 1,
                command: Command::Artwork {
                    album_id: Some(album),
                    entry_id: None,
                    library_revision: revision,
                    size: ArtworkSize::Display,
                    providers: vec![ArtworkProvider::CoverArtArchive],
                    discogs_token: None,
                },
            })
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.count.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let response = tokio::time::timeout(
        Duration::from_millis(500),
        control.request(WireRequest {
            id: 2,
            command: Command::Pause,
        }),
    )
    .await
    .expect("pause must not wait for provider")
    .unwrap();
    assert!(
        !pending.is_finished(),
        "the real provider request is still delayed"
    );
    let Message::Reply {
        reply: Reply::Ok {
            result: Response::Applied { snapshot, .. },
        },
        ..
    } = response
    else {
        panic!("pause failed: {response:?}")
    };
    assert_eq!(snapshot.queue_revision, before.queue_revision);
    assert_eq!(snapshot.current_id, before.current_id);
    assert_eq!(snapshot.error, before.error);
    let Message::Reply {
        reply: Reply::Ok {
            result: Response::Artwork { artwork },
        },
        ..
    } = pending.await.unwrap()
    else {
        panic!("artwork request failed")
    };
    assert_eq!(artwork.source, Some(ArtworkSource::CoverArtArchive));
    let after = control.subscribe().await.unwrap().snapshot;
    assert_eq!(after.queue_revision, before.queue_revision);
    assert_eq!(after.current_id, before.current_id);
    assert_eq!(after.error, before.error);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn excessive_pixel_count_is_rejected_before_decoding() {
    let fixture = Fixture::new();
    wav(&fixture.music("track.wav"));
    // Keep a real PNG's compressed payload but advertise 20 megapixels in its
    // checksummed IHDR. The worker must reject dimensions without allocating it.
    let mut bytes = png(1, 1, [0, 0, 0]);
    bytes[16..20].copy_from_slice(&5000_u32.to_be_bytes());
    bytes[20..24].copy_from_slice(&4000_u32.to_be_bytes());
    let mut crc = !0u32;
    for byte in &bytes[12..29] {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    bytes[29..33].copy_from_slice(&(!crc).to_be_bytes());
    fs::write(fixture.music("cover.png"), bytes).unwrap();
    let (_, album, revision) = fixture.scan().await;
    let workers = fixture.workers();
    let art = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(art.status, ArtworkStatus::Unavailable);
    assert!(art.message.as_deref().unwrap().contains("pixel count"));
    assert!(art.data.is_none());
    assert_eq!((art.width, art.height), (0, 0));
    shutdown(workers).await;
}

#[tokio::test]
async fn unusable_private_cache_does_not_prevent_local_artwork_or_touch_its_target() {
    let fixture = Fixture::new();
    wav(&fixture.music("track.wav"));
    fs::write(fixture.music("cover.png"), png(20, 10, [0, 230, 0])).unwrap();
    let (_, album, revision) = fixture.scan().await;
    let sentinel = fixture.root.join("unrelated");
    fs::write(&sentinel, b"do not alter").unwrap();
    std::os::unix::fs::symlink(&sentinel, fixture.database.with_extension("artwork.sqlite"))
        .unwrap();
    let workers = fixture.workers();
    let art = artwork(
        &workers.handle(),
        album_request(album, revision, ArtworkSize::Display),
    )
    .await;
    assert_eq!(art.source, Some(ArtworkSource::Folder));
    assert_eq!(fs::read(sentinel).unwrap(), b"do not alter");
    let state = Store::open(&fixture.database).unwrap().load().unwrap();
    assert_eq!(state.revision, 1);
    assert!(state.entries.is_empty());
    shutdown(workers).await;
}

#[tokio::test]
async fn concurrent_thumbnail_and_display_share_one_cold_provider_download() {
    let fixture = Fixture::new();
    tagged(&fixture.music("online.flac"), None, true);
    let (_, album, revision) = fixture.scan().await;
    let server = HttpFixture::delayed(200, png(800, 400, [230, 0, 0]), Duration::from_millis(200));
    let workers = fixture.provider_workers(server.provider());
    let first = workers.handle();
    let second = workers.handle();
    let (thumbnail, display) = tokio::join!(
        artwork(
            &first,
            album_request(album, revision, ArtworkSize::Thumbnail)
        ),
        artwork(
            &second,
            album_request(album, revision, ArtworkSize::Display)
        ),
    );
    assert_eq!(thumbnail.source, Some(ArtworkSource::CoverArtArchive));
    assert_eq!(display.source, Some(ArtworkSource::CoverArtArchive));
    assert_eq!((thumbnail.width, thumbnail.height), (128, 64));
    assert_eq!((display.width, display.height), (512, 256));
    assert_eq!(server.count.load(Ordering::Acquire), 1);
    shutdown(workers).await;
}

fn provider_request(album: u64, revision: u64, providers: Vec<ArtworkProvider>) -> Request {
    let discogs_token = providers
        .contains(&ArtworkProvider::Discogs)
        .then(|| serde_json::from_value(serde_json::json!("fixture-secret-token")).unwrap());
    Request {
        providers,
        discogs_token,
        ..album_request(album, revision, ArtworkSize::Display)
    }
}

fn catalogue_response(request: &str, origin: &str) -> (u16, Vec<u8>) {
    let json = if request.starts_with("GET /search/album?") {
        serde_json::json!({"total":2,"data":[
            {"id":8,"title":"Artwork Fixture","artist":{"name":"Wrong Artist"},"cover_big":format!("{origin}wrong")},
            {"id":9,"title":"Artwork Fixture","artist":{"name":"Artwork Artist"},"cover_big":format!("{origin}deezer-image")}
        ]})
    } else if request.starts_with("GET /database/search?") {
        assert!(
            request
                .to_lowercase()
                .contains("authorization: discogs token=fixture-secret-token")
        );
        serde_json::json!({"pagination":{"items":1},"results":[{"id":42}]})
    } else if request.starts_with("GET /releases/42 ") {
        assert!(
            request
                .to_lowercase()
                .contains("authorization: discogs token=fixture-secret-token")
        );
        serde_json::json!({"id":42,"title":"Artwork Fixture","artists":[{"name":"Artwork Artist"}],
            "images":[{"type":"secondary","uri":format!("{origin}wrong")},
                {"type":"primary","uri":format!("{origin}discogs-image")}]})
    } else if request.starts_with("GET /deezer-image ")
        || request.starts_with("GET /discogs-image ")
    {
        assert!(!request.to_lowercase().contains("authorization:"));
        return (200, png(64, 32, [10, 150, 230]));
    } else {
        panic!("unexpected provider request: {request}");
    };
    (200, serde_json::to_vec(&json).unwrap())
}

#[tokio::test]
async fn deezer_exact_match_has_provenance_and_never_reuses_image_cache() {
    let fixture = Fixture::new();
    tagged(&fixture.music("deezer.flac"), None, false);
    let (_, album, revision) = fixture.scan().await;
    let server = HttpFixture::routed(catalogue_response);
    let provider = server.provider();
    let workers = fixture.provider_workers(provider.clone());
    let request = provider_request(album, revision, vec![ArtworkProvider::Deezer]);
    let first = artwork(&workers.handle(), request.clone()).await;
    assert_eq!(first.source, Some(ArtworkSource::Deezer));
    assert_eq!(
        first.source_url.as_deref(),
        Some("https://www.deezer.com/album/9")
    );
    assert_eq!(first.cache_policy, ArtworkCachePolicy::DisplayOnly);
    assert_eq!(first.expires_at, None);
    let second = artwork(&workers.handle(), request.clone()).await;
    assert_eq!(second.cache_key, first.cache_key);
    assert_eq!(
        server.count.load(Ordering::Acquire),
        4,
        "active requests do not reuse Deezer images"
    );
    let local_only = artwork(&workers.handle(), provider_request(album, revision, vec![])).await;
    assert_eq!(local_only.status, ArtworkStatus::Missing);
    assert_eq!(server.count.load(Ordering::Acquire), 4);
    let cache = Connection::open(fixture.database.with_extension("artwork.sqlite")).unwrap();
    let images: u64 = cache
        .query_row(
            "SELECT COUNT(*) FROM artwork_cache WHERE value LIKE '%deezer%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(images, 0);
    shutdown(workers).await;
    drop(server);
    let workers = fixture.provider_workers(provider);
    assert_eq!(
        artwork(&workers.handle(), request).await.status,
        ArtworkStatus::Unavailable
    );
    shutdown(workers).await;
}

#[tokio::test]
async fn discogs_primary_image_expires_and_credentials_never_enter_cache() {
    let fixture = Fixture::new();
    tagged(&fixture.music("discogs.flac"), None, false);
    let (_, album, revision) = fixture.scan().await;
    let server = HttpFixture::routed(catalogue_response);
    let provider = server.provider();
    let workers = fixture.provider_workers(provider.clone());
    let request = provider_request(album, revision, vec![ArtworkProvider::Discogs]);
    let before = unix_time();
    let first = artwork(&workers.handle(), request.clone()).await;
    assert_eq!(first.source, Some(ArtworkSource::Discogs));
    assert_eq!(
        first.source_url.as_deref(),
        Some("https://www.discogs.com/release/42")
    );
    assert_eq!(first.cache_policy, ArtworkCachePolicy::Persistent);
    assert!(
        first.expires_at.unwrap() > before
            && first.expires_at.unwrap() <= unix_time() + 6 * 60 * 60
    );
    assert_eq!(
        artwork(&workers.handle(), request.clone()).await.cache_key,
        first.cache_key
    );
    assert_eq!(server.count.load(Ordering::Acquire), 3);
    {
        let requests = server.requests.lock().unwrap();
        assert!(requests.iter().all(|request| {
            !request
                .lines()
                .next()
                .unwrap()
                .contains("fixture-secret-token")
        }));
    }
    let cache = Connection::open(fixture.database.with_extension("artwork.sqlite")).unwrap();
    let leaks: u64 = cache.query_row("SELECT COUNT(*) FROM artwork_cache WHERE key LIKE '%fixture-secret-token%' OR value LIKE '%fixture-secret-token%'", [], |row| row.get(0)).unwrap();
    assert_eq!(leaks, 0);
    shutdown(workers).await;
    drop(server);
    let workers = fixture.provider_workers(provider);
    assert_eq!(
        artwork(&workers.handle(), request.clone()).await.source,
        Some(ArtworkSource::Discogs)
    );
    cache
        .execute(
            "UPDATE artwork_cache SET expires=0 WHERE key LIKE 'remote:%'",
            [],
        )
        .unwrap();
    let expired = artwork(&workers.handle(), request).await;
    assert_eq!(expired.status, ArtworkStatus::Unavailable);
    assert!(expired.data.is_none() && expired.source.is_none() && expired.expires_at.is_none());
    shutdown(workers).await;
}

#[tokio::test]
async fn remote_order_disabling_credentials_and_local_precedence_are_authoritative() {
    let fixture = Fixture::new();
    tagged(&fixture.music("order.flac"), None, false);
    let (root, album, revision) = fixture.scan().await;
    let server = HttpFixture::routed(catalogue_response);
    let workers = fixture.provider_workers(server.provider());
    let discogs_first = provider_request(
        album,
        revision,
        vec![ArtworkProvider::Discogs, ArtworkProvider::Deezer],
    );
    let first = artwork(&workers.handle(), discogs_first.clone()).await;
    assert_eq!(first.source, Some(ArtworkSource::Discogs));
    let deezer_first = provider_request(
        album,
        revision,
        vec![ArtworkProvider::Deezer, ArtworkProvider::Discogs],
    );
    let reordered = artwork(&workers.handle(), deezer_first).await;
    assert_eq!(reordered.source, Some(ArtworkSource::Deezer));
    assert_ne!(
        first.cache_key, reordered.cache_key,
        "same pixels retain distinct source/policy identities"
    );
    let disabled = artwork(&workers.handle(), provider_request(album, revision, vec![])).await;
    assert_eq!(disabled.status, ArtworkStatus::Missing);
    assert_eq!(server.count.load(Ordering::Acquire), 5);
    for invalid in [
        Request {
            discogs_token: None,
            ..discogs_first.clone()
        },
        provider_request(
            album,
            revision,
            vec![ArtworkProvider::Deezer, ArtworkProvider::Deezer],
        ),
        Request {
            providers: vec![],
            ..discogs_first.clone()
        },
    ] {
        assert!(matches!(
            workers.handle().request(invalid).await,
            Reply::Error {
                code: ErrorCode::InvalidCommand,
                ..
            }
        ));
    }
    assert_eq!(server.count.load(Ordering::Acquire), 5);
    fs::write(fixture.music("cover.png"), png(30, 30, [200, 0, 0])).unwrap();
    fixture.rescan(root).await;
    let (_, revision) = fixture.album().await;
    let local = artwork(
        &workers.handle(),
        Request {
            library_revision: revision,
            ..discogs_first
        },
    )
    .await;
    assert_eq!(local.source, Some(ArtworkSource::Folder));
    assert_eq!(server.count.load(Ordering::Acquire), 5);
    shutdown(workers).await;
}

#[tokio::test]
async fn provider_authentication_failure_and_invalid_images_fall_through() {
    for reject_credentials in [true, false] {
        let fixture = Fixture::new();
        tagged(&fixture.music("fallback.flac"), None, true);
        let (_, album, revision) = fixture.scan().await;
        let server = HttpFixture::routed(move |request, origin| {
            if reject_credentials && request.starts_with("GET /database/search?") {
                (401, b"fixture-secret-token must not be reflected".to_vec())
            } else if request.starts_with("GET /release/")
                || request.starts_with("GET /discogs-image ")
            {
                (200, b"invalid image".to_vec())
            } else {
                catalogue_response(request, origin)
            }
        });
        let workers = fixture.provider_workers(server.provider());
        let art = artwork(
            &workers.handle(),
            provider_request(
                album,
                revision,
                vec![
                    ArtworkProvider::CoverArtArchive,
                    ArtworkProvider::Discogs,
                    ArtworkProvider::Deezer,
                ],
            ),
        )
        .await;
        assert_eq!(art.source, Some(ArtworkSource::Deezer));
        if reject_credentials {
            assert_eq!(
                art.message.as_deref(),
                Some("Discogs rejected credentials or access")
            );
        }
        assert!(
            !art.message
                .as_deref()
                .unwrap_or("")
                .contains("fixture-secret-token")
        );
        shutdown(workers).await;
    }
}

#[tokio::test]
async fn direct_queue_metadata_can_resolve_deezer_without_library_album() {
    let fixture = Fixture::new();
    let path = fixture.music("direct.flac");
    tagged(&path, None, false);
    let db = Connection::open(&fixture.database).unwrap();
    db.execute(
        "INSERT INTO queue(id,ordinal,path) VALUES(7,0,?1)",
        [path.to_str().unwrap()],
    )
    .unwrap();
    let server = HttpFixture::routed(catalogue_response);
    let workers = fixture.provider_workers(server.provider());
    let art = artwork(
        &workers.handle(),
        Request {
            album_id: None,
            entry_id: Some(7),
            library_revision: 1,
            size: ArtworkSize::Display,
            providers: vec![ArtworkProvider::Deezer],
            discogs_token: None,
        },
    )
    .await;
    assert_eq!(art.source, Some(ArtworkSource::Deezer));
    shutdown(workers).await;
}

#[tokio::test]
async fn changed_discogs_credentials_cannot_reuse_previously_authorized_images() {
    let fixture = Fixture::new();
    tagged(&fixture.music("credentials.flac"), None, false);
    let (_, album, revision) = fixture.scan().await;
    let server = HttpFixture::routed(|request, origin| {
        if request
            .to_lowercase()
            .contains("authorization: discogs token=changed-token")
        {
            (403, b"changed-token".to_vec())
        } else {
            catalogue_response(request, origin)
        }
    });
    let workers = fixture.provider_workers(server.provider());
    let mut request = provider_request(album, revision, vec![ArtworkProvider::Discogs]);
    assert_eq!(
        artwork(&workers.handle(), request.clone()).await.source,
        Some(ArtworkSource::Discogs)
    );
    request.discogs_token =
        Some(serde_json::from_value(serde_json::json!("changed-token")).unwrap());
    let rejected = artwork(&workers.handle(), request).await;
    assert_eq!(rejected.status, ArtworkStatus::Unavailable);
    assert_eq!(
        rejected.message.as_deref(),
        Some("Discogs rejected credentials or access")
    );
    assert!(rejected.data.is_none());
    assert_eq!(server.count.load(Ordering::Acquire), 4);
    shutdown(workers).await;
}
