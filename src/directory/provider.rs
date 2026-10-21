use super::{Cancellation, mirrors};
use crate::protocol::{DirectoryStation, StreamSpec};
use anyhow::{Context, Result, bail, ensure};
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
    hosts: Vec<String>,
    resolved_at: Instant,
}

pub(crate) struct Provider {
    client: Client,
    mirrors: Mutex<Mirrors>,
    #[cfg(test)]
    fixture_origins: Option<Vec<Url>>,
}
impl Provider {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            client: Self::builder().https_only(true).build()?,
            mirrors: Mutex::new(Mirrors {
                hosts: Vec::new(),
                resolved_at: Instant::now(),
            }),
            #[cfg(test)]
            fixture_origins: None,
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
                hosts: Vec::new(),
                resolved_at: Instant::now(),
            }),
            fixture_origins: Some(origins),
        })
    }

    pub(crate) fn search(
        &self,
        query: &str,
        cancel: &Cancellation,
    ) -> Result<Vec<DirectoryStation>> {
        let deadline = Instant::now() + SEARCH_DEADLINE;
        let origins = self.origins(cancel, deadline)?;
        let mut last = None;
        for origin in origins {
            cancel.check(deadline)?;
            match self.search_one(&origin, query, cancel, deadline) {
                Ok(stations) => return Ok(stations),
                Err(error) => last = Some(error),
            }
        }
        cancel.check(deadline)?;
        self.forget_mirrors();
        let error = last.context("no radio-browser.info mirrors available")?;
        Err(error.context("radio-browser.info is unavailable"))
    }

    /// Best-effort: the API counts one click per IP, station and day.
    pub(crate) fn count_click(&self, uuid: &str) {
        if uuid.is_empty()
            || uuid.len() > MAX_UUID_BYTES
            || !uuid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return;
        }
        let _ = self.count_click_inner(uuid);
    }
    fn count_click_inner(&self, uuid: &str) -> Result<()> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let cancel = Cancellation::new(Default::default(), Default::default());
        let origin = self
            .origins(&cancel, deadline)?
            .into_iter()
            .next()
            .context("no mirrors")?;
        let url = origin.join(&format!("json/url/{uuid}"))?;
        let response = self.client.get(url).send()?;
        ensure!(response.status() == StatusCode::OK, "click not counted");
        read_bounded(response, MAX_CLICK_BYTES, &cancel, deadline)?;
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
            .append_pair("limit", "50")
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
            .context("mirror could not be reached")?;
        cancel.check(deadline)?;
        match response.status() {
            StatusCode::OK => {}
            status if status.is_redirection() => bail!("mirror redirect rejected"),
            status => bail!("mirror answered {status}"),
        }
        let bytes = read_bounded(response, MAX_SEARCH_BYTES, cancel, deadline)?;
        parse(&bytes)
    }

    fn origins(&self, cancel: &Cancellation, deadline: Instant) -> Result<Vec<Url>> {
        #[cfg(test)]
        if let Some(origins) = &self.fixture_origins {
            return Ok(origins.iter().take(MAX_HOSTS).cloned().collect());
        }
        let mut mirrors = self.mirrors.lock().unwrap_or_else(|p| p.into_inner());
        if mirrors.hosts.is_empty() || mirrors.resolved_at.elapsed() > MIRROR_TTL {
            cancel.check(deadline)?;
            mirrors.hosts = resolve_within(deadline)?;
            mirrors.resolved_at = Instant::now();
        }
        mirrors
            .hosts
            .iter()
            .take(MAX_HOSTS)
            .map(|host| Url::parse(&format!("https://{host}/")).map_err(Into::into))
            .collect()
    }
    fn forget_mirrors(&self) {
        self.mirrors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .hosts
            .clear();
    }
}

// DNS has no timeout of its own, so it runs on a helper thread the search can abandon.
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
        "response exceeds size limit"
    );
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        cancel.check(deadline)?;
        let count = response.read(&mut chunk).context("reading reply")?;
        if count == 0 {
            return Ok(bytes);
        }
        ensure!(count <= limit - bytes.len(), "response exceeds size limit");
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
    let records: Vec<serde_json::Value> =
        serde_json::from_slice(bytes).context("radio-browser.info sent an invalid reply")?;
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
    if uuid.is_empty()
        || uuid.len() > MAX_UUID_BYTES
        || !uuid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
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
