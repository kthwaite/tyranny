//! Rate-gated JSON GETs shared by the tagging providers.
use super::LookupError;
use crate::cancel::Cancellation;
use anyhow::Result;
use reqwest::{
    StatusCode, Url,
    blocking::{Client, ClientBuilder},
    header::{ACCEPT, CONTENT_TYPE, RETRY_AFTER},
};
use std::{
    io::Read,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

pub(super) const USER_AGENT: &str = concat!(
    "tyranny-likes-smoke/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/kthwaite/tyranny)"
);
pub(super) const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
const SLEEP_STEP: Duration = Duration::from_millis(20);

/// Spaces request starts by `interval`. Clones share one schedule.
#[derive(Clone)]
pub(super) struct StartGate {
    next: Arc<Mutex<Instant>>,
    interval: Duration,
    /// The instant each `wait` let its caller start, so tests can check the
    /// spacing without timing a server.
    #[cfg(test)]
    starts: Arc<Mutex<Vec<Instant>>>,
}
impl StartGate {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            next: Arc::new(Mutex::new(Instant::now())),
            interval,
            #[cfg(test)]
            starts: Arc::default(),
        }
    }
    /// The start instants `wait` has granted, in order.
    #[cfg(test)]
    pub(super) fn starts(&self) -> Vec<Instant> {
        self.starts.lock().unwrap().clone()
    }
    /// Blocks until this caller may start a request, and reserves the slot after it.
    pub(super) fn wait(&self, cancel: &Cancellation, deadline: Instant) -> Result<(), LookupError> {
        loop {
            cancel.check_lookup(deadline)?;
            let delay = {
                let mut next = self
                    .next
                    .lock()
                    .map_err(|_| LookupError::Failed("request gate unavailable".into()))?;
                let now = Instant::now();
                if now >= *next {
                    *next = now + self.interval;
                    #[cfg(test)]
                    self.starts.lock().unwrap().push(now);
                    return Ok(());
                }
                if *next >= deadline {
                    return Err(LookupError::TimedOut);
                }
                *next - now
            };
            thread::sleep(delay.min(SLEEP_STEP));
        }
    }
}

pub(super) fn client(https_only: bool) -> ClientBuilder {
    Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(REQUEST_TIMEOUT)
        .https_only(https_only)
}

/// Accepts only a plain `http://` loopback origin, for fixture servers in tests.
#[cfg(test)]
pub(super) fn loopback_origin(origin: &str) -> Result<Url> {
    let origin = Url::parse(origin)?;
    anyhow::ensure!(
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
    Ok(origin)
}

pub(super) fn get_json(
    client: &Client,
    gate: &StartGate,
    url: Url,
    service: &'static str,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<Vec<u8>, LookupError> {
    fetch(client, gate, url, service, cancel, deadline, false).map(|(_, body)| body)
}

/// As `get_json`, but a 4xx reply whose content type is JSON returns `Ok((status, body))` instead of `Failed`.
/// Busy statuses, redirects, size and transport errors map exactly as in `get_json`.
pub(super) fn get_json_or_error(
    client: &Client,
    gate: &StartGate,
    url: Url,
    service: &'static str,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<(u16, Vec<u8>), LookupError> {
    fetch(client, gate, url, service, cancel, deadline, true)
}

fn fetch(
    client: &Client,
    gate: &StartGate,
    url: Url,
    service: &'static str,
    cancel: &Cancellation,
    deadline: Instant,
    accept_error_json: bool,
) -> Result<(u16, Vec<u8>), LookupError> {
    let failed = |message: String| LookupError::Failed(message);
    let unreachable = || failed(format!("{service} could not be reached"));
    gate.wait(cancel, deadline)?;
    let mut response = client
        .get(url)
        .header(ACCEPT, "application/json")
        .timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(REQUEST_TIMEOUT),
        )
        .send()
        .map_err(|_| unreachable())?;
    cancel.check_lookup(deadline)?;
    let status = response.status();
    if status == StatusCode::SERVICE_UNAVAILABLE
        || status == StatusCode::TOO_MANY_REQUESTS
        || response.headers().contains_key(RETRY_AFTER)
    {
        return Err(LookupError::Busy(service));
    }
    let json = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split(';').next())
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"));
    let error_body = accept_error_json && status.is_client_error() && json;
    if !error_body {
        if status == StatusCode::NOT_FOUND {
            return Err(failed(format!("{service} has no such record")));
        }
        if status.is_redirection() {
            return Err(failed(format!("{service} redirect rejected")));
        }
        if status != StatusCode::OK {
            return Err(failed(format!(
                "{service} returned an unsuccessful response (HTTP {})",
                status.as_u16()
            )));
        }
        if !json {
            return Err(failed(format!("{service} returned non-JSON content")));
        }
    }
    let too_big = || failed(format!("{service} response exceeds 2 MiB"));
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(too_big());
    }
    let mut bytes = Vec::with_capacity(response.content_length().unwrap_or(8192) as usize);
    let mut chunk = [0u8; 8192];
    loop {
        cancel.check_lookup(deadline)?;
        let count = response.read(&mut chunk).map_err(|_| unreachable())?;
        if count == 0 {
            return Ok((status.as_u16(), bytes));
        }
        if count > MAX_RESPONSE_BYTES - bytes.len() {
            return Err(too_big());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}
