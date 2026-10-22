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
    "tyranny-likes-smoke/",
    env!("CARGO_PKG_VERSION"),
    " (development artwork client)"
);

#[derive(Clone)]
pub(super) struct Provider {
    client: Client,
    next_start: Arc<Mutex<Instant>>,
    discogs_retry_after: Arc<Mutex<Instant>>,
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

pub(super) struct RemoteImage {
    pub bytes: Vec<u8>,
    pub source_url: String,
}

type RemoteResult = Result<Option<RemoteImage>, &'static str>;

#[derive(Clone, Copy)]
enum Endpoint {
    DeezerApi,
    DeezerImage,
    DiscogsApi,
    DiscogsImage,
}

impl Provider {
    pub(super) fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: client_builder().https_only(true).build()?,
            next_start: Arc::new(Mutex::new(Instant::now())),
            discogs_retry_after: Arc::new(Mutex::new(Instant::now())),
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
            discogs_retry_after: Arc::new(Mutex::new(Instant::now())),
            timeout: REQUEST_TIMEOUT,
            start_interval: START_INTERVAL,
            fixture_origin: Some(origin),
        })
    }

    pub(super) fn before(&self, deadline: Instant) -> Self {
        let mut provider = self.clone();
        provider.timeout = provider
            .timeout
            .min(deadline.saturating_duration_since(Instant::now()));
        provider
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

    pub(super) fn fetch_deezer(&self, album: &str, artist: &str) -> RemoteResult {
        let deadline = Instant::now() + self.timeout;
        let mut url = self.api_url(Endpoint::DeezerApi, "search/album")?;
        url.query_pairs_mut()
            .append_pair("q", &format!("{artist} {album}"))
            .append_pair("limit", "25");
        let Some(bytes) = self.get(url, Endpoint::DeezerApi, None, deadline, 512 * 1024)? else {
            return Ok(None);
        };
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "Deezer returned invalid metadata")?;
        if !json["error"].is_null() {
            return Err("Deezer lookup failed");
        }
        let results = json["data"]
            .as_array()
            .ok_or("Deezer returned invalid metadata")?;
        if json["total"].as_u64().is_none_or(|total| total > 25) || results.len() > 25 {
            return Ok(None);
        }
        let mut found = None;
        for result in results {
            if !same_name(result["title"].as_str(), album)
                || !same_name(result["artist"]["name"].as_str(), artist)
            {
                continue;
            }
            let id = result["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .ok_or("Deezer returned invalid album identity")?;
            if let Some((previous, _)) = found
                && previous != id
            {
                return Ok(None);
            }
            found = Some((id, result));
        }
        let Some((id, result)) = found else {
            return Ok(None);
        };
        let image = result["cover_big"]
            .as_str()
            .or_else(|| result["cover_medium"].as_str())
            .ok_or("Deezer returned no album image")?;
        let url = Url::parse(image).map_err(|_| "Deezer returned an unsafe image URL")?;
        let bytes = self.get(url, Endpoint::DeezerImage, None, deadline, MAX_SOURCE_BYTES)?;
        Ok(bytes.map(|bytes| RemoteImage {
            bytes,
            source_url: format!("https://www.deezer.com/album/{id}"),
        }))
    }

    pub(super) fn fetch_discogs(&self, album: &str, artist: &str, token: &str) -> RemoteResult {
        let deadline = Instant::now() + self.timeout;
        let mut url = self.api_url(Endpoint::DiscogsApi, "database/search")?;
        url.query_pairs_mut()
            .append_pair("type", "release")
            .append_pair("artist", artist)
            .append_pair("release_title", album)
            .append_pair("per_page", "2");
        let Some(bytes) = self.get(url, Endpoint::DiscogsApi, Some(token), deadline, 512 * 1024)?
        else {
            return Ok(None);
        };
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "Discogs returned invalid metadata")?;
        let results = json["results"]
            .as_array()
            .ok_or("Discogs returned invalid metadata")?;
        // Search editions cannot be disambiguated by album/artist alone.
        if json["pagination"]["items"].as_u64() != Some(1) || results.len() != 1 {
            return Ok(None);
        }
        let id = results[0]["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or("Discogs returned invalid release identity")?;
        let url = self.api_url(Endpoint::DiscogsApi, &format!("releases/{id}"))?;
        let Some(bytes) = self.get(url, Endpoint::DiscogsApi, Some(token), deadline, 512 * 1024)?
        else {
            return Ok(None);
        };
        let release: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "Discogs returned invalid metadata")?;
        let artists = release["artists"]
            .as_array()
            .ok_or("Discogs returned invalid artists")?;
        if release["id"].as_u64() != Some(id)
            || !same_name(release["title"].as_str(), album)
            || artists.len() != 1
            || !same_name(artists[0]["name"].as_str(), artist)
        {
            return Ok(None);
        }
        let Some(images) = release["images"].as_array() else {
            return Ok(None);
        };
        let mut primary = images
            .iter()
            .filter(|image| image["type"].as_str() == Some("primary"));
        let Some(image) = primary.next() else {
            return Ok(None);
        };
        if primary.next().is_some() {
            return Ok(None);
        }
        let url = Url::parse(
            image["uri"]
                .as_str()
                .ok_or("Discogs returned no image URL")?,
        )
        .map_err(|_| "Discogs returned an unsafe image URL")?;
        // Authentication is restricted to API requests, never artwork/CDN requests.
        let bytes = self.get(
            url,
            Endpoint::DiscogsImage,
            None,
            deadline,
            MAX_SOURCE_BYTES,
        )?;
        Ok(bytes.map(|bytes| RemoteImage {
            bytes,
            source_url: format!("https://www.discogs.com/release/{id}"),
        }))
    }

    fn api_url(&self, endpoint: Endpoint, path: &str) -> Result<Url, &'static str> {
        let origin = match endpoint {
            Endpoint::DeezerApi => "https://api.deezer.com/",
            Endpoint::DiscogsApi => "https://api.discogs.com/",
            _ => return Err("invalid artwork API"),
        };
        #[cfg(test)]
        let origin = self.fixture_origin.as_ref().map_or(origin, Url::as_str);
        Url::parse(origin)
            .and_then(|url| url.join(path))
            .map_err(|_| "invalid artwork API URL")
    }

    fn allowed_url(&self, url: &Url, endpoint: Endpoint) -> bool {
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return false;
        }
        #[cfg(test)]
        if let Some(origin) = &self.fixture_origin {
            return url.origin() == origin.origin();
        }
        if url.scheme() != "https" || url.port().is_some() {
            return false;
        }
        matches!(
            (endpoint, url.host_str()),
            (Endpoint::DeezerApi, Some("api.deezer.com"))
                | (
                    Endpoint::DeezerImage,
                    Some("api.deezer.com" | "cdn-images.dzcdn.net")
                )
                | (Endpoint::DiscogsApi, Some("api.discogs.com"))
                | (Endpoint::DiscogsImage, Some("i.discogs.com"))
        )
    }

    fn get(
        &self,
        mut url: Url,
        endpoint: Endpoint,
        token: Option<&str>,
        deadline: Instant,
        limit: usize,
    ) -> Result<Option<Vec<u8>>, &'static str> {
        for redirects in 0..=MAX_REDIRECTS {
            if !self.allowed_url(&url, endpoint) {
                return Err("Artwork provider returned an unsafe URL");
            }
            if matches!(endpoint, Endpoint::DiscogsApi)
                && self
                    .discogs_retry_after
                    .lock()
                    .map_or(true, |next| *next > Instant::now())
            {
                return Err("Discogs is temporarily rate limited");
            }
            if !self.wait_for_start(deadline) {
                return Err("Artwork request is temporarily rate limited");
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("Artwork request timed out")?;
            let mut request = self.client.get(url).timeout(remaining);
            if let Some(token) = token {
                if !matches!(endpoint, Endpoint::DiscogsApi) {
                    return Err("Artwork credentials require the Discogs API");
                }
                let mut header =
                    reqwest::header::HeaderValue::from_str(&format!("Discogs token={token}"))
                        .map_err(|_| "Invalid Discogs credentials")?;
                header.set_sensitive(true);
                request = request.header(reqwest::header::AUTHORIZATION, header);
            }
            let mut response = request
                .send()
                .map_err(|_| "Artwork provider could not be reached")?;
            if matches!(endpoint, Endpoint::DiscogsApi) {
                let exhausted = response
                    .headers()
                    .get("x-discogs-ratelimit-remaining")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    == Some(0);
                if exhausted || response.status() == StatusCode::TOO_MANY_REQUESTS {
                    let seconds = response
                        .headers()
                        .get("retry-after")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(60)
                        .clamp(1, 3600);
                    if let Ok(mut next) = self.discogs_retry_after.lock() {
                        *next = (*next).max(Instant::now() + Duration::from_secs(seconds));
                    }
                }
            }
            match response.status() {
                StatusCode::NOT_FOUND => return Ok(None),
                StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT => {
                    if redirects == MAX_REDIRECTS {
                        return Err("Artwork provider redirected too many times");
                    }
                    url = response
                        .headers()
                        .get(LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|location| response.url().join(location).ok())
                        .ok_or("Artwork provider returned an unsafe redirect")?;
                    continue;
                }
                StatusCode::OK => {}
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    return Err(if matches!(endpoint, Endpoint::DiscogsApi) {
                        "Discogs rejected credentials or access"
                    } else {
                        "Artwork provider rejected credentials or access"
                    });
                }
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                    return Err("Artwork provider is temporarily rate limited");
                }
                _ => return Err("Artwork provider returned an unsuccessful response"),
            }
            if response
                .content_length()
                .is_some_and(|length| length > limit as u64)
            {
                return Err("Artwork response exceeds the size limit");
            }
            let mut bytes =
                Vec::with_capacity(response.content_length().unwrap_or(16 * 1024) as usize);
            let mut chunk = [0u8; 16 * 1024];
            loop {
                if Instant::now() >= deadline {
                    return Err("Artwork request timed out");
                }
                let count = response
                    .read(&mut chunk)
                    .map_err(|_| "Artwork response could not be downloaded")?;
                if Instant::now() >= deadline {
                    return Err("Artwork request timed out");
                }
                if count == 0 {
                    return if bytes.is_empty() {
                        Err("Artwork provider returned an empty response")
                    } else {
                        Ok(Some(bytes))
                    };
                }
                if count > limit - bytes.len() {
                    return Err("Artwork response exceeds the size limit");
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
        }
        Err("Artwork provider redirected too many times")
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

fn same_name(candidate: Option<&str>, expected: &str) -> bool {
    candidate.is_some_and(|candidate| {
        !expected.trim().is_empty()
            && candidate.trim().to_lowercase() == expected.trim().to_lowercase()
    })
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

    fn json_response(stream: &mut TcpStream, json: serde_json::Value) {
        let body = serde_json::to_vec(&json).unwrap();
        response(
            stream,
            "200 OK",
            &format!("Content-Length: {}\r\n", body.len()),
            &body,
        );
    }

    #[test]
    fn new_provider_urls_restrict_every_origin_and_reject_credentials_and_fragments() {
        let provider = Provider::new().unwrap();
        for (endpoint, valid) in [
            (
                Endpoint::DeezerApi,
                "https://api.deezer.com/search/album?q=test",
            ),
            (
                Endpoint::DeezerImage,
                "https://cdn-images.dzcdn.net/images/cover/test.jpg",
            ),
            (
                Endpoint::DeezerImage,
                "https://api.deezer.com/album/1/image",
            ),
            (Endpoint::DiscogsApi, "https://api.discogs.com/releases/42"),
            (
                Endpoint::DiscogsImage,
                "https://i.discogs.com/signed/image.jpg?signature=unchanged",
            ),
        ] {
            let url = Url::parse(valid).unwrap();
            assert!(provider.allowed_url(&url, endpoint));
            for unsafe_url in [
                valid.replacen("https:", "http:", 1),
                valid.replacen("https://", "https://secret@", 1),
                format!("{valid}#fragment"),
                "https://127.0.0.1/image".into(),
                "https://i.discogs.com.attacker.invalid/image".into(),
                "https://api.discogs.com:8443/releases/42".into(),
                "file:///private/token".into(),
            ] {
                assert!(
                    !provider.allowed_url(&Url::parse(&unsafe_url).unwrap(), endpoint),
                    "{unsafe_url}"
                );
            }
        }
        assert!(!provider.allowed_url(
            &Url::parse("https://i.discogs.com/image").unwrap(),
            Endpoint::DiscogsApi
        ));
        assert!(!provider.allowed_url(
            &Url::parse("https://api.discogs.com/releases/42").unwrap(),
            Endpoint::DiscogsImage
        ));
    }

    #[test]
    fn deezer_checks_full_identity_and_rejects_ambiguous_or_truncated_results() {
        for json in [
            serde_json::json!({"total":1,"data":[{"id":1,"title":"Discovery","artist":{"name":"Data Punk"}}]}),
            serde_json::json!({"total":1,"data":[{"id":1,"title":"Discovery (Deluxe)","artist":{"name":"Daft Punk"}}]}),
            serde_json::json!({"total":2,"data":[
                {"id":1,"title":"Discovery","artist":{"name":"Daft Punk"}},
                {"id":2,"title":"Discovery","artist":{"name":"Daft Punk"}}]}),
            serde_json::json!({"total":26,"data":[{"id":1,"title":"Discovery","artist":{"name":"Daft Punk"}}]}),
        ] {
            let fixture = Fixture::new(move |_, request, stream| {
                assert!(request.starts_with("GET /search/album?q=Daft+Punk+Discovery&limit=25 "));
                assert!(!request.to_lowercase().contains("authorization:"));
                json_response(stream, json.clone());
            });
            assert!(
                fixture
                    .provider()
                    .fetch_deezer("Discovery", "Daft Punk")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(fixture.count(), 1);
        }
    }

    #[test]
    fn deezer_errors_malformed_metadata_and_unsafe_images_are_not_missing_successes() {
        for json in [
            serde_json::json!({"error":{"message":"untrusted provider diagnostic","code":4}}),
            serde_json::json!({"data":"wrong shape"}),
            serde_json::json!({"total":1,"data":[{"id":1,"title":"Discovery","artist":{"name":"Daft Punk"},
                "cover_big":"http://169.254.169.254/latest/meta-data"}]}),
        ] {
            let fixture = Fixture::new(move |_, _, stream| json_response(stream, json.clone()));
            let error = fixture
                .provider()
                .fetch_deezer("Discovery", "Daft Punk")
                .err()
                .unwrap();
            assert!(!error.contains("untrusted"));
            assert_eq!(fixture.count(), 1);
        }
    }

    #[test]
    fn discogs_refuses_ambiguous_editions_wrong_identity_and_nonprimary_images() {
        for case in 0..5 {
            let fixture = Fixture::new(move |index, _, stream| {
                if index == 0 {
                    json_response(
                        stream,
                        if case == 0 {
                            serde_json::json!({"pagination":{"items":2},"results":[{"id":1},{"id":2}]})
                        } else {
                            serde_json::json!({"pagination":{"items":1},"results":[{"id":42}]})
                        },
                    );
                } else {
                    let url = format!("http://{}/image", stream.local_addr().unwrap());
                    let mut json = serde_json::json!({"id":42,"title":"Discovery","artists":[{"name":"Daft Punk"}],
                        "images":[{"type":"primary","uri":url}]});
                    match case {
                        1 => json["title"] = serde_json::json!("Discovery (Deluxe)"),
                        2 => json["artists"][0]["name"] = serde_json::json!("Data Punk"),
                        3 => json["images"][0]["type"] = serde_json::json!("secondary"),
                        4 => {
                            json["images"] = serde_json::json!([
                            {"type":"primary","uri":url},{"type":"primary","uri":url}])
                        }
                        _ => unreachable!(),
                    }
                    json_response(stream, json);
                }
            });
            assert!(
                fixture
                    .provider()
                    .fetch_discogs("Discovery", "Daft Punk", "secret")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(fixture.count(), if case == 0 { 1 } else { 2 });
        }
    }

    #[test]
    fn discogs_authentication_is_api_only_including_redirected_image_requests() {
        let fixture = Fixture::new(|index, request, stream| {
            if index < 2 {
                assert!(
                    request
                        .to_lowercase()
                        .contains("authorization: discogs token=secret")
                );
            } else {
                assert!(!request.to_lowercase().contains("authorization:"));
                assert!(!request.contains("secret"));
            }
            match index {
                0 => json_response(
                    stream,
                    serde_json::json!({"pagination":{"items":1},"results":[{"id":42}]}),
                ),
                1 => {
                    let url = format!(
                        "http://{}/image?signed=keep%2Fthis",
                        stream.local_addr().unwrap()
                    );
                    json_response(
                        stream,
                        serde_json::json!({"id":42,"title":"Discovery","artists":[{"name":"Daft Punk"}],
                        "images":[{"type":"primary","uri":url}]}),
                    );
                }
                2 => {
                    assert!(request.starts_with("GET /image?signed=keep%2Fthis "));
                    response(stream, "302 Found", "Location: /cdn-image\r\n", b"");
                }
                3 => response(stream, "200 OK", "Content-Length: 5\r\n", b"image"),
                _ => panic!("unexpected request"),
            }
        });
        let image = fixture
            .provider()
            .fetch_discogs("Discovery", "Daft Punk", "secret")
            .unwrap()
            .unwrap();
        assert_eq!(image.bytes, b"image");
        assert_eq!(image.source_url, "https://www.discogs.com/release/42");
        assert_eq!(fixture.count(), 4);
    }

    #[test]
    fn authenticated_api_redirects_cannot_escape_to_another_origin() {
        let fixture = Fixture::new(|_, _, stream| {
            response(
                stream,
                "302 Found",
                "Location: http://127.0.0.1:1/steal\r\n",
                b"",
            );
        });
        let error = fixture
            .provider()
            .fetch_discogs("Discovery", "Daft Punk", "secret")
            .err()
            .unwrap();
        assert!(error.contains("unsafe"));
        assert!(!error.contains("secret"));
        assert_eq!(fixture.count(), 1);
    }

    #[test]
    fn provider_limits_metadata_and_preserves_one_multistage_deadline() {
        let oversized = Fixture::new(|_, _, stream| {
            response(stream, "200 OK", "Content-Length: 524289\r\n", b"")
        });
        assert!(
            oversized
                .provider()
                .fetch_deezer("Discovery", "Daft Punk")
                .err()
                .unwrap()
                .contains("size limit")
        );
        let delayed = Fixture::new(|index, _, stream| {
            thread::sleep(Duration::from_millis(90));
            if index == 0 {
                json_response(
                    stream,
                    serde_json::json!({"pagination":{"items":1},"results":[{"id":42}]}),
                );
            } else {
                let url = format!("http://{}/image", stream.local_addr().unwrap());
                json_response(
                    stream,
                    serde_json::json!({"id":42,"title":"Discovery","artists":[{"name":"Daft Punk"}],
                    "images":[{"type":"primary","uri":url}]}),
                );
            }
        });
        let mut provider = delayed.provider();
        provider.timeout = Duration::from_millis(140);
        let start = Instant::now();
        assert!(
            provider
                .fetch_discogs("Discovery", "Daft Punk", "secret")
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_millis(260));
        assert!(delayed.count() <= 2);
    }

    #[test]
    fn discogs_rate_headers_pause_discogs_without_blocking_other_providers() {
        let fixture = Fixture::new(|_, request, stream| {
            if request.starts_with("GET /database/search?") {
                response(
                    stream,
                    "429 Too Many Requests",
                    "Retry-After: 60\r\nX-Discogs-Ratelimit-Remaining: 0\r\n",
                    b"",
                );
            } else {
                response(stream, "404 Not Found", "", b"");
            }
        });
        let provider = fixture.provider();
        assert!(
            provider
                .fetch_discogs("Discovery", "Daft Punk", "secret")
                .is_err()
        );
        assert!(
            provider
                .fetch_discogs("Discovery", "Daft Punk", "secret")
                .is_err()
        );
        assert_eq!(fixture.count(), 1);
        assert!(matches!(
            provider.fetch_front(RELEASE),
            FetchOutcome::Missing
        ));
        assert_eq!(fixture.count(), 2);
    }

    #[test]
    fn exhausted_overall_budget_never_starts_a_new_provider_request() {
        let fixture = Fixture::new(|_, _, _| panic!("expired work must not reach the network"));
        let provider = fixture.provider().before(Instant::now());
        assert!(matches!(
            provider.fetch_front(RELEASE),
            FetchOutcome::Unavailable(_)
        ));
        assert!(provider.fetch_deezer("Discovery", "Daft Punk").is_err());
        assert!(
            provider
                .fetch_discogs("Discovery", "Daft Punk", "secret")
                .is_err()
        );
        assert_eq!(fixture.count(), 0);
    }
}
