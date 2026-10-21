use super::{Cancellation, mirrors};
use crate::protocol::{DirectoryStation, StreamSpec};
use anyhow::{Context, Result, anyhow, bail, ensure};
use reqwest::{
    StatusCode, Url,
    blocking::{Client, Response},
    header::ACCEPT,
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    io::Read,
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const MAX_SEARCH_BYTES: usize = 512 * 1024;
const MAX_CLICK_BYTES: usize = 64 * 1024;
const SEARCH_DEADLINE: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MIRROR_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_HOSTS: usize = 3;
const MAX_RESULTS: usize = 50;
const MAX_TAGS: usize = 5;
const MAX_TAG_BYTES: usize = 32;
const MAX_CODEC_BYTES: usize = 16;
const MAX_UUID_BYTES: usize = 64;
pub(super) const USER_AGENT: &str = concat!(
    "Tyranny/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/kthwaite/tyranny)"
);

struct Mirrors {
    // Head first; a host that fails moves to the back.
    origins: Vec<Url>,
    resolved_at: Instant,
    // Test origins stand in for DNS: never re-resolved, never forgotten.
    #[cfg(test)]
    pinned: bool,
    // Counts the forgets a pinned list ignored.
    #[cfg(test)]
    forgets: usize,
}

pub(crate) struct Provider {
    client: Client,
    mirrors: Mutex<Mirrors>,
}
impl Provider {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            client: Self::builder().https_only(true).build()?,
            mirrors: Mutex::new(Mirrors {
                origins: Vec::new(),
                resolved_at: Instant::now(),
                #[cfg(test)]
                pinned: false,
                #[cfg(test)]
                forgets: 0,
            }),
        })
    }
    fn builder() -> reqwest::blocking::ClientBuilder {
        Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(REQUEST_TIMEOUT)
    }
    #[cfg(test)]
    pub(super) fn for_test(origins: Vec<Url>) -> Result<Self> {
        for origin in &origins {
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
        }
        Ok(Self {
            client: Self::builder().no_proxy().build()?,
            mirrors: Mutex::new(Mirrors {
                origins,
                resolved_at: Instant::now(),
                pinned: true,
                forgets: 0,
            }),
        })
    }

    pub(crate) fn search(
        &self,
        query: &str,
        cancel: &Cancellation,
    ) -> Result<Vec<DirectoryStation>> {
        self.search_mirrors(query, cancel, Instant::now() + SEARCH_DEADLINE)
    }
    fn search_mirrors(
        &self,
        query: &str,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<Vec<DirectoryStation>> {
        let mut last = None;
        for origin in self.origins(cancel, deadline)? {
            cancel.check(deadline)?;
            match self.search_one(&origin, query, cancel, deadline) {
                Ok(stations) => return Ok(stations),
                Err(error) => {
                    // Cancellation and the deadline are not the host's fault.
                    cancel.check(deadline)?;
                    self.demote(&origin);
                    last = Some(error);
                }
            }
        }
        // Every tried host failed on its own, so the cached list may be stale.
        // Cancellation and the deadline returned above and keep it.
        self.forget_mirrors();
        Err(last.unwrap_or_else(|| anyhow!("radio-browser.info has no mirrors available")))
    }

    /// Best-effort: the API counts one click per IP, station and day.
    /// Stops before sending if `cancel` is set, so shutdown waits for no new request.
    pub(crate) fn count_click(&self, uuid: &str, cancel: &Cancellation) {
        if !is_safe_uuid(uuid) {
            return;
        }
        let _ = self.count_click_inner(uuid, cancel);
    }
    fn count_click_inner(&self, uuid: &str, cancel: &Cancellation) -> Result<()> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let origin = self
            .origins(cancel, deadline)?
            .into_iter()
            .next()
            .context("no mirrors")?;
        let url = origin.join(&format!("json/url/{uuid}"))?;
        cancel.check(deadline)?;
        let response = self.client.get(url).send()?;
        ensure!(response.status() == StatusCode::OK, "click not counted");
        read_bounded(response, MAX_CLICK_BYTES, cancel, deadline)?;
        Ok(())
    }

    fn search_one(
        &self,
        origin: &Url,
        query: &str,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<Vec<DirectoryStation>> {
        let mut url = origin.join("json/stations/search")?;
        url.query_pairs_mut()
            .append_pair("name", query)
            .append_pair("limit", &MAX_RESULTS.to_string())
            .append_pair("hidebroken", "true")
            .append_pair("order", "clickcount")
            .append_pair("reverse", "true");
        // A literal '+' is already %2B, so every '+' left here is an encoded space.
        let encoded = url.query().unwrap_or_default().replace('+', "%20");
        url.set_query(Some(&encoded));
        let response = self
            .client
            .get(url)
            .header(ACCEPT, "application/json")
            .timeout(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(REQUEST_TIMEOUT),
            )
            .send()
            .map_err(|error| failure(origin, "request", &error, UNREACHABLE))?;
        cancel.check(deadline)?;
        match response.status() {
            StatusCode::OK => {}
            status if status.is_redirection() => {
                bail!("radio-browser.info sent a redirect. Try again.")
            }
            status => {
                eprintln!("tyranny: {origin} answered {status}");
                bail!("radio-browser.info answered with an error. Try again.")
            }
        }
        let bytes = read_bounded(response, MAX_SEARCH_BYTES, cancel, deadline)?;
        parse(&bytes)
    }

    pub(super) fn origins(&self, cancel: &Cancellation, deadline: Instant) -> Result<Vec<Url>> {
        let stale = {
            let mirrors = self.mirrors.lock().unwrap_or_else(|p| p.into_inner());
            #[cfg(test)]
            let pinned = mirrors.pinned;
            #[cfg(not(test))]
            let pinned = false;
            !pinned && (mirrors.origins.is_empty() || mirrors.resolved_at.elapsed() > MIRROR_TTL)
        };
        if stale {
            cancel.check(deadline)?;
            // Resolve outside the lock so a slow lookup blocks no one else.
            let origins = resolve_within(deadline)?
                .iter()
                .map(|host| Url::parse(&format!("https://{host}/")).map_err(Into::into))
                .collect::<Result<Vec<Url>>>()?;
            let mut mirrors = self.mirrors.lock().unwrap_or_else(|p| p.into_inner());
            mirrors.origins = origins;
            mirrors.resolved_at = Instant::now();
            return Ok(mirrors.origins.iter().take(MAX_HOSTS).cloned().collect());
        }
        let mirrors = self.mirrors.lock().unwrap_or_else(|p| p.into_inner());
        Ok(mirrors.origins.iter().take(MAX_HOSTS).cloned().collect())
    }
    #[cfg(test)]
    pub(super) fn forgets(&self) -> usize {
        self.mirrors.lock().unwrap().forgets
    }
    fn demote(&self, failed: &Url) {
        let mut mirrors = self.mirrors.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(index) = mirrors.origins.iter().position(|o| o == failed) {
            let origin = mirrors.origins.remove(index);
            mirrors.origins.push(origin);
        }
    }
    fn forget_mirrors(&self) {
        let mut mirrors = self.mirrors.lock().unwrap_or_else(|p| p.into_inner());
        #[cfg(test)]
        if mirrors.pinned {
            mirrors.forgets += 1;
            return;
        }
        mirrors.origins.clear();
    }
}

const UNREACHABLE: &str = "radio-browser.info could not be reached. Try again.";

/// reqwest's Display includes the full request URL, which has no place in a message the user
/// sees. The detail goes to stderr, like the owner's other background failures.
fn failure(origin: &Url, what: &str, error: &dyn std::fmt::Display, shown: &str) -> anyhow::Error {
    eprintln!("tyranny: directory {what} to {origin} failed: {error}");
    anyhow!("{shown}")
}

fn is_safe_uuid(uuid: &str) -> bool {
    !uuid.is_empty()
        && uuid.len() <= MAX_UUID_BYTES
        && uuid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

// DNS has no timeout of its own, so it runs on a helper thread the search can abandon.
// A hung resolver leaves its thread behind, one per search that had to resolve; a failed
// search forgets the cache, so that can be one per failed search.
fn resolve_within(deadline: Instant) -> Result<Vec<String>> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("directory-dns".into())
        .spawn(move || {
            let _ = sender.send(mirrors::resolve());
        })?;
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .context("radio-browser.info mirror lookup timed out")?
}

fn read_bounded(
    mut response: Response,
    limit: usize,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().is_none_or(|n| n <= limit as u64),
        "radio-browser.info sent too large a reply."
    );
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        cancel.check(deadline)?;
        let count = response.read(&mut chunk).map_err(|error| {
            eprintln!("tyranny: reading a directory reply failed: {error}");
            anyhow!(UNREACHABLE)
        })?;
        if count == 0 {
            return Ok(bytes);
        }
        ensure!(
            count <= limit - bytes.len(),
            "radio-browser.info sent too large a reply."
        );
        bytes.extend_from_slice(&chunk[..count]);
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawStation {
    stationuuid: Option<String>,
    name: Option<String>,
    url: Option<String>,
    url_resolved: Option<String>,
    hls: Option<f64>,
    bitrate: Option<f64>,
    codec: Option<String>,
    countrycode: Option<String>,
    tags: Option<String>,
}

fn parse(bytes: &[u8]) -> Result<Vec<DirectoryStation>> {
    let records: Vec<serde_json::Value> = serde_json::from_slice(bytes).map_err(|error| {
        eprintln!("tyranny: directory reply is not a JSON list: {error}");
        anyhow!("radio-browser.info sent an invalid reply.")
    })?;
    let mut seen = HashSet::new();
    let mut stations = Vec::new();
    for record in records {
        if stations.len() == MAX_RESULTS {
            break;
        }
        let Ok(raw) = serde_json::from_value::<RawStation>(record) else {
            continue;
        };
        if let Some(station) = convert(raw)
            && seen.insert(station.url.clone())
        {
            stations.push(station);
        }
    }
    Ok(stations)
}

fn convert(raw: RawStation) -> Option<DirectoryStation> {
    // The FFmpeg build has no HLS demuxer.
    if raw.hls == Some(1.0) {
        return None;
    }
    let uuid = raw.stationuuid.unwrap_or_default();
    if !is_safe_uuid(&uuid) {
        return None;
    }
    let resolved = raw.url_resolved.unwrap_or_default();
    let url = if resolved.trim().is_empty() {
        raw.url.unwrap_or_default()
    } else {
        resolved
    };
    let spec = StreamSpec::new(url.trim().to_owned(), raw.name.unwrap_or_default()).ok()?;
    let country_code = raw.countrycode.unwrap_or_default();
    Some(DirectoryStation {
        uuid,
        name: spec.title,
        url: spec.url,
        codec: truncate(raw.codec.unwrap_or_default(), MAX_CODEC_BYTES),
        bitrate: raw
            .bitrate
            .map_or(0, |b| b.clamp(0.0, u32::MAX as f64) as u32),
        country_code: if country_code.len() == 2
            && country_code.bytes().all(|b| b.is_ascii_alphabetic())
        {
            country_code
        } else {
            String::new()
        },
        tags: raw
            .tags
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .take(MAX_TAGS)
            .map(|tag| truncate(tag.to_owned(), MAX_TAG_BYTES))
            .collect(),
    })
}

fn truncate(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}
