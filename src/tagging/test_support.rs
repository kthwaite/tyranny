//! Loopback HTTP fixture for provider tests. Tests never touch the network.
use std::{
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
    pub at: Instant,
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
                        at: Instant::now(),
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
                at: s.at,
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
