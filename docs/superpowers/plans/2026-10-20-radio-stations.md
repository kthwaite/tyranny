# Radio Stations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A "Radio" source in the sidebar. It lists saved internet-radio stations. You can add, edit and remove stations by URL, import `.pls`/`.m3u` station files, and play a station. The transport shows the station name under the ICY title.

**Architecture:** Stations are a small catalog that the server persists in SQLite (schema 7), with one catalog revision, like saved playlists. `ControlState.stations_revision` tells clients when to refetch. Playing a station reuses the existing stream path: the client sends `append_stream` with the station's URL and name, then `play`. Station files are parsed on the client in TyrannyLikesSmokeCore, so import also works against a remote server. The server only accepts a batch of `{name, url}`.

**Tech Stack:** Rust (rusqlite, serde, tokio server), SwiftUI + TyrannyLikesSmokeCore (Swift package), XCTest/Swift Testing as used in `macos/Tests`.

**Spec:** None. The user skipped design questions (2026-10-20). The task is TSK-brave-pigeon-3nn2c. The decisions below were made by Claude and are listed so a reviewer can reject them:
- Scope: the task's checklist items 1–3. The radio-browser.info directory lookup (item 4, "optional") is out of scope.
- One `Radio` destination, shown as a table of stations. Stations are not listed one by one in the sidebar.
- A station's identity is its canonical URL. URLs are unique, and adding a URL that is already saved is skipped, not duplicated.
- Stations are sorted by name, case-insensitive, then by id.
- Editing or removing a station never touches queue entries already made from it. Each queue entry holds its own `StreamSpec`.

Background reading for every task: `SPIKE.md` (status line `:9`, stream notes `:168`, playlists `:270-289`, deferral `:348`) and `SPIKE_RESULTS.md` "HTTP stream playback" (`:19-47`).

## Global Constraints

- Protocol version goes 10 → 11 (`src/protocol.rs` `VERSION`). Versioning is exact-match. Every hard-coded `v10`/`version=10` must move to 11 in the same task, including:
  - `macos/Sources/TyrannyLikesSmoke/WireClient.swift`, `StatusBar.swift:77` and `NativeShellSmoke.swift`;
  - `src/bin/tyranny-likes-smoke-ctl.rs`;
  - `scripts/{playlist,smart_playlist,server,library,scale}_gate.py`;
  - `SPIKE.md:9`.
  - Find them with `grep -rn "v10\|version=10\|VERSION: u32 = 10\|== 10" src macos scripts SPIKE.md`.
- Store schema goes 6 → 7 (`src/store.rs` `SCHEMA_VERSION`). Add exactly one migrate step `6 → 7`, written in the existing `if version == N && target > N` style.
- Station URL and name validation reuse `StreamSpec::new(url, name)` (`src/protocol.rs:465`). Do not write a second validator. A stored station is always the `StreamSpec`-canonical URL plus the resolved title.
- `MAX_STATIONS = 1000` (catalog size). `MAX_STATION_BATCH = 500` (stations per add command). Going over either is `InvalidCommand`, and nothing is committed.
- New wire names (snake_case JSON):
  - commands: `stations`, `add_stations`, `update_station`, `remove_station`;
  - replies: `stations`, `stations_applied`;
  - error code: `stale_stations_revision`.
- Accessibility identifiers use the `radio-` prefix (`source-radio` for the sidebar row).
- Commit messages: one imperative sentence, sentence case, no prefix, no attribution lines.
- Gates after every task: `cargo build --locked --all-targets`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test --locked --all-targets`, `swift build --package-path macos --scratch-path target/swift-client`, `swift test --package-path macos --scratch-path target/swift-client`.

## Review Focus

1. **Re-importing the same station file** must not create duplicates. Adding a URL that is already saved is skipped (Task 1 test `add_skips_urls_already_saved`).
2. **Real-world `.pls` files:**
   - CRLF line endings, a UTF-8 BOM, lower-case keys (`file1=`), a missing `NumberOfEntries`, and gaps in the numbering (File1, File3) must all parse.
   - Entries with no `TitleN` get the host as their name (Task 4 test `plsToleratesRealWorldVariants`).
3. **`.m3u` files with local paths or non-HTTP lines:**
   - Those lines are skipped.
   - A file with no HTTP(S) entries is an error that names the file, not a silent empty import (Task 4 test `m3uSkipsNonHTTPAndRejectsEmpty`).
4. **Editing or removing the playing station** leaves the playing queue entry, its title and its URL unchanged (Task 2 test `station_edits_do_not_touch_queue_entries`).
5. **Two clients editing at once:** a stale `expected_revision` gets `stale_stations_revision`. The client refetches and shows the error instead of silently dropping the edit (Task 2 test `stale_station_revision_is_rejected`; Task 5 test `staleEditRefetchesAndReports`).

---

### Task 1: Station store (schema 7)

**Files:**
- Create: `src/store/stations.rs` (table migration, catalog queries, edits)
- Create: `src/store/stations_tests.rs`
- Modify: `src/store.rs` (`SCHEMA_VERSION = 7`, migrate step 6→7 calling `stations::migrate`, `mod stations;`, the `user_version` asserts near `:1099` and `:1159`)
- Modify: `src/protocol.rs` (add the `Station` struct only)

**Interfaces:**
- Produces, in `src/protocol.rs`:
  ```rust
  #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
  pub struct Station { pub id: u64, pub name: String, pub url: String }
  pub const MAX_STATIONS: usize = 1000;
  pub const MAX_STATION_BATCH: usize = 500;
  ```
- Produces, in `src/store/stations.rs` (methods on `Store`, mirroring `src/store/playlists.rs`):
  ```rust
  pub enum StationEdit { Add(Vec<StreamSpec>), Update { id: u64, spec: StreamSpec }, Remove { id: u64 } }
  pub struct StationChange { pub revision: u64, pub stations: Vec<Station> } // added or updated rows; empty for Remove
  pub fn stations_revision(&self) -> Result<u64>
  pub fn stations(&self) -> Result<(u64, Vec<Station>)>          // (revision, all rows, sorted)
  pub fn edit_stations(&mut self, expected: u64, edit: StationEdit) -> Result<StationChange>
  ```
- Tables: `stations_state(revision INTEGER NOT NULL)` (a single row) and `stations(id INTEGER PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL UNIQUE, sort_key TEXT NOT NULL)`. `sort_key` is the lowercased name. Order rows by `sort_key, id`.
- Errors:
  - A stale revision uses the same typed-error mechanism as `playlists.rs::check_revision`, with a distinct stations variant. Task 2 maps it to `stale_stations_revision`.
  - An unknown id, a full catalog, an oversized batch, or an update whose URL another station already uses is `invalid(...)`.
- Every successful edit bumps the revision by exactly 1, through `next_revision`, even when an `Add` skips all of its URLs. Clients use the bump to refetch.

- [ ] **Step 1: Write failing tests** in `src/store/stations_tests.rs`:
  - `schema_seven_migrates_from_six_with_empty_catalog`: open a schema-6 fixture DB (reuse the 6-builder pattern from `schema_six_preserves_files_and_round_trips_streams`). Assert `user_version == 7`, `stations() == (0, vec![])`, and the queue rows are intact.
  - `add_update_remove_round_trip_and_survive_reopen`: add 2, update 1 (name and URL), remove 1. Reopen the store. Assert the rows and `revision == 3`.
  - `add_skips_urls_already_saved`: add `http://a/x`, then add `[http://A/x, http://b/y]`. The second change returns only `b`. The catalog has 2 rows, and the revision moved twice.
  - `stations_sort_by_name_case_insensitively`: names `b`, `A`, `c`. Expect the order `A, b, c`.
  - `update_to_an_existing_url_is_invalid_and_commits_nothing`.
  - `stale_revision_and_limits_commit_nothing`: a wrong `expected`; a batch of 501; a catalog at 1000 plus 1. Each errors, and the revision and rows are unchanged.
- [ ] **Step 2: Run** `cargo test --locked --lib stations`. Expected: compile failure / FAIL.
- [ ] **Step 3: Implement** `src/store/stations.rs` and the `store.rs` migrate step.
- [ ] **Step 4: Run** `cargo test --locked --lib`. Expected: PASS, including the existing migration tests with the updated `user_version` asserts.
- [ ] **Step 5: Commit** "Persist saved radio stations in schema 7".

### Task 2: Station protocol, owner and server (protocol v11)

**Files:**
- Modify: `src/protocol.rs`:
  - add `Command::{Stations, AddStations, UpdateStation, RemoveStation}`;
  - add `Response::{Stations, StationsApplied}`;
  - add `ErrorCode::StaleStationsRevision`;
  - add `ControlState.stations_revision: u64`;
  - set `VERSION = 11` and update the version test at `:904`.
- Modify: `src/owner.rs` (dispatch, `stations_revision` field loaded at startup and published in `ControlState`, handlers mirroring `playlist_applied` at `:972`)
- Modify: `src/server.rs:526` (sequence `StationsApplied` like `PlaylistApplied`)
- Modify: `src/bin/tyranny-likes-smoke-ctl.rs`, `scripts/*_gate.py`, `SPIKE.md`, and the Swift files listed in Global Constraints (version 11 only, plus decoding `stations_revision` in the hand-written `ControlState` decoder in `macos/Sources/TyrannyLikesSmokeCore/WireModels.swift:61/109/118`)
- Create: `tests/support/stations.rs`, mounted from `tests/server_transport.rs` like `saved_playlists`

**Interfaces:**
- Consumes: Task 1 `Store::{stations, edit_stations, stations_revision}`, `StationEdit`, `StationChange`, `Station`.
- Produces (wire):
  ```rust
  Stations {}                                                                  // → Response::Stations { revision: u64, stations: Vec<Station> }
  AddStations { expected_revision: u64, stations: Vec<StationInput> }          // → StationsApplied
  UpdateStation { station_id: u64, expected_revision: u64, name: String, url: String } // → StationsApplied
  RemoveStation { station_id: u64, expected_revision: u64 }                    // → StationsApplied
  pub struct StationInput { pub name: String, pub url: String }                // name may be blank → host
  Response::StationsApplied { snapshot: Box<ControlState>, stations: Vec<Station> }
  ```
- The owner turns each `StationInput` into a `StreamSpec` with `StreamSpec::new(url, name)`. If any entry is invalid, the whole command is `InvalidCommand`, and the error message names the 0-based index of the bad entry.

- [ ] **Step 1: Write failing integration tests** in `tests/support/stations.rs`, using the helper style of `tests/support/saved_playlists.rs` (`ok`, `command`, `state`):
  - `station_catalog_round_trips_and_survives_restart`: add 2, list, update, remove. Restart the server on the same state directory. List again. After each edit, `ControlState.stations_revision` in the broadcast equals the reply snapshot's value.
  - `stale_station_revision_is_rejected`: two edits with the same `expected_revision`. The second gets `code == "stale_stations_revision"`.
  - `invalid_station_batch_commits_nothing`: `[valid, "ftp://x", valid]`. The error message contains `1`, and the catalog is unchanged.
  - `station_edits_do_not_touch_queue_entries`: add a station, then `append_stream` with its URL and name. Update the station's name and URL, then remove it. The queue entry's `stream` is still the original `{url, title}`.
- [ ] **Step 2: Run** `cargo test --locked --test server_transport stations`. Expected: FAIL.
- [ ] **Step 3: Implement** the protocol additions, owner dispatch and server sequencing. Bump the version everywhere listed in Global Constraints. Add `stations_revision` to the Swift `ControlState` decoder.
- [ ] **Step 4: Run** all Global Constraints gates. Expected: PASS. Swift `WireModelTests` decode a `ControlState` that includes `stations_revision`; update its fixture JSON.
- [ ] **Step 5:** Add a SPIKE.md subsection "Saved radio stations". Cover the commands, uniqueness by canonical URL, the sort order, the limits, and that queue entries are independent. Change the deferral note at `:348` to name only the directory lookup.
- [ ] **Step 6: Commit** "Serve saved radio stations over protocol 11".

### Task 3: Station name in the transport

**Files:**
- Modify: `src/protocol.rs` (`NowPlaying.station: Option<String>`)
- Modify: `src/owner.rs` `describe()` (`:702`): for a stream entry, set `station = Some(stream.title)`; for others, `None`.
- Modify: `macos/Sources/TyrannyLikesSmokeCore/WireModels.swift` (`NowPlaying.station: String?`, optional in decoding, added to `init`)
- Modify: `macos/Sources/TyrannyLikesSmoke/Models/TransportModel.swift` (`artistAlbum`), `SystemMediaControls.swift:100`
- Test: `tests/support/http_streams.rs` (extend the existing test), `macos/Tests/TyrannyLikesSmokeCoreTests/WireModelTests.swift`, `NowPlayingArtTests.swift` or a new `TransportSubtitleTests.swift`

**Interfaces:**
- Produces: `NowPlaying.station` (Rust `Option<String>`, Swift `String?`).
- TransportModel rule: if `station` is non-nil and differs from `title`, `artistAlbum` is the station name. Otherwise it keeps the existing artist — album text. Put the rule in a pure TyrannyLikesSmokeCore function, `public func transportSubtitle(title: String, artist: String, album: String, station: String?) -> String`, so it can be unit-tested. `SystemMediaControls` uses `station` as `MPMediaItemPropertyArtist` when present.

- [ ] **Step 1: Write failing tests:**
  - In `live_streams_recover_publish_icy_persist_and_keep_controls_responsive`, assert `now_playing.station == Some(<station title>)` once an ICY title arrives.
  - Swift `transportSubtitleShowsStationForStreams`: `(title: "Song", artist: "", album: "", station: "KEXP")` gives `"KEXP"`; `(title: "KEXP", …, station: "KEXP")` gives `""`; `station: nil` with an artist and album gives `"A — B"`.
- [ ] **Step 2: Run** `cargo test --locked --test server_transport live_streams` and `swift test … --filter transportSubtitle`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates. Expected: PASS.
- [ ] **Step 5: Commit** "Show the station name under a stream's title".

### Task 4: Station wire models and station-file parser (TyrannyLikesSmokeCore)

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmokeCore/WireModels.swift` (`Station`, `StationsPage`, `StationsApplied`)
- Create: `macos/Sources/TyrannyLikesSmokeCore/StationFile.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/StationFileTests.swift`, `WireModelTests.swift`

**Interfaces:**
- Produces:
  ```swift
  public struct Station: Codable, Sendable, Equatable, Identifiable { public let id: UInt64; public let name: String; public let url: String }
  public struct StationsPage: Codable, Sendable { public let revision: UInt64; public let stations: [Station] }
  public struct StationsApplied: Codable, Sendable { public let snapshot: ControlState; public let stations: [Station] }
  public struct StationInput: Codable, Sendable, Equatable { public let name: String; public let url: String }
  public enum StationFile {
      public static func parse(_ text: String, fileName: String) throws -> [StationInput]
  }
  public struct StationFileError: Error, Equatable { public let message: String } // message names fileName
  ```
- Format rules:
  - The format is chosen by extension, case-insensitive: `.pls`; `.m3u`/`.m3u8`. Any other extension throws.
  - **PLS:**
    - Strip a UTF-8 BOM, split on `\r\n` or `\n`, and match keys case-insensitively.
    - Use `FileN=` and `TitleN=`. Ignore `NumberOfEntries`. Keep the numeric order of N.
  - **M3U:**
    - Read `#EXTINF:<dur>,<title>` for the next URL's title. Skip other `#` lines.
    - Keep only lines that start with `http://` or `https://`, case-insensitive. Skip everything else.
  - Name: a missing name is `""` (the server falls back to the host). Trim whitespace on names and URLs.
  - De-duplicate exact URLs within one file and keep the first.
  - Zero entries → `StationFileError("\(fileName) has no http or https stations")`.

- [ ] **Step 1: Write failing tests:**
  - `plsParsesEntriesInNumericOrder`
  - `plsToleratesRealWorldVariants`: CRLF, BOM, `file1=`, no NumberOfEntries, File1+File3 only, a missing Title3 → name `""`.
  - `m3uReadsExtinfTitles`
  - `m3uSkipsNonHTTPAndRejectsEmpty`: a file of local paths and `rtsp://` lines throws, with a message that contains the file name.
  - `duplicateURLsInOneFileKeepFirst`
  - `unknownExtensionThrows`
  - `stationsAppliedDecodes`
- [ ] **Step 2: Run** `swift test --package-path macos --scratch-path target/swift-client --filter StationFile`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates. Expected: PASS.
- [ ] **Step 5: Commit** "Parse pls and m3u station files in TyrannyLikesSmokeCore".

### Task 5: Radio source — model, view, sheets, import, play

**Files:**
- Create: `macos/Sources/TyrannyLikesSmoke/Models/StationsModel.swift`
- Create: `macos/Sources/TyrannyLikesSmoke/Views/RadioView.swift` (station table plus `StationSheet` for add/edit)
- Modify: `Shell/Destination.swift` (`case radio`; key `"radio"`, title `"Radio"`, systemImage `"dot.radiowaves.left.and.right"`; added to `Destination.playing` after `.history`; extend every switch)
- Modify: `Shell/MainWindowView.swift:164-181` (`.radio` → `RadioView`)
- Modify: `WireClient.swift` `resultKind(for:)` (`:351-364`) and reply kinds (`:495-497`) for the four commands
- Modify: `TyrannyLikesSmokeApp.swift` / wherever `PlaylistsModel` is created and synced, so `StationsModel` is created and fed snapshots the same way
- Modify: `TyrannyLikesSmokeCore/CommandLanguage.swift:118` (add `radio` to `go` choices), `CommandDispatcher.swift:31`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/StationsModelTests.swift` (model logic behind a request seam, following the pattern of `PlaylistRemovalTests`/`CommandTransportTests`), `CommandLanguageTests.swift`

**Interfaces:**
- Consumes:
  - Task 2's wire commands;
  - Task 4's `Station`, `StationsPage`, `StationsApplied`, `StationInput`, `StationFile.parse`;
  - the existing `UpNextModel.addStream(url:title:play:)` (`UpNextModel.swift:278`) for playing and enqueueing.
- Produces (`@MainActor @Observable final class StationsModel`):
  ```swift
  var stations: [Station]; var revision: UInt64; var error: String?
  func sync(_ snapshot: ControlState) async          // refetch `stations` when snapshot.stations_revision != revision
  func add(name: String, url: String) async -> Bool
  func update(_ id: UInt64, name: String, url: String) async -> Bool
  func remove(_ id: UInt64) async
  func importFile(_ url: URL) async                 // read file → StationFile.parse → add_stations; sets error on failure
  func play(_ station: Station) async               // error = await upNext.addStream(url:, title: name, play: true)
  func enqueue(_ station: Station) async            // same with play: false; addStream returns String? (error text)
  ```
- On `stale_stations_revision`, the model refetches, sets `error` to the server message, and returns `false`. It does not retry.
- RadioView:
  - Table columns are Name and URL.
  - Double-click or Return plays.
  - The context menu has Play, Add to Up Next, Edit…, Remove…; Remove asks for confirmation with an alert, like playlist delete.
  - The toolbar has Add Station… and Import… (`NSOpenPanel`, allowed types pls/m3u/m3u8).
  - An empty state says "No stations" and has the Add and Import buttons.
  - `StationSheet` follows the `AddStreamSheet`/`PlaylistNameSheet` pattern. Its fields are URL and Name, its error text comes from the server reply, and it has a Save button.
- Identifiers:
  - `source-radio`, `radio-table`, `radio-add`, `radio-import`;
  - `radio-station-play`, `radio-station-enqueue`, `radio-station-edit`, `radio-station-remove`;
  - `radio-sheet-url`, `radio-sheet-name`, `radio-sheet-error`, `radio-sheet-save`.

- [ ] **Step 1: Write failing tests:**
  - `StationsModelTests.syncFetchesOnlyWhenRevisionChanges`
  - `addSendsAddStationsWithExpectedRevision`
  - `importParsesAndSendsOneBatch`
  - `importOfEmptyFileSetsErrorAndSendsNothing`
  - `staleEditRefetchesAndReports`
  - `playAppendsStreamWithStationNameAndPlays`
  - `CommandLanguageTests`: `go radio` completes and parses to the radio destination.
- [ ] **Step 2: Run** `swift test … --filter "StationsModel|CommandLanguage"`. Expected: FAIL.
- [ ] **Step 3: Implement** the model, destination, view, sheet and wiring.
- [ ] **Step 4: Run** all gates. Expected: PASS.
- [ ] **Step 5: Manual check (needs the app running):** `just build`, then launch. Add a station (for example `https://ice1.somafm.com/groovesalad-128-mp3`), play it, edit it, remove it, and import a `.pls`. Record in SPIKE_RESULTS.md whether this was done. If Accessibility or Screen Recording permission is missing, say so; do not claim it.
- [ ] **Step 6:** Add a SPIKE_RESULTS.md section "Saved radio stations" with the test commands, then commit "Add the Radio source with saved stations and station-file import".
