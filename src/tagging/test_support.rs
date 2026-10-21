//! Loopback HTTP fixture for provider tests, and a scanned album of tagged
//! files. Tests never touch the network.
use super::tags::{TagEdit, edit_tags};
use crate::library::Library;
use crate::library_types::{LibraryCommand, LibraryReply, ScanPhase};
use crate::protocol::TagField;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(super) struct Seen {
    pub target: String,
    pub headers: String,
}
pub(super) struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    pub delay: Duration,
}
pub(super) fn json(status: u16, body: Vec<u8>) -> Reply {
    Reply {
        status,
        headers: vec![("Content-Type", "application/json".into())],
        body,
        delay: Duration::ZERO,
    }
}
pub(super) fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/musicbrainz")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub(super) struct HttpFixture {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
    done: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl HttpFixture {
    /// Serves each request with `route(target)`, one connection at a time.
    pub(super) fn new(route: impl Fn(&str) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, done) = (Arc::clone(&seen), Arc::clone(&done));
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if done.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let Some((target, headers)) = read_request(&mut stream) else {
                        continue;
                    };
                    seen.lock().unwrap().push(Seen {
                        target: target.clone(),
                        headers,
                    });
                    let reply = route(&target);
                    thread::sleep(reply.delay);
                    let _ = write_reply(&mut stream, &reply);
                }
            })
        };
        Self {
            address,
            seen,
            done,
            thread: Some(thread),
        }
    }
    pub(super) fn origin(&self) -> String {
        format!("http://{}/", self.address)
    }
    pub(super) fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| Seen {
                target: s.target.clone(),
                headers: s.headers.clone(),
            })
            .collect()
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut data = Vec::new();
    let mut chunk = [0u8; 1024];
    while !data.windows(4).any(|w| w == b"\r\n\r\n") {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..count]);
    }
    let text = String::from_utf8_lossy(&data).into_owned();
    let (line, headers) = text.split_once("\r\n")?;
    let mut parts = line.split(' ');
    (parts.next()? == "GET").then_some(())?;
    Some((parts.next()?.to_owned(), headers.to_owned()))
}
fn write_reply(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)
}

/// Writes `seconds` of a 48 kHz s16 stereo logarithmic chirp, 200 Hz to 2000 Hz, as a WAV file.
pub(super) fn write_tone_wav(path: &std::path::Path, seconds: u32) {
    const RATE: u32 = 48_000;
    let frames = u64::from(RATE) * u64::from(seconds);
    let (f0, f1) = (200.0_f64, 2000.0_f64);
    let total = f64::from(seconds);
    let k = (f1 / f0).ln();
    let data_len = (frames * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for n in 0..frames {
        let t = n as f64 / f64::from(RATE);
        let phase = std::f64::consts::TAU * f0 * total / k * ((k * t / total).exp() - 1.0);
        let sample = ((phase.sin() * 0.5) * 32767.0) as i16;
        out.extend_from_slice(&sample.to_le_bytes());
        out.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, out).unwrap();
}

/// An album of tagged copies of format fixtures, indexed by a real `Library`.
pub(super) struct LibraryFixture {
    pub root: PathBuf,
    pub database: PathBuf,
    pub album_id: u64,
    pub revision: u64,
    pub files: Vec<PathBuf>,
}
impl LibraryFixture {
    /// Each entry is a fixture name in `tests/fixtures/formats` (or `"tone10.wav"`,
    /// a 10 s generated tone) and the tags to give its copy. Every copy also
    /// gets album "Old Album" and album artist "Old Artist" unless the entry
    /// sets them. All copies must form one album.
    pub(super) fn new(files: &[(&str, &[(TagField, &str)])]) -> Self {
        let root = std::env::temp_dir().join(format!("tyranny-tagging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let folder = root.join("Album");
        std::fs::create_dir(&folder).unwrap();
        let mut paths = Vec::with_capacity(files.len());
        for (index, (name, values)) in files.iter().enumerate() {
            let path = folder.join(format!("{:02} {name}", index + 1));
            let mut tags = BTreeMap::from([
                (TagField::Album, "Old Album".to_owned()),
                (TagField::AlbumArtist, "Old Artist".to_owned()),
            ]);
            tags.extend(
                values
                    .iter()
                    .map(|(field, value)| (*field, value.to_string())),
            );
            if name.ends_with(".mka") {
                ffmpeg_tagged(&formats(name), &path, &tags);
            } else {
                if *name == "tone10.wav" {
                    write_tone_wav(&path, 10);
                } else {
                    std::fs::copy(formats(name), &path).unwrap();
                }
                let edit = TagEdit {
                    set: tags.into_iter().map(|(f, v)| (f, Some(v))).collect(),
                    restore: Vec::new(),
                };
                edit_tags(&path, &edit).unwrap();
            }
            paths.push(path);
        }
        let database = root.join("state.sqlite");
        drop(crate::store::Store::open(&database).unwrap());
        let library = Library::start(database.clone()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let reply = library
                .handle()
                .request(LibraryCommand::AddRoot {
                    path: folder.to_str().unwrap().into(),
                })
                .await
                .unwrap();
            assert!(matches!(reply, LibraryReply::RootAdded(_)), "{reply:?}");
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let scan = library.state().scan;
                if !matches!(scan.phase, ScanPhase::Discovering | ScanPhase::Indexing) {
                    break;
                }
                assert!(Instant::now() < deadline, "scan did not finish: {scan:?}");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        library.shutdown().unwrap();
        let db = rusqlite::Connection::open(&database).unwrap();
        let count: usize = db
            .query_row("SELECT COUNT(*) FROM library_tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, files.len(), "every copy is indexed");
        let albums: Vec<u64> = db
            .prepare("SELECT DISTINCT album_id FROM library_tracks")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(albums.len(), 1, "the copies form one album: {albums:?}");
        let revision = db
            .query_row(
                "SELECT revision FROM library_state WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        Self {
            root,
            database,
            album_id: albums[0],
            revision,
            files: paths,
        }
    }
}
impl Drop for LibraryFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn formats(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formats")
        .join(name)
}

/// Copies `source`'s streams to `path` with FFmpeg, tagged with `tags`. For
/// formats lofty cannot write.
fn ffmpeg_tagged(
    source: &std::path::Path,
    path: &std::path::Path,
    tags: &BTreeMap<TagField, String>,
) {
    let mut command = std::process::Command::new("ffmpeg");
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(source)
        .args(["-c", "copy"]);
    for (field, value) in tags {
        command
            .arg("-metadata")
            .arg(format!("{}={value}", ffmpeg_key(*field)));
    }
    let output = command
        .arg(path)
        .output()
        .expect("FFmpeg is required to create tagged test media");
    assert!(
        output.status.success(),
        "fixture FFmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn ffmpeg_key(field: TagField) -> &'static str {
    match field {
        TagField::Title => "title",
        TagField::Artist => "artist",
        TagField::Album => "album",
        TagField::AlbumArtist => "album_artist",
        TagField::TrackNumber => "track",
        TagField::TrackTotal => "TRACKTOTAL",
        TagField::DiscNumber => "disc",
        TagField::DiscTotal => "DISCTOTAL",
        TagField::Date => "date",
        TagField::OriginalDate => "ORIGINALDATE",
        TagField::MusicbrainzRecordingId => "MUSICBRAINZ_TRACKID",
        TagField::MusicbrainzTrackId => "MUSICBRAINZ_RELEASETRACKID",
        TagField::MusicbrainzReleaseId => "MUSICBRAINZ_ALBUMID",
        TagField::MusicbrainzReleaseGroupId => "MUSICBRAINZ_RELEASEGROUPID",
        TagField::MusicbrainzArtistId => "MUSICBRAINZ_ARTISTID",
        TagField::MusicbrainzAlbumArtistId => "MUSICBRAINZ_ALBUMARTISTID",
    }
}
