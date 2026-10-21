use super::{Cancellation, Provider, mirrors, provider::USER_AGENT};
use crate::protocol::DirectoryStation;
use reqwest::Url;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const FIXTURE: &str = include_str!("../../tests/fixtures/radio-browser/search.json");
const SEARCH_PATH: &str =
    "/json/stations/search?name=jazz%20fm&limit=50&hidebroken=true&order=clickcount&reverse=true";

fn reply(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn json(body: &str) -> String {
    reply("200 OK", "application/json", body)
}

/// Serves one connection; the thread returns the raw request.
fn serve(response: String) -> (Url, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
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
    (origin, thread)
}

fn search(origins: Vec<Url>, query: &str) -> anyhow::Result<Vec<DirectoryStation>> {
    Provider::for_test(origins)
        .unwrap()
        .search(query, &Cancellation::idle())
}

#[test]
fn hosts_keep_only_radio_browser_names() {
    let names = [
        "DE1.api.radio-browser.info.",
        "evil.example",
        "fi1.api.radio-browser.info",
        "de1.api.radio-browser.info",
        "api.radio-browser.info.evil.example",
    ];
    assert_eq!(
        mirrors::select_hosts(names.map(String::from), 1),
        ["fi1.api.radio-browser.info", "de1.api.radio-browser.info"]
    );
    assert!(mirrors::select_hosts(["evil.example".to_owned()], 7).is_empty());
}

#[test]
fn search_sends_expected_request() {
    let (origin, server) = serve(json("[]"));
    assert!(search(vec![origin], "jazz fm").unwrap().is_empty());
    let request = server.join().unwrap();
    assert!(
        request.starts_with(&format!("GET {SEARCH_PATH} HTTP/1.1\r\n")),
        "{request}"
    );
    assert!(
        request
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&format!("user-agent: {USER_AGENT}"))),
        "{request}"
    );
}

#[test]
fn results_are_filtered() {
    let (origin, _server) = serve(json(FIXTURE));
    let stations = search(vec![origin], "jazz").unwrap();
    assert_eq!(
        stations,
        [
            DirectoryStation {
                uuid: "11111111-1111-1111-1111-111111111111".into(),
                name: "Jazz FM".into(),
                url: "https://a.example/jazz".into(),
                codec: "MP3".into(),
                bitrate: 128,
                country_code: "GB".into(),
                tags: vec![
                    "jazz".into(),
                    "smooth".into(),
                    "a".into(),
                    "b".into(),
                    "c".into()
                ],
            },
            DirectoryStation {
                uuid: "33333333-3333-3333-3333-333333333333".into(),
                name: "Rock Unresolved".into(),
                url: "https://b.example/rock".into(),
                codec: "AAC+".into(),
                bitrate: 0,
                country_code: "US".into(),
                tags: vec![],
            },
            DirectoryStation {
                uuid: "99999999-9999-9999-9999-999999999999".into(),
                name: "Padded Name".into(),
                url: "https://f.example/stream".into(),
                codec: "OGG".into(),
                bitrate: 64,
                country_code: String::new(),
                tags: vec!["lo-fi".into(), "chill".into()],
            },
        ]
    );
}

#[test]
fn tags_are_cut_on_char_boundaries() {
    let tag = "é".repeat(20);
    let body = serde_json::json!([{
        "stationuuid": "a", "name": "N", "url_resolved": "https://a.example/x",
        "codec": "x".repeat(40), "tags": tag
    }]);
    let (origin, _server) = serve(json(&body.to_string()));
    let stations = search(vec![origin], "n").unwrap();
    assert_eq!(stations[0].tags, ["é".repeat(16)]);
    assert_eq!(stations[0].codec.len(), 16);
}

#[test]
fn results_are_capped_at_fifty() {
    let records: Vec<_> = (0..80)
        .map(|i| {
            serde_json::json!({
                "stationuuid": format!("id-{i}"), "name": "N",
                "url_resolved": format!("https://a.example/{i}")
            })
        })
        .collect();
    let (origin, _server) = serve(json(&serde_json::to_string(&records).unwrap()));
    assert_eq!(search(vec![origin], "n").unwrap().len(), 50);
}

#[test]
fn non_json_reply_is_an_error() {
    let (origin, _server) = serve(reply("200 OK", "text/html", "<html>Bad gateway</html>"));
    let error = search(vec![origin], "jazz").unwrap_err();
    assert!(
        format!("{error:#}").contains("radio-browser.info"),
        "{error:#}"
    );
}

#[test]
fn oversized_reply_is_an_error() {
    let body = format!("[{}]", " ".repeat(512 * 1024 + 1));
    let (origin, _server) = serve(json(&body));
    assert!(search(vec![origin], "jazz").is_err());
}

#[test]
fn search_fails_over_to_next_host() {
    let (first, first_server) = serve(reply("503 Service Unavailable", "text/plain", "down"));
    let (second, second_server) = serve(json(FIXTURE));
    let stations = search(vec![first, second], "jazz").unwrap();
    assert_eq!(stations.len(), 3);
    assert!(
        first_server
            .join()
            .unwrap()
            .starts_with("GET /json/stations/search")
    );
    assert!(
        second_server
            .join()
            .unwrap()
            .starts_with("GET /json/stations/search")
    );
}

#[test]
fn search_gives_up_after_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    thread::spawn(move || {
        let _socket = listener.accept().unwrap();
        thread::sleep(Duration::from_secs(12));
    });
    let started = Instant::now();
    assert!(search(vec![origin], "jazz").is_err());
    assert!(started.elapsed() < Duration::from_secs(9));
}

#[test]
fn cancelled_search_stops_before_any_request() {
    let (origin, _server) = serve(json("[]"));
    let cancel = Cancellation::idle();
    cancel
        .abandoned
        .store(true, std::sync::atomic::Ordering::Release);
    let provider = Provider::for_test(vec![origin]).unwrap();
    assert!(provider.search("jazz", &cancel).is_err());
}

#[test]
fn count_click_requests_url_path() {
    let (origin, server) = serve(json("{\"ok\":true}"));
    Provider::for_test(vec![origin])
        .unwrap()
        .count_click("11111111-1111-1111-1111-111111111111");
    assert!(
        server
            .join()
            .unwrap()
            .starts_with("GET /json/url/11111111-1111-1111-1111-111111111111 HTTP/1.1\r\n")
    );
}

#[test]
fn count_click_ignores_unsafe_ids() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let provider = Provider::for_test(vec![origin]).unwrap();
    provider.count_click("../stations");
    provider.count_click("");
    let error = listener.accept().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn failed_host_moves_to_the_back() {
    let (first, first_server) = serve(reply("503 Service Unavailable", "text/plain", "down"));
    let (second, second_server) = serve(json("[]"));
    let provider = Provider::for_test(vec![first.clone(), second.clone()]).unwrap();
    provider.search("jazz", &Cancellation::idle()).unwrap();
    first_server.join().unwrap();
    second_server.join().unwrap();
    let cancel = Cancellation::idle();
    let order = provider
        .origins(&cancel, Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(order, [second, first]);
}

#[test]
#[ignore = "talks to the real radio-browser.info"]
fn live_mirrors_resolve_and_search() {
    let hosts = mirrors::resolve().unwrap();
    assert!(hosts.iter().all(|h| h.ends_with(".api.radio-browser.info")));
    let stations = Provider::new()
        .unwrap()
        .search("jazz", &Cancellation::idle())
        .unwrap();
    assert!(!stations.is_empty());
}
