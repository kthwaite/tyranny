//! MusicBrainz ws/2 lookups: one release by id, or a release search.
use super::{
    LookupError,
    http::{self, StartGate},
};
use crate::cancel::Cancellation;
use anyhow::Result;
use reqwest::{Url, blocking::Client};
use serde::{Deserialize, Deserializer};
use std::time::{Duration, Instant};

const SERVICE: &str = "MusicBrainz";
const ORIGIN: &str = "https://musicbrainz.org/";
const START_INTERVAL: Duration = Duration::from_secs(1);
const SEARCH_LIMIT: u32 = 5;

pub(super) struct MusicBrainz {
    client: Client,
    gate: StartGate,
    origin: Url,
}
pub(super) struct SearchQuery<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub tracks: Option<usize>,
}

impl MusicBrainz {
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
    pub(super) fn release(
        &self,
        id: &str,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<Release, LookupError> {
        if !is_mbid(id) {
            return Err(LookupError::Failed(format!(
                "{SERVICE} release id is not valid"
            )));
        }
        let mut url = self.endpoint(&format!("ws/2/release/{id}"))?;
        url.set_query(Some(
            "inc=recordings+artist-credits+release-groups+media&fmt=json",
        ));
        let bytes = http::get_json(&self.client, &self.gate, url, SERVICE, cancel, deadline)?;
        parse(&bytes)
    }
    pub(super) fn search(
        &self,
        query: &SearchQuery<'_>,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<Vec<SearchHit>, LookupError> {
        let mut url = self.endpoint("ws/2/release")?;
        url.query_pairs_mut()
            .append_pair("query", &lucene(query))
            .append_pair("limit", &SEARCH_LIMIT.to_string())
            .append_pair("fmt", "json");
        let bytes = http::get_json(&self.client, &self.gate, url, SERVICE, cancel, deadline)?;
        #[derive(Deserialize)]
        struct Hits {
            #[serde(default)]
            releases: Vec<SearchHit>,
        }
        Ok(parse::<Hits>(&bytes)?.releases)
    }
    fn endpoint(&self, path: &str) -> Result<Url, LookupError> {
        self.origin
            .join(path)
            .map_err(|e| LookupError::Failed(format!("{SERVICE} url: {e}")))
    }
}

fn parse<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, LookupError> {
    serde_json::from_slice(bytes)
        .map_err(|_| LookupError::Failed(format!("{SERVICE} returned an unreadable response")))
}

/// `release:"<title>" AND artist:"<artist>"`, plus `AND tracks:<n>`.
pub(super) fn lucene(query: &SearchQuery<'_>) -> String {
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut text = format!(
        "release:\"{}\" AND artist:\"{}\"",
        quote(query.title),
        quote(query.artist)
    );
    if let Some(tracks) = query.tracks {
        text.push_str(&format!(" AND tracks:{tracks}"));
    }
    text
}

/// 8-4-4-4-12 lowercase hex.
pub(super) fn is_mbid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

fn empty_is_none<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.filter(|s| !s.is_empty()))
}

#[derive(Deserialize)]
pub(super) struct Release {
    pub id: String,
    pub title: String,
    #[serde(default, deserialize_with = "empty_is_none")]
    pub date: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default, rename = "artist-credit")]
    pub artist_credit: Vec<Credit>,
    #[serde(default, rename = "release-group")]
    pub release_group: Option<ReleaseGroup>,
    #[serde(default)]
    pub media: Vec<Medium>,
}
#[derive(Deserialize)]
pub(super) struct Credit {
    pub name: String,
    #[serde(default)]
    pub joinphrase: String,
    pub artist: CreditArtist,
}
#[derive(Deserialize)]
pub(super) struct CreditArtist {
    pub id: String,
}
#[derive(Deserialize)]
pub(super) struct ReleaseGroup {
    pub id: String,
    #[serde(
        default,
        rename = "first-release-date",
        deserialize_with = "empty_is_none"
    )]
    pub first_release_date: Option<String>,
}
#[derive(Deserialize)]
pub(super) struct Medium {
    pub position: u32,
    #[serde(rename = "track-count")]
    pub track_count: u32,
    #[serde(default)]
    pub tracks: Vec<ReleaseTrack>,
}
#[derive(Deserialize)]
pub(super) struct ReleaseTrack {
    pub id: String,
    pub position: u32,
    pub title: String,
    #[serde(default)]
    pub length: Option<u64>,
    #[serde(default, rename = "artist-credit")]
    pub artist_credit: Vec<Credit>,
    pub recording: Recording,
}
#[derive(Deserialize)]
pub(super) struct Recording {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub length: Option<u64>,
    #[serde(default, rename = "artist-credit")]
    pub artist_credit: Vec<Credit>,
}
#[derive(Deserialize)]
pub(super) struct SearchHit {
    pub id: String,
    pub score: u32,
}

/// Credited names with their join phrases, concatenated.
pub(super) fn credit_name(credits: &[Credit]) -> String {
    credits
        .iter()
        .map(|c| format!("{}{}", c.name, c.joinphrase))
        .collect()
}
impl Release {
    pub fn artist(&self) -> String {
        credit_name(&self.artist_credit)
    }
    pub fn track_count(&self) -> u32 {
        self.media.iter().map(|m| m.track_count).sum()
    }
    /// Sum of track lengths (recording length when the track has none).
    /// `None` if any track has neither.
    pub fn duration_ms(&self) -> Option<u64> {
        let mut total = 0;
        for track in self.media.iter().flat_map(|m| &m.tracks) {
            total += track.length.or(track.recording.length)?;
        }
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{HttpFixture, Reply, fixture, json};
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }
    fn provider(fixture: &HttpFixture) -> MusicBrainz {
        MusicBrainz::for_test(&fixture.origin(), Duration::ZERO).unwrap()
    }
    fn releases(target: &str) -> Reply {
        let name = [(A, "a"), (B, "b"), (C, "c")]
            .iter()
            .find(|(id, _)| target.contains(id))
            .map(|(_, n)| *n)
            .unwrap();
        json(200, fixture(&format!("release-{name}.json")))
    }
    fn release(id: &str) -> Release {
        let fixture = HttpFixture::new(releases);
        provider(&fixture)
            .release(id, &Cancellation::new(), deadline())
            .unwrap()
    }

    #[test]
    fn release_request_path_headers_and_parse() {
        let fixture = HttpFixture::new(releases);
        let release = provider(&fixture)
            .release(A, &Cancellation::new(), deadline())
            .unwrap();
        let seen = fixture.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].target,
            format!(
                "/ws/2/release/{A}?inc=recordings+artist-credits+release-groups+media&fmt=json"
            )
        );
        assert!(seen[0].headers.contains(http::USER_AGENT));
        assert!(
            seen[0]
                .headers
                .to_ascii_lowercase()
                .contains("accept: application/json")
        );
        assert_eq!(release.artist(), "Fixture Artist");
        assert_eq!(
            credit_name(&release.media[0].tracks[1].artist_credit),
            "Fixture Artist feat. Guest"
        );
        assert_eq!(release.track_count(), 3);
        assert_eq!(release.duration_ms(), Some(750));
        assert_eq!(
            release.release_group.unwrap().first_release_date.as_deref(),
            Some("1999-02-01")
        );
        assert_eq!(release.date.as_deref(), Some("2001-05-14"));
        assert_eq!(release.country.as_deref(), Some("GB"));
    }

    #[test]
    fn search_escapes_lucene_and_parses_scores() {
        let query = SearchQuery {
            title: r#"AC/DC "Live" \ Back"#,
            artist: "Guns N' Roses",
            tracks: Some(12),
        };
        assert_eq!(
            lucene(&query),
            r#"release:"AC/DC \"Live\" \\ Back" AND artist:"Guns N' Roses" AND tracks:12"#
        );
        let server = HttpFixture::new(|_| json(200, fixture("search.json")));
        let hits = provider(&server)
            .search(&query, &Cancellation::new(), deadline())
            .unwrap();
        let hits: Vec<_> = hits.iter().map(|h| (h.id.as_str(), h.score)).collect();
        assert_eq!(hits, [(A, 100), (B, 90)]);
        let target = &server.seen()[0].target;
        assert!(target.starts_with("/ws/2/release?query="), "{target}");
        assert!(target.ends_with("&limit=5&fmt=json"), "{target}");
        let empty = HttpFixture::new(|_| json(200, fixture("search-empty.json")));
        assert!(
            provider(&empty)
                .search(&query, &Cancellation::new(), deadline())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn gate_spaces_request_starts_by_at_least_one_second() {
        let fixture = HttpFixture::new(releases);
        let mb = MusicBrainz::for_test(&fixture.origin(), Duration::from_secs(1)).unwrap();
        for _ in 0..3 {
            mb.release(A, &Cancellation::new(), deadline()).unwrap();
        }
        let seen = fixture.seen();
        assert_eq!(seen.len(), 3);
        for pair in seen.windows(2) {
            assert!(pair[1].at - pair[0].at >= Duration::from_secs(1));
        }
    }

    #[test]
    fn busy_statuses_stop_without_retry() {
        for (status, retry_after) in [(503, false), (429, false), (500, true)] {
            let fixture = HttpFixture::new(move |_| {
                let mut reply = json(status, b"{}".to_vec());
                if retry_after {
                    reply.headers.push(("Retry-After", "5".into()));
                }
                reply
            });
            let error = provider(&fixture)
                .release(A, &Cancellation::new(), deadline())
                .err()
                .unwrap();
            assert!(
                matches!(error, LookupError::Busy("MusicBrainz")),
                "{error:?}"
            );
            assert_eq!(error.to_string(), "MusicBrainz is busy, try again shortly");
            assert_eq!(fixture.seen().len(), 1);
        }
    }

    #[test]
    fn redirects_404_oversize_and_non_json_fail_readably() {
        let cases: Vec<(Reply, &str)> = vec![
            (
                Reply {
                    status: 302,
                    headers: vec![("Location", "http://127.0.0.1:1/".into())],
                    body: vec![],
                    delay: Duration::ZERO,
                },
                "MusicBrainz redirect rejected",
            ),
            (json(404, b"{}".to_vec()), "MusicBrainz has no such record"),
            (
                json(200, vec![b' '; http::MAX_RESPONSE_BYTES + 1]),
                "MusicBrainz response exceeds 2 MiB",
            ),
            (
                Reply {
                    status: 200,
                    headers: vec![("Content-Type", "text/html".into())],
                    body: b"<html>".to_vec(),
                    delay: Duration::ZERO,
                },
                "MusicBrainz returned non-JSON content",
            ),
            (
                json(418, b"{}".to_vec()),
                "MusicBrainz returned an unsuccessful response (HTTP 418)",
            ),
        ];
        for (reply, message) in cases {
            let reply = std::sync::Mutex::new(Some(reply));
            let fixture = HttpFixture::new(move |_| reply.lock().unwrap().take().unwrap());
            let error = provider(&fixture)
                .release(A, &Cancellation::new(), deadline())
                .err()
                .unwrap();
            assert!(
                matches!(&error, LookupError::Failed(m) if m == message),
                "{error:?}"
            );
        }
        // Nothing listens here: connect errors are readable too.
        let closed = HttpFixture::new(|_| json(200, vec![]));
        let origin = closed.origin();
        drop(closed);
        let error = MusicBrainz::for_test(&origin, Duration::ZERO)
            .unwrap()
            .release(A, &Cancellation::new(), deadline())
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "MusicBrainz could not be reached");
    }

    #[test]
    fn cancelled_lookups_send_nothing() {
        let fixture = HttpFixture::new(releases);
        let cancel = Cancellation::with_flags(
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
        );
        let mb = provider(&fixture);
        assert!(matches!(
            mb.release(A, &cancel, deadline()),
            Err(LookupError::Cancelled)
        ));
        assert!(matches!(
            mb.release(A, &Cancellation::new(), Instant::now()),
            Err(LookupError::TimedOut)
        ));
        assert!(fixture.seen().is_empty());
    }

    #[test]
    fn empty_dates_are_absent() {
        assert_eq!(release(B).release_group.unwrap().first_release_date, None);
        let release: Release = parse(
            br#"{"id":"x","title":"t","date":"","media":[],"release-group":{"id":"g","first-release-date":""}}"#,
        )
        .unwrap();
        assert_eq!(release.date, None);
        assert_eq!(release.release_group.unwrap().first_release_date, None);
    }

    #[test]
    fn non_mbid_ids_are_refused_before_http() {
        let fixture = HttpFixture::new(releases);
        let mb = provider(&fixture);
        for id in [
            "",
            "../x",
            "11111111-1111-4111-8111-11111111111Z",
            "C1000000-0000-4000-8000-000000000002",
            &format!("{A}?x=1"),
        ] {
            assert!(
                mb.release(id, &Cancellation::new(), deadline()).is_err(),
                "{id}"
            );
        }
        assert!(is_mbid(A) && !is_mbid("11111111111141118111111111111111"));
        assert!(fixture.seen().is_empty());
    }

    #[test]
    fn for_test_rejects_non_loopback_origins() {
        for origin in [
            "https://musicbrainz.org/",
            "http://example.com/",
            "http://127.0.0.1:1/path",
            "http://user@127.0.0.1:1/",
            "http://127.0.0.1:1/?q=1",
        ] {
            assert!(
                MusicBrainz::for_test(origin, Duration::ZERO).is_err(),
                "{origin}"
            );
        }
        assert!(MusicBrainz::for_test("http://127.0.0.1:1/", Duration::ZERO).is_ok());
    }

    #[test]
    fn duration_falls_back_to_recording_length() {
        let mut a = release(A);
        a.media[0].tracks[0].length = None;
        a.media[0].tracks[0].recording.length = Some(1000);
        assert_eq!(a.duration_ms(), Some(1500));
        a.media[0].tracks[0].recording.length = None;
        assert_eq!(a.duration_ms(), None);
        assert_eq!(release(C).duration_ms(), None);
        assert_eq!(release(C).track_count(), 4);
        assert_eq!(release(B).duration_ms(), None);
    }
}
