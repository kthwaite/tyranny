use super::{
    Cancellation, Lyrics, LyricsSource, LyricsStatus, MAX_TEXT_BYTES,
    discovery::{Identity, has_timestamps},
    unix_time,
};
use anyhow::{Context, Result, ensure};
use reqwest::{
    StatusCode, Url,
    blocking::Client,
    header::{ACCEPT, CONTENT_TYPE, RETRY_AFTER},
};
use serde::Deserialize;
use std::{
    io::Read,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const TIMEOUT: Duration = Duration::from_secs(6);
const ORIGIN: &str = "https://lrclib.net/";
const USER_AGENT: &str = concat!(
    "Tyranny/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/kthwaite/tyranny)"
);

#[derive(Clone)]
pub(super) struct Provider {
    client: Client,
    // Held for the whole request: sequential fetches and a delay after completion.
    next_start: Arc<Mutex<u64>>,
    #[cfg(test)]
    fixture_origin: Option<Url>,
}
impl Provider {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            client: Self::builder().https_only(true).build()?,
            next_start: Arc::new(Mutex::new(0)),
            #[cfg(test)]
            fixture_origin: None,
        })
    }
    fn builder() -> reqwest::blocking::ClientBuilder {
        Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(TIMEOUT)
    }
    #[cfg(test)]
    pub(super) fn for_test(origin: &str) -> Result<Self> {
        let origin = Url::parse(origin)?;
        ensure!(
            origin.scheme() == "http"
                && origin
                    .host_str()
                    .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback())
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.path() == "/"
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "fixture must be loopback HTTP origin"
        );
        Ok(Self {
            client: Self::builder().no_proxy().build()?,
            next_start: Arc::new(Mutex::new(0)),
            fixture_origin: Some(origin),
        })
    }
    pub(super) fn fetch(
        &self,
        identity: &Identity,
        cancel: &Cancellation,
        persisted_retry: u64,
    ) -> (Lyrics, u64) {
        let deadline = Instant::now() + TIMEOUT;
        let mut gate = loop {
            if let Err(error) = cancel.check(deadline) {
                return (Lyrics::unavailable(error.to_string()), persisted_retry);
            }
            match self.next_start.try_lock() {
                Ok(gate) => break gate,
                Err(std::sync::TryLockError::WouldBlock) => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(_) => {
                    return (
                        Lyrics::unavailable("lyrics provider lock unavailable"),
                        persisted_retry,
                    );
                }
            }
        };
        *gate = (*gate).max(persisted_retry);
        // Delays from the server are persisted globally, not just per track.
        if *gate > unix_time() {
            return (
                Lyrics::unavailable("LRCLIB is temporarily rate limited"),
                *gate,
            );
        }
        let result = self.fetch_inner(identity, cancel, deadline, &mut gate);
        // Keep the gate for the full delay even when the requester disconnects;
        // cancellation must not let another track bypass provider throttling.
        thread::sleep(Duration::from_millis(500));
        (
            result.unwrap_or_else(|error| Lyrics::unavailable(format!("{error:#}"))),
            *gate,
        )
    }
    fn fetch_inner(
        &self,
        identity: &Identity,
        cancel: &Cancellation,
        deadline: Instant,
        retry: &mut u64,
    ) -> Result<Lyrics> {
        ensure!(
            identity.eligible(),
            "track metadata is insufficient for exact lyrics lookup"
        );
        let origin = ORIGIN;
        #[cfg(test)]
        let origin = self.fixture_origin.as_ref().map_or(origin, Url::as_str);
        let mut url = Url::parse(origin)?.join("api/get")?;
        url.query_pairs_mut()
            .append_pair("track_name", &identity.title)
            .append_pair("artist_name", &identity.artist)
            .append_pair("album_name", &identity.album)
            .append_pair("duration", &identity.duration.to_string());
        cancel.check(deadline)?;
        let mut response = self
            .client
            .get(url)
            .header(ACCEPT, "application/json")
            .timeout(deadline.saturating_duration_since(Instant::now()))
            .send()
            .context("LRCLIB could not be reached")?;
        cancel.check(deadline)?;
        match response.status() {
            StatusCode::NOT_FOUND => return Ok(Lyrics::missing()),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                let delay = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(60);
                // UUID randomness supplies positive jitter without another RNG dependency.
                let random = u128::from_le_bytes(*uuid::Uuid::new_v4().as_bytes()) as u64;
                let jitter = 1 + random % (delay / 4 + 1);
                *retry = unix_time()
                    .saturating_add(delay)
                    .saturating_add(jitter)
                    .min(i64::MAX as u64);
                anyhow::bail!("LRCLIB is temporarily rate limited");
            }
            StatusCode::OK => {}
            status if status.is_redirection() => anyhow::bail!("LRCLIB redirect rejected"),
            _ => anyhow::bail!("LRCLIB returned an unsuccessful response"),
        }
        ensure!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|s| s
                    .split(';')
                    .next()
                    .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))),
            "LRCLIB returned non-JSON content"
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= MAX_RESPONSE_BYTES as u64),
            "lyrics response exceeds size limit"
        );
        let mut bytes = Vec::with_capacity(response.content_length().unwrap_or(8192) as usize);
        let mut chunk = [0u8; 8192];
        loop {
            cancel.check(deadline)?;
            let count = response.read(&mut chunk).context("reading LRCLIB lyrics")?;
            cancel.check(deadline)?;
            if count == 0 {
                break;
            }
            ensure!(
                count <= MAX_RESPONSE_BYTES - bytes.len(),
                "lyrics response exceeds size limit"
            );
            bytes.extend_from_slice(&chunk[..count]);
        }
        parse(&bytes, identity)
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    id: u64,
    track_name: String,
    artist_name: String,
    album_name: String,
    duration: f64,
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}
fn parse(bytes: &[u8], identity: &Identity) -> Result<Lyrics> {
    let record: Record = serde_json::from_slice(bytes).context("invalid LRCLIB record")?;
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    ensure!(
        same(&record.track_name, &identity.title)
            && same(&record.artist_name, &identity.artist)
            && (identity.album.is_empty() || same(&record.album_name, &identity.album))
            && record.duration.is_finite()
            && (record.duration - identity.duration).abs() <= 2.0,
        "LRCLIB returned a different track identity"
    );
    let plain = record.plain_lyrics.filter(|text| !text.trim().is_empty());
    let synced = record.synced_lyrics.filter(|text| !text.trim().is_empty());
    ensure!(
        plain.as_ref().is_none_or(|s| s.len() <= MAX_TEXT_BYTES)
            && synced.as_ref().is_none_or(|s| s.len() <= MAX_TEXT_BYTES),
        "lyrics text exceeds size limit"
    );
    ensure!(
        synced.as_ref().is_none_or(|s| has_timestamps(s)),
        "LRCLIB returned invalid synchronized lyrics"
    );
    if !record.instrumental && plain.is_none() && synced.is_none() {
        return Ok(Lyrics::missing());
    }
    Ok(Lyrics {
        status: if record.instrumental {
            LyricsStatus::Instrumental
        } else {
            LyricsStatus::Ready
        },
        source: Some(LyricsSource::Lrclib),
        plain_text: if record.instrumental { None } else { plain },
        synced_lrc: if record.instrumental { None } else { synced },
        attribution_url: Some(format!("https://lrclib.net/tracks/{}", record.id)),
        message: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    pub(super) fn identity() -> Identity {
        Identity {
            title: "Title & Name".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration: 123.0,
        }
    }
    fn record() -> serde_json::Value {
        serde_json::json!({"id":42,"trackName":"Title & Name","artistName":"Artist","albumName":"Album","duration":123,"instrumental":false,"plainLyrics":"Words","syncedLyrics":"[00:01.00]Words"})
    }
    #[test]
    fn identity_instrumental_missing_and_bounds() {
        let id = identity();
        assert_eq!(
            parse(&serde_json::to_vec(&record()).unwrap(), &id)
                .unwrap()
                .status,
            LyricsStatus::Ready
        );
        let mut wrong = record();
        wrong["artistName"] = "Other".into();
        assert!(parse(&serde_json::to_vec(&wrong).unwrap(), &id).is_err());
        let mut wrong = record();
        wrong["duration"] = 126.into();
        assert!(parse(&serde_json::to_vec(&wrong).unwrap(), &id).is_err());
        let mut missing = record();
        missing["plainLyrics"] = serde_json::Value::Null;
        missing["syncedLyrics"] = serde_json::Value::Null;
        assert_eq!(
            parse(&serde_json::to_vec(&missing).unwrap(), &id)
                .unwrap()
                .status,
            LyricsStatus::Missing
        );
        missing["instrumental"] = true.into();
        assert_eq!(
            parse(&serde_json::to_vec(&missing).unwrap(), &id)
                .unwrap()
                .status,
            LyricsStatus::Instrumental
        );
        let mut oversized = record();
        oversized["plainLyrics"] = "x".repeat(MAX_TEXT_BYTES + 1).into();
        assert!(parse(&serde_json::to_vec(&oversized).unwrap(), &id).is_err());
    }
    fn fixture(response: String) -> (Provider, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let provider =
            Provider::for_test(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let thread = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
            }
            let _ = socket.write_all(response.as_bytes());
            String::from_utf8(request).unwrap()
        });
        (provider, thread)
    }
    #[test]
    fn exact_request_http_outcomes_redirects_and_global_backoff() {
        for (status, headers, body, expected) in [
            (
                "200 OK",
                "Content-Type: application/json\r\n",
                record().to_string(),
                LyricsStatus::Ready,
            ),
            ("404 Not Found", "", String::new(), LyricsStatus::Missing),
            (
                "500 Internal Server Error",
                "",
                String::new(),
                LyricsStatus::Unavailable,
            ),
            (
                "302 Found",
                "Location: http://192.0.2.1/unsafe\r\n",
                String::new(),
                LyricsStatus::Unavailable,
            ),
            (
                "429 Too Many Requests",
                "Retry-After: 7200\r\n",
                String::new(),
                LyricsStatus::Unavailable,
            ),
            (
                "503 Service Unavailable",
                "Retry-After: 7200\r\n",
                String::new(),
                LyricsStatus::Unavailable,
            ),
            (
                "200 OK",
                "Content-Type: application/json\r\n",
                "x".repeat(MAX_RESPONSE_BYTES + 1),
                LyricsStatus::Unavailable,
            ),
        ] {
            let response = format!(
                "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let (provider, server) = fixture(response);
            let (lyrics, retry) = provider.fetch(&identity(), &Cancellation::new(), 0);
            assert_eq!(lyrics.status, expected);
            let request = server.join().unwrap();
            assert!(request.starts_with("GET /api/get?track_name=Title+%26+Name&artist_name=Artist&album_name=Album&duration=123 "));
            assert!(request.contains(USER_AGENT));
            if status.starts_with("429") || status.starts_with("503") {
                assert!(retry >= unix_time() + 7199);
                assert_eq!(
                    provider
                        .fetch(&identity(), &Cancellation::new(), retry)
                        .0
                        .status,
                    LyricsStatus::Unavailable
                );
            }
        }
    }
}
