//! AcoustID v2 lookup: a chromaprint fingerprint to the release IDs it votes for.
use super::{
    LookupError,
    fingerprint::Fingerprint,
    http::{self, StartGate},
};
use crate::cancel::Cancellation;
use anyhow::Result;
use reqwest::{Url, blocking::Client};
use serde::Deserialize;
use std::time::{Duration, Instant};

const SERVICE: &str = "AcoustID";
const ORIGIN: &str = "https://api.acoustid.org/";
const START_INTERVAL: Duration = Duration::from_millis(334);
const MAX_CANDIDATES: usize = 5;

pub(super) struct AcoustId {
    client: Client,
    gate: StartGate,
    origin: Url,
}

impl AcoustId {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            client: http::client(true).build()?,
            gate: StartGate::new(START_INTERVAL),
            origin: Url::parse(ORIGIN)?,
        })
    }
    #[cfg(test)]
    pub(super) fn for_test(origin: &str, interval: Duration) -> Result<Self> {
        Ok(Self {
            client: http::client(false).no_proxy().build()?,
            gate: StartGate::new(interval),
            origin: http::loopback_origin(origin)?,
        })
    }
    /// Release IDs this file votes for, deduplicated, in reply order.
    /// No error message contains `key`.
    pub(super) fn lookup(
        &self,
        key: &str,
        print: &Fingerprint,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<Vec<String>, LookupError> {
        let mut url = self
            .origin
            .join("v2/lookup")
            .map_err(|_| LookupError::Failed(format!("{SERVICE} address is not valid")))?;
        // A space in a query value is sent as `+`, which is how AcoustID joins `meta` values.
        url.query_pairs_mut()
            .append_pair("client", key)
            .append_pair("meta", "recordingids releaseids")
            .append_pair("duration", &print.duration_s.to_string())
            .append_pair("fingerprint", &print.encoded)
            .append_pair("format", "json");
        let (status, bytes) =
            http::get_json_or_error(&self.client, &self.gate, url, SERVICE, cancel, deadline)?;
        let reply: Reply = serde_json::from_slice(&bytes)
            .map_err(|_| LookupError::Failed(format!("{SERVICE} returned an unreadable reply")))?;
        if reply.status.as_deref() != Some("ok") {
            return Err(LookupError::Failed(match reply.error {
                Some(ErrorBody {
                    message: Some(message),
                }) => format!("{SERVICE}: {message}"),
                _ => format!("{SERVICE} returned an unsuccessful response (HTTP {status})"),
            }));
        }
        let mut ids: Vec<String> = Vec::new();
        for id in reply
            .results
            .iter()
            .flat_map(|r| &r.recordings)
            .flat_map(|r| &r.releases)
            .map(|r| &r.id)
        {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        Ok(ids)
    }
}

#[derive(Deserialize)]
struct Reply {
    status: Option<String>,
    error: Option<ErrorBody>,
    #[serde(default)]
    results: Vec<Match>,
}
#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
}
#[derive(Deserialize)]
struct Match {
    #[serde(default)]
    recordings: Vec<Recording>,
}
#[derive(Deserialize)]
struct Recording {
    #[serde(default)]
    releases: Vec<ReleaseRef>,
}
#[derive(Deserialize)]
struct ReleaseRef {
    id: String,
}

/// Counts one vote per file per release. Most votes first, ties in first-seen order; at most five.
pub(super) fn rank_releases(per_file: &[Vec<String>]) -> Vec<(String, u32)> {
    let mut tally: Vec<(String, u32)> = Vec::new();
    for id in per_file.iter().flatten() {
        match tally.iter_mut().find(|(seen, _)| seen == id) {
            Some((_, votes)) => *votes += 1,
            None => tally.push((id.clone(), 1)),
        }
    }
    tally.sort_by_key(|(_, votes)| std::cmp::Reverse(*votes));
    tally.truncate(MAX_CANDIDATES);
    tally
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{HttpFixture, fixture, json};
    use super::*;
    use std::time::Duration;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";
    const D: &str = "44444444-4444-4444-8444-444444444444";
    const E: &str = "55555555-5555-4555-8555-555555555555";
    const F: &str = "66666666-6666-4666-8666-666666666666";
    const G: &str = "77777777-7777-4777-8777-777777777777";

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }
    fn print() -> Fingerprint {
        Fingerprint {
            encoded: "AQAAexample_-".into(),
            duration_s: 10,
        }
    }
    fn provider(fixture: &HttpFixture, interval: Duration) -> AcoustId {
        AcoustId::for_test(&fixture.origin(), interval).unwrap()
    }
    fn v(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lookup_request_shape_and_release_ids() {
        let fixture = HttpFixture::new(|_| json(200, fixture("acoustid.json")));
        let ids = provider(&fixture, Duration::ZERO)
            .lookup("KEY123", &print(), &Cancellation::new(), deadline())
            .unwrap();
        assert_eq!(ids, v(&[B, A]));
        let seen = fixture.seen();
        assert_eq!(seen.len(), 1);
        let target = &seen[0].target;
        for part in [
            "/v2/lookup?",
            "client=KEY123",
            "meta=recordingids+releaseids",
            "duration=10",
            "format=json",
            "fingerprint=AQAAexample_-",
        ] {
            assert!(target.contains(part), "{part} missing from {target}");
        }
    }

    #[test]
    fn acoustid_errors_are_readable_and_never_echo_the_key() {
        for status in [400, 200] {
            let fixture = HttpFixture::new(move |_| json(status, fixture("acoustid-error.json")));
            let err = provider(&fixture, Duration::ZERO)
                .lookup("KEY123", &print(), &Cancellation::new(), deadline())
                .unwrap_err();
            let text = err.to_string();
            assert_eq!(text, "AcoustID: invalid API key", "HTTP {status}");
            assert!(!text.contains("KEY123"));
        }
    }

    #[test]
    fn gate_spaces_acoustid_requests_by_334_ms() {
        let fixture = HttpFixture::new(|_| json(200, fixture("acoustid.json")));
        let acoustid = provider(&fixture, Duration::from_millis(334));
        for _ in 0..3 {
            acoustid
                .lookup("KEY123", &print(), &Cancellation::new(), deadline())
                .unwrap();
        }
        assert_eq!(fixture.seen().len(), 3);
        // The gate's own start instants: server arrival times jitter under load.
        let starts = acoustid.gate.starts();
        assert_eq!(starts.len(), 3);
        for pair in starts.windows(2) {
            assert!(
                pair[1] - pair[0] >= Duration::from_millis(334),
                "{starts:?}"
            );
        }
    }

    #[test]
    fn busy_acoustid_stops() {
        let fixture = HttpFixture::new(|_| json(503, b"{}".to_vec()));
        let err = provider(&fixture, Duration::ZERO)
            .lookup("KEY123", &print(), &Cancellation::new(), deadline())
            .unwrap_err();
        assert_eq!(err.to_string(), "AcoustID is busy, try again shortly");
    }

    #[test]
    fn client_error_without_json_gives_the_http_message() {
        let fixture = HttpFixture::new(|_| {
            let mut reply = json(400, b"nope".to_vec());
            reply.headers = vec![("Content-Type", "text/plain".into())];
            reply
        });
        let err = provider(&fixture, Duration::ZERO)
            .lookup("KEY123", &print(), &Cancellation::new(), deadline())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "AcoustID returned an unsuccessful response (HTTP 400)"
        );
    }

    #[test]
    fn rank_releases_counts_votes_and_keeps_five() {
        let per_file = [
            v(&[A, B]),
            v(&[B]),
            v(&[B, C]),
            v(&[D]),
            v(&[E]),
            v(&[F]),
            v(&[G]),
        ];
        let ranked = rank_releases(&per_file);
        let expect: Vec<(String, u32)> = [(B, 3), (A, 1), (C, 1), (D, 1), (E, 1)]
            .iter()
            .map(|(id, n)| (id.to_string(), *n))
            .collect();
        assert_eq!(ranked, expect);
    }
}
