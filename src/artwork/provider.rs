use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{ACCEPT, LOCATION};
use reqwest::{StatusCode, Url};

pub(super) const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
const START_INTERVAL: Duration = Duration::from_secs(1);
const MAX_REDIRECTS: usize = 5;
const ORIGIN: &str = "https://coverartarchive.org/";
const USER_AGENT: &str = concat!(
    "Tyranny/",
    env!("CARGO_PKG_VERSION"),
    " (development artwork client)"
);

#[derive(Clone)]
pub(super) struct Provider {
    client: Client,
    next_start: Arc<Mutex<Instant>>,
    timeout: Duration,
    start_interval: Duration,
    #[cfg(test)]
    fixture_origin: Option<Url>,
}

pub(super) enum FetchOutcome {
    Found(Vec<u8>),
    Missing,
    Unavailable(String),
}

impl Provider {
    pub(super) fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: client_builder().https_only(true).build()?,
            next_start: Arc::new(Mutex::new(Instant::now())),
            timeout: REQUEST_TIMEOUT,
            start_interval: START_INTERVAL,
            #[cfg(test)]
            fixture_origin: None,
        })
    }

    // Explicitly test-only: neither a production URL override nor an environment hook.
    #[cfg(test)]
    pub(super) fn for_test(origin: &str) -> anyhow::Result<Self> {
        let origin = Url::parse(origin)?;
        let loopback = origin
            .host_str()
            .and_then(|host| {
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback());
        anyhow::ensure!(
            loopback
                && origin.scheme() == "http"
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.path() == "/"
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "artwork fixture requires an explicit loopback HTTP origin"
        );
        Ok(Self {
            client: client_builder().no_proxy().build()?,
            next_start: Arc::new(Mutex::new(Instant::now())),
            timeout: REQUEST_TIMEOUT,
            start_interval: START_INTERVAL,
            fixture_origin: Some(origin),
        })
    }

    pub(super) fn fetch_front(&self, release_id: &str) -> FetchOutcome {
        // Do not accept paths, release-group IDs disguised as URLs, or search text.
        if release_id.len() != 36 || uuid::Uuid::try_parse(release_id).is_err() {
            return unavailable("Artwork release identifier is invalid");
        }
        let deadline = Instant::now() + self.timeout;
        if !self.wait_for_start(deadline) {
            return unavailable("Artwork request is temporarily rate limited");
        }
        let origin = ORIGIN;
        #[cfg(test)]
        let origin = self.fixture_origin.as_ref().map_or(origin, Url::as_str);
        let Ok(mut url) = Url::parse(&format!("{origin}release/{release_id}/front-500")) else {
            return unavailable("Artwork request could not be constructed");
        };

        // Handle redirects ourselves: validate and upgrade legacy HTTP URLs BEFORE
        // sending anything. Every hop shares the original end-to-end deadline.
        for redirects in 0..=MAX_REDIRECTS {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return unavailable("Artwork request timed out");
            };
            let response = self
                .client
                .get(url)
                .header(ACCEPT, "image/jpeg, image/png, image/webp")
                .timeout(remaining)
                .send();
            let mut response = match response {
                Ok(response) => response,
                Err(_) => return unavailable("Artwork provider could not be reached"),
            };
            match response.status() {
                StatusCode::NOT_FOUND => return FetchOutcome::Missing,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                    return unavailable("Artwork provider is temporarily rate limited");
                }
                StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT => {
                    if redirects == MAX_REDIRECTS {
                        return unavailable("Artwork provider redirected too many times");
                    }
                    let next = response
                        .headers()
                        .get(LOCATION)
                        .and_then(|location| location.to_str().ok())
                        .and_then(|location| response.url().join(location).ok())
                        .and_then(|next| self.allowed_redirect(next));
                    let Some(next) = next else {
                        return unavailable("Artwork provider returned an unsafe redirect");
                    };
                    url = next;
                    continue;
                }
                StatusCode::OK => {}
                _ => return unavailable("Artwork provider returned an unsuccessful response"),
            }
            if response
                .content_length()
                .is_some_and(|length| length > MAX_SOURCE_BYTES as u64)
            {
                return unavailable("Artwork image exceeds the size limit");
            }
            // Only use the already-bounded length as a capacity hint; streaming
            // still enforces the limit for chunked or incorrectly declared bodies.
            let capacity = response.content_length().unwrap_or(16 * 1024) as usize;
            let mut bytes = Vec::with_capacity(capacity);
            let mut chunk = [0u8; 16 * 1024];
            loop {
                if Instant::now() >= deadline {
                    return unavailable("Artwork request timed out");
                }
                let count = match response.read(&mut chunk) {
                    Ok(count) => count,
                    Err(_) => return unavailable("Artwork image could not be downloaded"),
                };
                if Instant::now() >= deadline {
                    return unavailable("Artwork request timed out");
                }
                if count == 0 {
                    return if bytes.is_empty() {
                        unavailable("Artwork provider returned an empty image")
                    } else {
                        FetchOutcome::Found(bytes)
                    };
                }
                if count > MAX_SOURCE_BYTES - bytes.len() {
                    return unavailable("Artwork image exceeds the size limit");
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
        }
        unavailable("Artwork provider redirected too many times")
    }

    fn wait_for_start(&self, deadline: Instant) -> bool {
        loop {
            let delay = {
                let Ok(mut next_start) = self.next_start.lock() else {
                    return false;
                };
                let now = Instant::now();
                if now >= deadline {
                    return false;
                }
                if now >= *next_start {
                    *next_start = now + self.start_interval;
                    return true;
                }
                if *next_start >= deadline {
                    return false;
                }
                *next_start - now
            };
            thread::sleep(delay);
        }
    }

    fn allowed_redirect(&self, mut url: Url) -> Option<Url> {
        if !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        #[cfg(test)]
        if let Some(origin) = &self.fixture_origin {
            return (url.origin() == origin.origin()).then_some(url);
        }
        let host = url.host_str()?;
        if !(host == "coverartarchive.org"
            || host == "archive.org"
            || host.ends_with(".archive.org"))
            || url.port().is_some()
        {
            return None;
        }
        match url.scheme() {
            "https" => {}
            // The official API documentation includes legacy HTTP archive redirects.
            // Upgrade trusted, default-port URLs without making an HTTP request.
            "http" => url.set_scheme("https").ok()?,
            _ => return None,
        }
        url.set_fragment(None);
        Some(url)
    }
}

fn client_builder() -> ClientBuilder {
    Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .referer(false)
        .connect_timeout(Duration::from_secs(2))
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(2)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
}

fn unavailable(message: &str) -> FetchOutcome {
    FetchOutcome::Unavailable(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};

    const RELEASE: &str = "76df3287-6cda-33eb-8e9a-044b5e15ffdd";

    struct Fixture {
        origin: String,
        requests: Arc<Mutex<Vec<(Instant, String)>>>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl Fixture {
        fn new(handler: impl Fn(usize, &str, &mut TcpStream) + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let origin = format!("http://{}/", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&requests);
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => panic!("fixture accept failed: {error}"),
                    };
                    // Darwin can inherit the listener's O_NONBLOCK on accept.
                    // Body-size tests need real backpressure, not an early EOF.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut reader = BufReader::new(&mut stream);
                    let mut request = String::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        request.push_str(&line);
                    }
                    let index = {
                        let mut requests = recorded.lock().unwrap();
                        let index = requests.len();
                        requests.push((Instant::now(), request.clone()));
                        index
                    };
                    handler(index, &request, &mut stream);
                }
            });
            Self {
                origin,
                requests,
                stop,
                worker: Some(worker),
            }
        }

        fn provider(&self) -> Provider {
            let mut provider = Provider::for_test(&self.origin).unwrap();
            provider.start_interval = Duration::ZERO;
            provider
        }

        fn count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    fn response(stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n"
        );
        let _ = stream.write_all(body);
    }

    fn assert_unavailable(outcome: FetchOutcome) -> String {
        match outcome {
            FetchOutcome::Unavailable(message) => message,
            _ => panic!("expected unavailable artwork"),
        }
    }

    #[test]
    fn fetches_only_release_thumbnail_and_follows_relative_redirect() {
        let fixture = Fixture::new(|index, _, stream| {
            if index == 0 {
                response(
                    stream,
                    "307 Temporary Redirect",
                    "Location: /image.jpg\r\n",
                    b"",
                );
            } else {
                response(
                    stream,
                    "200 OK",
                    "Content-Length: 3\r\nContent-Type: image/jpeg\r\n",
                    b"art",
                );
            }
        });
        match fixture.provider().fetch_front(RELEASE) {
            FetchOutcome::Found(bytes) => assert_eq!(bytes, b"art"),
            _ => panic!("expected image bytes"),
        }
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0]
                .1
                .starts_with(&format!("GET /release/{RELEASE}/front-500 HTTP/1.1\r\n"))
        );
        let headers = requests[0].1.to_ascii_lowercase();
        assert!(headers.contains("user-agent: tyranny/"));
        assert!(!headers.contains("authorization:"));
        assert!(!requests[1].1.to_ascii_lowercase().contains("referer:"));
    }

    #[test]
    fn invalid_identifiers_never_reach_http() {
        let fixture = Fixture::new(|_, _, _| panic!("invalid ID reached network"));
        let provider = fixture.provider();
        for invalid in [
            "",
            "../front",
            "https://archive.org/",
            "76df32876cda33eb8e9a044b5e15ffdd",
            "76df3287-6cda-33eb-8e9a-044b5e15ffdg",
        ] {
            assert_unavailable(provider.fetch_front(invalid));
        }
        assert_eq!(fixture.count(), 0);
    }

    #[test]
    fn distinguishes_missing_and_transient_statuses_without_retries() {
        for (status, missing) in [
            ("404 Not Found", true),
            ("429 Too Many Requests", false),
            ("503 Unavailable", false),
            ("500 Error", false),
            ("403 Forbidden", false),
            ("204 No Content", false),
            ("206 Partial Content", false),
        ] {
            let fixture = Fixture::new(move |_, _, stream| {
                response(stream, status, "", b"private server failure")
            });
            let outcome = fixture.provider().fetch_front(RELEASE);
            if missing {
                assert!(matches!(outcome, FetchOutcome::Missing));
            } else {
                assert!(!assert_unavailable(outcome).contains("private server failure"));
            }
            assert_eq!(fixture.count(), 1);
        }
    }

    #[test]
    fn missing_redirect_target_is_missing() {
        let fixture = Fixture::new(|index, _, stream| {
            if index == 0 {
                response(
                    stream,
                    "307 Temporary Redirect",
                    "Location: /absent\r\n",
                    b"",
                );
            } else {
                response(stream, "404 Not Found", "", b"");
            }
        });
        assert!(matches!(
            fixture.provider().fetch_front(RELEASE),
            FetchOutcome::Missing
        ));
        assert_eq!(fixture.count(), 2);
    }

    #[test]
    fn rejects_redirect_escape_credentials_and_missing_locations() {
        let escaped = Fixture::new(|_, _, _| panic!("escaped fixture origin"));
        for target in [
            escaped.origin.clone(),
            "https://archive.org/image.jpg".to_owned(),
            "http://user:secret@127.0.0.1/image".to_owned(),
            "file:///etc/passwd".to_owned(),
            "https://archive.org.evil.invalid/image?secret=private".to_owned(),
            "".to_owned(),
        ] {
            let fixture = Fixture::new(move |_, _, stream| {
                let header = if target.is_empty() {
                    String::new()
                } else {
                    format!("Location: {target}\r\n")
                };
                response(stream, "302 Found", &header, b"");
            });
            let message = assert_unavailable(fixture.provider().fetch_front(RELEASE));
            assert!(!message.contains("secret"));
            assert!(!message.contains("127.0.0.1"));
            assert_eq!(fixture.count(), 1);
        }
        assert_eq!(escaped.count(), 0);
    }

    #[test]
    fn production_redirect_policy_requires_trusted_tls_and_upgrades_legacy_http() {
        let provider = Provider::new().unwrap();
        for target in [
            "https://archive.org/a",
            "https://dn711103.ca.archive.org/a",
            "https://coverartarchive.org/a",
            "http://archive.org/a",
            "http://s3.us.archive.org/a",
        ] {
            let accepted = provider
                .allowed_redirect(Url::parse(target).unwrap())
                .unwrap();
            assert_eq!(accepted.scheme(), "https");
            assert_eq!(accepted.port_or_known_default(), Some(443));
        }
        for target in [
            "https://archive.org.evil.invalid/a",
            "https://evilarchive.org/a",
            "https://sub.coverartarchive.org/a",
            "https://archive.org:444/a",
            "http://archive.org:443/a",
            "https://user:pass@archive.org/a",
            "http://127.0.0.1/a",
            "https://[::1]/a",
            "file:///etc/passwd",
            "ftp://archive.org/a",
        ] {
            assert!(
                provider
                    .allowed_redirect(Url::parse(target).unwrap())
                    .is_none(),
                "accepted {target}"
            );
        }
        for origin in [
            "https://127.0.0.1/",
            "http://localhost/",
            "http://example.org/",
            "http://127.0.0.1/path",
            "http://127.0.0.1/?query",
            "http://user:secret@127.0.0.1/",
        ] {
            assert!(Provider::for_test(origin).is_err());
        }
    }

    #[test]
    fn bounds_redirect_loops() {
        let fixture = Fixture::new(|_, _, stream| {
            response(stream, "307 Temporary Redirect", "Location: /loop\r\n", b"")
        });
        assert_unavailable(fixture.provider().fetch_front(RELEASE));
        assert_eq!(fixture.count(), MAX_REDIRECTS + 1);
    }

    #[test]
    fn rejects_declared_oversize_without_waiting_for_body() {
        let fixture = Fixture::new(|_, _, stream| {
            response(
                stream,
                "200 OK",
                &format!("Content-Length: {}\r\n", MAX_SOURCE_BYTES + 1),
                b"",
            );
        });
        let message = assert_unavailable(fixture.provider().fetch_front(RELEASE));
        assert!(message.contains("size limit"));
    }

    #[test]
    fn caps_chunked_and_unknown_length_bodies_and_accepts_exact_limit() {
        for (chunked, size) in [
            (false, MAX_SOURCE_BYTES),
            (false, MAX_SOURCE_BYTES + 1),
            (true, MAX_SOURCE_BYTES + 1),
        ] {
            let fixture = Fixture::new(move |_, _, stream| {
                let headers = if chunked {
                    "Transfer-Encoding: chunked\r\n"
                } else {
                    ""
                };
                response(stream, "200 OK", headers, b"");
                let block = [b'x'; 16 * 1024];
                let mut sent = 0;
                while sent < size {
                    let count = (size - sent).min(block.len());
                    if chunked && write!(stream, "{count:x}\r\n").is_err() {
                        break;
                    }
                    if let Err(error) = stream.write_all(&block[..count]) {
                        assert!(
                            size > MAX_SOURCE_BYTES,
                            "exact-limit fixture stopped after {sent} bytes: {error}"
                        );
                        break;
                    }
                    if chunked && stream.write_all(b"\r\n").is_err() {
                        break;
                    }
                    sent += count;
                }
                if size == MAX_SOURCE_BYTES {
                    assert_eq!(
                        sent, size,
                        "exact-limit fixture must send its complete body"
                    );
                }
                if chunked {
                    let _ = stream.write_all(b"0\r\n\r\n");
                }
            });
            let outcome = fixture.provider().fetch_front(RELEASE);
            if size == MAX_SOURCE_BYTES {
                match outcome {
                    FetchOutcome::Found(bytes) => assert_eq!(bytes.len(), MAX_SOURCE_BYTES),
                    _ => panic!("exact byte limit should be accepted"),
                }
            } else {
                assert!(assert_unavailable(outcome).contains("size limit"));
            }
        }
    }

    #[test]
    fn rejects_empty_and_truncated_bodies() {
        for headers in ["Content-Length: 0\r\n", "Content-Length: 100\r\n"] {
            let fixture =
                Fixture::new(move |_, _, stream| response(stream, "200 OK", headers, b""));
            assert_unavailable(fixture.provider().fetch_front(RELEASE));
        }
    }

    #[test]
    fn times_out_headers_and_body_with_one_total_deadline() {
        for body_stall in [false, true] {
            let fixture = Fixture::new(move |_, _, stream| {
                if body_stall {
                    response(stream, "200 OK", "Content-Length: 10\r\n", b"a");
                }
                thread::sleep(Duration::from_millis(300));
                if !body_stall {
                    response(stream, "200 OK", "Content-Length: 1\r\n", b"a");
                }
            });
            let mut provider = fixture.provider();
            provider.timeout = Duration::from_millis(80);
            let start = Instant::now();
            assert_unavailable(provider.fetch_front(RELEASE));
            assert!(start.elapsed() < Duration::from_millis(250));
            assert_eq!(fixture.count(), 1);
        }
    }

    #[test]
    fn slow_body_progress_does_not_extend_deadline() {
        let fixture = Fixture::new(|_, _, stream| {
            response(stream, "200 OK", "Content-Length: 20\r\n", b"");
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(25));
                if stream.write_all(b"x").is_err() {
                    break;
                }
            }
        });
        let mut provider = fixture.provider();
        provider.timeout = Duration::from_millis(100);
        let start = Instant::now();
        assert_unavailable(provider.fetch_front(RELEASE));
        assert!(start.elapsed() < Duration::from_millis(300));
        assert_eq!(fixture.count(), 1);
    }

    #[test]
    fn redirects_do_not_reset_total_deadline() {
        let fixture = Fixture::new(|_, _, stream| {
            thread::sleep(Duration::from_millis(80));
            response(
                stream,
                "307 Temporary Redirect",
                "Location: /again\r\n",
                b"",
            );
        });
        let mut provider = fixture.provider();
        provider.timeout = Duration::from_millis(130);
        let start = Instant::now();
        assert_unavailable(provider.fetch_front(RELEASE));
        assert!(start.elapsed() < Duration::from_millis(240));
        assert!(fixture.count() <= 2);
    }

    #[test]
    fn clones_share_start_limiter_and_rate_wait_uses_total_deadline() {
        let fixture = Fixture::new(|_, _, stream| response(stream, "404 Not Found", "", b""));
        let mut provider = fixture.provider();
        provider.start_interval = Duration::from_millis(100);
        let clone = provider.clone();
        assert!(matches!(
            provider.fetch_front(RELEASE),
            FetchOutcome::Missing
        ));
        assert!(matches!(clone.fetch_front(RELEASE), FetchOutcome::Missing));
        let requests = fixture.requests.lock().unwrap();
        assert!(requests[1].0.duration_since(requests[0].0) >= Duration::from_millis(85));
        drop(requests);
        provider.timeout = Duration::from_millis(20);
        assert_unavailable(provider.fetch_front(RELEASE));
        assert_eq!(fixture.count(), 2);
    }
}
