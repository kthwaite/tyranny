# Station Directory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Search radio-browser.info from the Radio source, then play or save the stations the user picks.

**Architecture:** The server does the lookup, like lyrics and artwork. A `directory` worker picks a radio-browser mirror by DNS, runs one search at a time with reqwest, and filters the results. Two new commands carry it: `search_directory` and `count_directory_click`. TyrannyLikesSmokeCore gets a `StationDirectory` that searches and saves through the existing `StationCatalog`. The app gets a Find Stations sheet in the Radio view.

**Tech Stack:** Rust (reqwest blocking + rustls, `libc::getnameinfo`, serde), SwiftUI + TyrannyLikesSmokeCore (Swift Testing).

**Spec:** vault task TSK-frail-pollen-xyhm0 and the radio-browser API rules below. Decisions not in the task are rulings, listed at the end.

## Global Constraints

- radio-browser rules (api.radio-browser.info, de1.api.radio-browser.info):
  - Find mirrors by a DNS lookup of `all.api.radio-browser.info`, then a reverse lookup of each address. Randomize the list and use the first entry. On failure, use the next.
  - Send a descriptive User-Agent: `tyranny likes smoke/<CARGO_PKG_VERSION> (https://github.com/kthwaite/tyranny)`.
  - Call `GET /json/url/{stationuuid}` when the user plays a station. The API counts one click per IP, station and day.
  - Do not use `/json/servers` (the API says to use it only without DNS).
- Use HTTPS only and no redirects, like `src/lyrics/provider.rs`. Connect timeout 3 s, request timeout 5 s.
- Make requests only to hosts whose name ends in `.api.radio-browser.info`, after lowercasing and removing a trailing dot.
- Search URL: `https://{host}/json/stations/search?name=<query>&limit=50&hidebroken=true&order=clickcount&reverse=true`.
- Query: trim it. It must be 1..=100 bytes with no control characters. Otherwise reply `invalid_command`.
- Search results:
  - At most 50 stations.
  - Drop stations with `hls == 1`. The FFmpeg build has no HLS demuxer.
  - Use `url_resolved` when it is not empty, else `url`. The pair (url, name) must pass `StreamSpec::new`, which supplies the name rules.
  - Drop a later station that repeats an earlier station's URL.
- Response size cap: 512 KiB for a search and 64 KiB for a click. A bigger response is an error.
- Do not bump the protocol on the branch. `VERSION` stays 12 until the merge into `dev`, where the controller sets it to the next free number.
- The Swift app target has no tests. All logic goes in TyrannyLikesSmokeCore.

## Review Focus

1. **A mirror is down or slow.** Fail over to the next host within one 8 s deadline (at most 3 hosts). The user then sees an error message, not a hang. Pinned in Task 1 (`search_fails_over_to_next_host`, `search_gives_up_after_deadline`).
2. **A reverse lookup returns a foreign hostname.** Ignore it. Never send a request to it. Pinned in Task 1 (`hosts_keep_only_radio_browser_names`).
3. **Bad directory data:**
   - HLS streams;
   - an empty `url_resolved`;
   - credentials or a non-HTTP scheme in a URL;
   - control characters in a name;
   - duplicate URLs;
   - an HTML error page instead of JSON.
   Each is filtered out or becomes a clear error. Pinned in Task 1 (`results_are_filtered`, `non_json_reply_is_an_error`).
4. **A second search starts before the first returns.** Only the latest search's results show. Pinned in Task 3 (`staleSearchReplyIsDropped`).
5. **A disconnect during a search.** The server cancels the work, because the command is in the server's discovery set. The client drops a reply that arrives after `reset`. Pinned in Task 2 (the server's discovery `matches!`) and Task 3 (`replyAfterResetIsDropped`).

---

### Task 1: Directory provider (server, no protocol)

**Files:**
- Create: `src/directory/mod.rs`, `src/directory/provider.rs`, `src/directory/mirrors.rs`, `src/directory/tests.rs`
- Create: `tests/fixtures/radio-browser/search.json`. This is a hand-written reply of about 8 stations. It covers each filter case in Review Focus 3, plus 3 good stations.
- Modify: `src/lib.rs` (`mod directory;`)

**Interfaces:**
- Produces:
  - `pub(crate) struct DirectoryStation { pub uuid: String, pub name: String, pub url: String, pub codec: String, pub bitrate: u32, pub country_code: String, pub tags: Vec<String> }`.
    - Defined in `src/protocol.rs` with `Serialize, Deserialize, Clone, Debug, PartialEq, Eq`, so Task 2 needs no move.
    - `tags`: split on commas, trimmed, empty ones dropped, at most 5, each at most 32 bytes (cut on a char boundary).
    - `codec`: at most 16 bytes.
    - `country_code`: 2 ASCII letters, or empty.
  - `mirrors::select_hosts(names: impl IntoIterator<Item = String>, offset: usize) -> Vec<String>`. This is pure: it filters, dedupes, sorts, then rotates by `offset % len`.
  - `mirrors::resolve() -> Result<Vec<String>>`. This is real DNS: `("all.api.radio-browser.info", 443).to_socket_addrs()`, then `libc::getnameinfo` with `NI_NAMEREQD` for each address, then `select_hosts` with an offset from `uuid::Uuid::new_v4().as_u128()`. It is not unit-tested. Add one `#[ignore]` live test.
  - `Provider`:
    - `new()`.
    - `#[cfg(test)] for_test(origins: Vec<Url>)`. It takes loopback HTTP origins in place of mirrors. Validate them like `lyrics::provider::Provider::for_test`.
    - `search(&self, query: &str, cancel: &Cancellation) -> Result<Vec<DirectoryStation>>`.
    - `count_click(&self, uuid: &str)`. This is best-effort: it ignores errors.
  - Mirror cache inside `Provider`: re-resolve when the list is empty, older than 1 hour, or every host failed in the last search.
  - `Cancellation`: copy the lyrics shape (stop + abandoned flags, `check(deadline)`).

- [ ] **Step 1: Write the failing tests** in `src/directory/tests.rs`. Use the loopback fixture server pattern from `src/lyrics/provider.rs:306`.
  - `hosts_keep_only_radio_browser_names`:
    - input `["DE1.api.radio-browser.info.", "evil.example", "fi1.api.radio-browser.info", "de1.api.radio-browser.info", "api.radio-browser.info.evil.example"]`, offset 1;
    - expected `["fi1.api.radio-browser.info", "de1.api.radio-browser.info"]`.
  - `search_sends_expected_request`:
    - the request path and query equal the Global Constraints search URL for query `jazz fm`, with the space encoded;
    - the User-Agent header equals the constant.
  - `results_are_filtered`:
    - the fixture gives exactly the 3 good stations, in reply order;
    - `url` is the resolved URL;
    - tags are capped as specified.
  - `non_json_reply_is_an_error`: a 200 reply with an HTML body gives `Err`. The message contains `radio-browser.info`.
  - `oversized_reply_is_an_error`: a body over 512 KiB gives `Err`.
  - `search_fails_over_to_next_host`: the first origin returns 503 and the second returns the fixture. Results come back, and both servers saw one request each.
  - `search_gives_up_after_deadline`: a fixture accepts the connection and never answers. `search` returns `Err` within 9 s.
  - `count_click_requests_url_path`: the fixture sees `GET /json/url/<uuid>`.
- [ ] **Step 2:** `cargo test --lib directory` fails to compile.
- [ ] **Step 3:** Implement. Parse the reply with serde into a private `RawStation`:
  - every field is `#[serde(default)]`;
  - `hls` and `bitrate` take a number;
  - unknown fields are ignored.
- [ ] **Step 4:** `cargo test --lib directory` passes. `cargo clippy --all-targets -- -D warnings` is clean.
- [ ] **Step 5:** Commit.

### Task 2: Protocol, worker and routing (server)

**Files:**
- Modify:
  - `src/protocol.rs`:
    - `Command::SearchDirectory { query: String }`;
    - `Command::CountDirectoryClick { station_uuid: String }`;
    - `Response::DirectoryResults { stations: Vec<DirectoryStation> }`;
    - `Response::DirectoryClickCounted`;
    - `ErrorCode::Unavailable`.
  - `src/directory/mod.rs`: `DirectoryWorkers::start()`, `handle()`, `stop()`, `shutdown()`, and `Handle::search(query) -> Reply` and `Handle::count_click(uuid) -> Reply`. Copy the `LyricsWorkers` shape, with 1 worker and a queue capacity of 4.
  - `src/owner.rs`: route both commands to the worker, next to `Command::Lyrics`. Start and stop the worker with lyrics. Add both commands to the `unreachable!` arm.
  - `src/server.rs`: add `Command::SearchDirectory { .. }` to the `discovery` `matches!`.
  - `src/bin/tyranny-likes-smoke-ctl.rs`: add help lines for the two commands, tagged "protocol 12 (bumped at merge)". The controller fixes the tag at merge.
- Test: `src/protocol.rs` tests, `tests/server_transport.rs`.

**Interfaces:**
- Wire forms:
  - `{"command":"search_directory","query":"jazz"}` replies `{"kind":"directory_results","stations":[{uuid,name,url,codec,bitrate,country_code,tags}]}`.
  - `{"command":"count_directory_click","station_uuid":"<uuid>"}` replies `{"kind":"directory_click_counted"}`.
- `count_directory_click`:
  - It replies as soon as the click is queued. It does not wait for the HTTP request. A full queue drops the click and still replies `directory_click_counted`.
  - `station_uuid` must parse as a UUID. Otherwise reply `invalid_command`.
- Search failure (DNS, every host failed, a full queue, workers stopped): reply `Reply::Error { code: Unavailable, message }`. The message names radio-browser.info.

- [ ] **Step 1: Write the failing tests:**
  - protocol round trips for both commands and both responses;
  - `search_directory` rejects `""`, `"   "`, 101 bytes and `"a\u{7}"`, and accepts exactly 100 bytes;
  - `count_directory_click` rejects `"not-a-uuid"`;
  - `ErrorCode::Unavailable` serializes as `"unavailable"`;
  - in `tests/server_transport.rs`, `search_directory_rejects_empty_query` asserts `invalid_command` over the socket. It makes no network call.
- [ ] **Step 2:** Run them. They fail.
- [ ] **Step 3:** Implement. Validate before queueing, so invalid input never reaches the worker.
- [ ] **Step 4:** Run the full gates: `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, and `cargo test` (all targets). All pass.
- [ ] **Step 5:** Commit.

### Task 3: TyrannyLikesSmokeCore StationDirectory (Swift)

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmokeCore/WireModels.swift`. Add `public struct DirectoryStation: Codable, Sendable, Equatable, Identifiable` (`id` = `uuid`), with the fields from Task 2's wire form, and `DirectoryResults { stations }`.
- Create: `macos/Sources/TyrannyLikesSmokeCore/StationDirectory.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/StationDirectoryTests.swift`

**Interfaces:**
- Consumes: `StationCatalog.Send`, `StationCatalog.AddStream`, and `StationCatalog.add(name:url:) async -> Bool`. A failure is in `catalog.error`.
- Produces: `@MainActor @Observable public final class StationDirectory`:
  - `init(send: @escaping StationCatalog.Send, catalog: StationCatalog, addStream: @escaping StationCatalog.AddStream)`;
  - `var query: String`;
  - `private(set) var results: [DirectoryStation]`;
  - `private(set) var searching: Bool`;
  - `var error: String?`;
  - `func search() async`;
  - `func save(_: DirectoryStation) async -> Bool`;
  - `func play(_: DirectoryStation) async`;
  - `func isSaved(_: DirectoryStation) -> Bool`, true when `catalog.stations` has a station with the same `url`;
  - `func reset()`.
  - Clicks: after a successful `save` or `play`, send `count_directory_click`. Ignore its errors. A failed save or play sends no click.
  - `search()`: trim the query. An empty query sends nothing and clears `results`. A token tracks the latest search. A reply to an older token, or one after `reset`, is dropped. On an error reply, `error` is the server message and `results` stays as it was.

- [ ] **Step 1: Write the failing tests.** Use a fake `send` that records requests and replies from a script, in the `StationsModelTests` style:
  - `searchSendsTrimmedQueryAndShowsResults`;
  - `emptyQuerySendsNothing`;
  - `staleSearchReplyIsDropped`: two searches, and the first reply resolves last;
  - `replyAfterResetIsDropped`;
  - `errorReplyShowsMessage`;
  - `saveAddsThenCountsClick`: request names in order are `add_stations`, then `count_directory_click` with the uuid;
  - `failedSaveSendsNoClick`;
  - `playAddsStreamThenCountsClick`;
  - `isSavedMatchesCatalogURL`;
  - `directoryStationDecodesWireForm`.
- [ ] **Step 2:** `swift test --filter StationDirectoryTests` (in `macos/`) fails.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `swift build` and the full `swift test` pass.
- [ ] **Step 5:** Commit.

### Task 4: Find Stations sheet (Swift app)

**Files:**
- Modify:
  - `macos/Sources/TyrannyLikesSmoke/Models/StationsModel.swift`: own a `StationDirectory` built next to the catalog, and reset it with the catalog.
  - `macos/Sources/TyrannyLikesSmoke/Views/RadioView.swift`: a "Find Stations…" button next to "Add Station…", in both places. It opens `DirectorySheet`.
  - `macos/Sources/TyrannyLikesSmoke/WireClient.swift` `resultKind`: `search_directory` maps to `directory_results`, and `count_directory_click` maps to `directory_click_counted`. Add both to the non-snapshot result list.
- Create: `macos/Sources/TyrannyLikesSmoke/Views/DirectorySheet.swift`

**Sheet contents:**
- A search field (placeholder "Search radio-browser.info"). It searches on Return only, not on each keystroke.
- A `ProgressView` while searching.
- The error text.
- A results list. Each row has the name, then a secondary line `GB · MP3 · 128 kbps · jazz, smooth`, which leaves out empty parts. Each row has a Play button and an Add button. Add shows "Saved" and is disabled when `isSaved`.
- The footer has "Station data from radio-browser.info" and a Done button.
- Accessibility identifiers: `radio-directory-sheet`, `-query`, `-results`, `-error`, `-done`.

- [ ] **Step 1:** Implement. The logic is already tested in Task 3. This task only wires the view to it.
- [ ] **Step 2:** `swift build` passes. The full `swift test` passes.
- [ ] **Step 3:** Commit.

### Merge (controller)

- Bump the protocol to the next free version in these places:
  - `src/protocol.rs` `VERSION`;
  - `WireProtocol.version`;
  - the tyranny-likes-smoke-ctl tag;
  - the scripts' version defaults;
  - the `StationCatalog` doc comment.
- Run the full gates. Update the vault task. Merge into `dev`.

## Rulings

- **The lookup runs on the server.** Lyrics and artwork already work this way: reqwest, a worker thread and loopback fixtures. tyranny-likes-smoke-ctl gets it for free. Cost if wrong: the server makes outbound requests that a client-side design would avoid.
- **Mirrors come from DNS plus `getnameinfo`.** This avoids SRV lookup, which would need a new DNS crate. Cost if wrong: a reverse lookup failure means no mirrors, so search shows an error.
- **"Click" means play or save from the results.** Saved stations do not store the uuid, so later plays from Radio are not counted. Cost if wrong: under-counted clicks. Adding a uuid column later is a schema change.
- **A new `ErrorCode::Unavailable`.** Lyrics instead uses an ok-with-status reply. A search has no partial result, so an error fits better. Cost if wrong: one more error code.
