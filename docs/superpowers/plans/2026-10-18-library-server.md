# Library Redesign — Server Implementation Plan (1 of 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the Rust server everything the redesigned window needs. That means new track fields, list descriptions with sort and grouping, run enqueue, queue sources and played state, shuffle/repeat/volume, and play history, all over protocol v9 on schema 5.

**Architecture:** One new pure module (`src/listing.rs`) compiles a `ListDescription` to SQL. The library coordinator uses it to read pages, and the owner's `Store` uses it to resolve selections for enqueue. A second pure module (`src/play_order.rs`) decides the next and previous entries, and `Player` consults it instead of `cursor + 1`. Schema changes land as one transactional migration, 4 → 5. Every new wire command is a variant of `protocol::Command`, routed the way existing ones are.

**Tech Stack:** Rust 2024, rusqlite 0.37 (bundled SQLite with window functions), ffmpeg-next 9, tokio, serde.

**Spec:** `docs/superpowers/specs/2026-10-18-library-ui-design.md` (Sections 4.7, 5.1–5.2 and 6 drive this plan; read the "Amendments during planning" section at its end).

**Client plan:** `docs/superpowers/plans/2026-10-18-library-client.md` consumes the wire contract defined in Tasks 7, 9–11 and 13–15.

## Global Constraints

- Protocol `VERSION = 9` (`src/protocol.rs`). Main SQLite `user_version = 5`. `MAX_PAGE_SIZE = 500` is unchanged.
- `MAX_RUN_ENTRIES = 100_000`: the largest selection one enqueue or playlist-add may resolve to.
- Recently Added window: `RECENT_WINDOW_SECS = 2_592_000` (30 days), measured against `date_added` in UTC seconds.
- Play counts when an occurrence's pass reaches `min(duration / 2, 240 s)`, or ends naturally. One pass counts at most once.
- Played-prefix retention: `PLAYED_RETENTION = 100` played entries kept in the queue, not counting the current entry.
- Volume is `0..=100` percent, default `100`. Output amplitude is `(percent / 100)²`. Gain stays before buffering. Volume applies after.
- Repeat modes `off | all | one`, default `off`. Shuffle is a bool, default `false`. The shuffle logic sits behind the `ShuffleStrategy` trait (required by the user).
- New filter fields: `genre:`, `year:` (`year:1959` or inclusive `year:1970..1979`), `albumartist:`.
- All new settings fields use `#[serde(default)]`, so schema-4 settings JSON still loads.
- The library **revision** changes only when rows visible to list queries change. Scan progress no longer bumps it.
- Gates for every task: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked --all-targets`.
- Work on branch `feature/library-redesign` cut from `main`. Merge to `main` when both plans are done. Commit messages carry no attribution lines.

## Review Focus

1. **A scan running while the user scrolls.** Indexing bumps the revision on every batch that installs tracks, so cached list windows and enqueue checks go stale often. Expected: list reads keep working, a stale enqueue is rejected with `stale_library_revision`, and audio is unaffected. The test goes in Task 4.
2. **Repeat One or Repeat All replays an entry whose token was already consumed.** Expected: the second pass plays from frame 0, `current_id` stays correct, and the position resets. The play counts once per pass. The tests go in Tasks 13 and 15.
3. **A 50,000-row run enqueue.** Every source path is canonicalized. Expected: one atomic commit, no partial queue, and the time is measured and reported. The test goes in Task 17. Correctness at 600 rows (more than one page) goes in Task 9.
4. **Unicode and odd tags.** Expected for the new fields:
   - Multiple GENRE keys, `;`-joined genres, and genres differing only in case dedupe to one genre (first spelling kept).
   - A date like `1959-08-17` gives year 1959. Garbage dates give no year.
   - Tests go in Task 2.
5. **Upgrading a populated schema-4 database while the app is open.** Expected: queue, playlists, smart rules and checkpoint survive. New fields are blank until the automatic rescan reaches each file. Recently Added starts empty, not full. Tests go in Tasks 1 and 4.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/store.rs` (modify) | migration step 4 → 5; queue row columns; `Loaded` carries queue entry metadata |
| `src/library/db.rs` (modify) | `migrate_v5` library DDL; `install` writes new columns and genres; content-revision bumps |
| `src/library/mod.rs` (modify) | automatic upgrade rescans; route new list commands |
| `src/library/query.rs` (modify) | list pages, album/artist/genre lists, album summaries, totals |
| `src/listing.rs` (create) | `ListDescription` types and `compile` → SQL; shared by library queries and `Store` |
| `src/metadata.rs` (modify) | extract year, genres, codec, bit depth, bitrate |
| `src/scanner.rs` (modify) | `Candidate.created_ns` from file birth time |
| `src/filter.rs` (modify) | `genre:`, `year:`, `albumartist:` |
| `src/library_types.rs` (modify) | new `TrackMetadata` fields; `LibraryState` totals; list result types |
| `src/play_order.rs` (create) | `RepeatMode`, `ShuffleStrategy`, `UniformShuffle`, `PlayOrder` |
| `src/playback.rs` (modify) | `Player` uses `PlayOrder`; replace-upcoming; fresh token per pass |
| `src/output.rs` (modify) | `Output::set_volume` applied in `BlockConsumer::consume` |
| `src/owner.rs` (modify) | new commands; selection resolution; play counting; trim |
| `src/store/history.rs` (create) | `track_stats`, `play_history` writes and reads |
| `src/store/selection.rs` (create) | resolve `TrackSelection` to `(track_id, path)` inside one read transaction |
| `src/protocol.rs` (modify) | v9 commands, responses, settings, state fields |
| `tests/support/library_lists.rs` (create) | wire-level integration tests, included from `tests/server_transport.rs` |
| `src/bin/tyranny-likes-smoke-ctl.rs`, `scripts/server_gate.py`, `scripts/scale_gate.py` (modify) | v9 and scale measurements |
| `macos/Sources/TyrannyLikesSmoke/WireClient.swift`, `WireModels.swift` (modify) | v9 compatibility only (Task 16) |

---

### Task 1: Schema 5 migration

**Files:**
- Modify: `src/store.rs:60-125` (migration ladder)
- Modify: `src/library/db.rs` (add `migrate_v5`)
- Modify: `src/store/playlists.rs` (nothing beyond what the ladder calls)
- Test: `src/store.rs` `mod tests`

**Interfaces:**
- Produces: `pub(crate) fn migrate_v5(transaction: &Transaction<'_>) -> Result<()>` in `src/library/db.rs`, re-exported from `library`.
- Produces: schema-5 tables and columns, used by every later task:
  - `library_tracks` + `year INTEGER`, `codec TEXT`, `bit_depth INTEGER`, `bitrate_kbps INTEGER`, `file_size INTEGER`, `date_added INTEGER NOT NULL DEFAULT 0`
  - `library_assets` + `metadata_version INTEGER NOT NULL DEFAULT 1`, `created_ns INTEGER`
  - `library_state` + `pending_upgrade INTEGER NOT NULL DEFAULT 0`
  - `library_genres(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, search TEXT NOT NULL UNIQUE)`
  - `library_track_genres(track_id REFERENCES library_tracks(id), genre_id REFERENCES library_genres(id), position INTEGER NOT NULL, PRIMARY KEY(track_id,genre_id))`
  - `track_stats(track_id INTEGER PRIMARY KEY REFERENCES library_tracks(id), play_count INTEGER NOT NULL DEFAULT 0, last_played INTEGER)`
  - `play_history(id INTEGER PRIMARY KEY AUTOINCREMENT, played_at INTEGER NOT NULL, entry_id INTEGER NOT NULL, track_id INTEGER REFERENCES library_tracks(id), path TEXT NOT NULL, title TEXT NOT NULL, artist TEXT NOT NULL, album TEXT NOT NULL, duration_ms INTEGER, source_kind TEXT, source_name TEXT)`
  - `queue` + `source_kind TEXT`, `source_name TEXT`, `run_id INTEGER`, `played_seq INTEGER`
  - indexes: `library_tracks(year,id)`, `library_tracks(date_added,id)`, `library_track_genres(genre_id,track_id)`, `track_stats(play_count,track_id)`, `track_stats(last_played,track_id)`
- The migration sets `pending_upgrade=1` when `library_tracks` is non-empty.

- [ ] **Step 1: Write the failing test** `schema_4_database_migrates_to_5_preserving_state`. Build a schema-4 file by running the existing ladder with the target capped at 4. Extract `fn migrate(connection: &mut Connection, target: i64) -> Result<i64>` from `Store::open` so the test can call it. Insert one root, asset, track, queue row, regular playlist entry, smart playlist, and checkpoint. Then `Store::open` it. Assert:
```rust
assert_eq!(user_version(&path), 5);
assert_eq!(loaded.entries.len(), 1);
assert_eq!(loaded.checkpoint.current_id, Some(1));
assert_eq!(pending_upgrade(&path), 1);
assert_eq!(column_default(&path, "library_tracks", "date_added"), 0);
assert!(table_exists(&path, "play_history") && table_exists(&path, "track_stats"));
```
Also assert an empty schema-4 database migrates with `pending_upgrade == 0`.
- [ ] **Step 2: Run** `cargo test --locked store::tests::schema_4` — expect FAIL (`unsupported database version` is not reached; the version stays 4 or the helpers are missing).
- [ ] **Step 3: Implement** the `migrate` extraction, the `version == 4` step calling `library::migrate_v5` plus the queue `ALTER`s in one transaction, and `ensure!(version == 5, …)`.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS (existing tests open fresh databases through the full ladder).
- [ ] **Step 5: Commit** `git commit -m "Migrate main database to schema 5"`.

### Task 2: Extract year, genres, codec, bit depth, bitrate

**Files:**
- Modify: `src/library_types.rs` (`TrackMetadata`)
- Modify: `src/metadata.rs` (`Tags::tag`, `probe_before`)
- Test: `src/metadata.rs` `mod tests`, `src/scanner.rs` `native_wave_probe_returns_real_audio_and_unicode_tags`

**Interfaces:**
- Produces new `TrackMetadata` fields, each `#[serde(default)]`: `year: Option<i32>`, `genres: Vec<String>`, `codec: Option<String>`, `bit_depth: Option<u16>`, `bitrate_kbps: Option<u32>`.
- Produces `pub(crate) fn year_from(value: &str) -> Option<i32>`: the first run of exactly four ASCII digits in 1000..=2999.
- Produces `pub(crate) fn split_genres(value: &str) -> impl Iterator<Item = &str>`: splits on `;` and `\0`, trims, drops empties.
- Produces `pub(crate) fn codec_label(codec: ffmpeg::codec::Id, container: &str) -> &'static str`:
  - FLAC, MP3, AAC, ALAC, Opus, Vorbis, WavPack, APE, Musepack.
  - `pcm_*` maps to the container (`wav` → WAV, `aiff` → AIFF, else PCM).
  - Anything else gives `"Other"`.
- Produces `pub fn format_label(codec: Option<&str>, bit_depth: Option<u16>, sample_rate: u32, bitrate_kbps: Option<u32>) -> String` in `library_types.rs`:
  - lossless with a depth: `"FLAC 24/96"`, `"WAV 16/44.1"` (sample rate in kHz, trailing `.0` dropped);
  - lossy: `"MP3 320"` (kbps);
  - otherwise the codec alone or `""`.
- Tag precedence for year: `date` → `year` → `originaldate` → `originalyear`. The first one that parses wins.
- Genres come from every `genre` key across container and stream. They dedupe case-insensitively and keep the first spelling.
- Bit depth is set only for lossless codecs (FLAC, ALAC, WAV/AIFF/PCM, WavPack, APE). It is taken from `bits_per_raw_sample`, falling back to `bits_per_coded_sample`. Zero means none.
- Bitrate: the stream `bit_rate`, else the container `bit_rate`, ÷ 1000, rounded. Zero means none.

- [ ] **Step 1: Write failing tests** `year_tags_parse_dates_and_reject_garbage` (`"1959-08-17"→1959`, `"1971"→1971`, `"c. 1985?"→1985`, `"85"→None`, `"0000"→None`), `genre_values_split_dedupe_and_keep_first_spelling` (tags `genre=Jazz; Modal`, `GENRE=jazz` → `["Jazz","Modal"]`), `format_labels_follow_codec_class` (`("FLAC",Some(24),96000,None)→"FLAC 24/96"`, `("WAV",Some(16),44100,None)→"WAV 16/44.1"`, `("MP3",None,44100,Some(320))→"MP3 320"`, `(None,None,0,None)→""`), and `legacy_metadata_json_without_new_fields_deserializes`.
- [ ] **Step 2: Extend** `native_wave_probe_returns_real_audio_and_unicode_tags` to assert `codec == Some("WAV")` and `bit_depth == Some(16)`, and that a `genre` INFO tag round-trips into `genres`.
- [ ] **Step 3: Run** `cargo test --locked metadata:: scanner::` — expect FAIL.
- [ ] **Step 4: Implement** the fields and functions above in the files named.
- [ ] **Step 5: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 6: Commit** `git commit -m "Extract year, genres and stream format metadata"`.

### Task 3: Index the new fields

**Files:**
- Modify: `src/scanner.rs` (`Candidate`, `fingerprint`)
- Modify: `src/library/db.rs` (`install`, `next_batch`)
- Test: `src/library/tests.rs`

**Interfaces:**
- Consumes: Task 1 columns; Task 2 `TrackMetadata` fields.
- Produces: `Candidate { path, size, modified_ns, created_ns: Option<i64> }`. `created_ns` comes from `metadata.created()` (birth time) and falls back to `modified_ns`.
- Produces: `pub(super) const METADATA_VERSION: i64 = 2;`. `next_batch`'s unchanged fast path also requires `a.metadata_version = METADATA_VERSION`.
- `install` writes:
  - `year`, `codec`, `bit_depth`, `bitrate_kbps`, `file_size = candidate.size`;
  - `date_added`, only when the stored value is 0. The value is `min(created_ns / 1e9, now)` in seconds;
  - `metadata_version = 2` and `created_ns` on the asset.
- `install` replaces the track's `library_track_genres` rows in genre order (`position`). It upserts `library_genres` by `search = name.to_lowercase()`.
- The bare-word search corpus (`t.search`) appends the joined genres.

- [ ] **Step 1: Write failing tests**:
  - `rescan_reextracts_assets_with_old_metadata_version`. Import a FLAC with `genre=Jazz`. Force `metadata_version=1` and `year=NULL` by SQL. Rescan with an unchanged file. Assert `updated == 1`, the year is restored, and `unchanged == 0`.
  - `date_added_uses_birth_time_and_survives_reindex`. Import, read `date_added`, touch the file, rescan. Assert `date_added` is unchanged and `<= now`.
  - `genres_are_normalized_rows_in_tag_order`.
- [ ] **Step 2: Run** `cargo test --locked library::tests::` — expect FAIL.
- [ ] **Step 3: Implement** the interface above.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Index genres, format fields and date added"`.

### Task 4: Content revision and automatic upgrade rescan

**Files:**
- Modify: `src/library/db.rs` (`save_status`, `install`, `discover`, `probed`, `finish`)
- Modify: `src/library/mod.rs` (`Coordinator` gains `pending_rescans: VecDeque<u64>`)
- Test: `src/library/tests.rs`

**Interfaces:**
- Produces: `fn bump_revision(transaction: &Transaction<'_>) -> Result<()>`.
  - `save_status` stops incrementing `revision`.
  - The bump happens once per transaction, where that transaction:
    - installs at least one track;
    - sets an error on an already-indexed asset;
    - restores at least one tombstoned asset in `discover`;
    - advances `completed_scan` in `finish`.
- Produces: on `Coordinator::open`, `pending_upgrade=1` queues every root ID in `pending_rescans`. While the coordinator is idle, it starts the next rescan with the same path as `LibraryCommand::Rescan`. When the queue drains, it sets `pending_upgrade=0`. A root whose scan fails is not retried automatically.
- A user `Rescan` while upgrade rescans are pending follows today's `Busy` rule. The pending queue skips a root the user already scanned in this process.

- [ ] **Step 1: Write failing tests**:
  - `scan_progress_does_not_change_revision_but_installs_do`. Record the revision. Run a scan of 3 unchanged files and assert the revision is unchanged. Then change one file and rescan: the revision rises.
  - `stale_enqueue_revision_is_rejected_after_a_concurrent_install`. Covers Review Focus 1. `resolve_track` with the old revision after an install gives `StaleLibraryRevision`.
  - `upgrade_flag_rescans_every_root_once_and_clears`. Open a library with `pending_upgrade=1` and two roots. Wait for idle. Assert both roots' `last_scan.phase == Complete` and `pending_upgrade == 0`.
  - `upgraded_tracks_without_date_added_are_not_recent`. Covers Review Focus 5: a track with `date_added=0` is excluded by the Recent source in Task 6. Write it here as a pending assertion on `date_added == 0`. Task 6 finishes it.
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets`. Expect PASS. Fix any existing test that asserted a revision bump on status-only saves. Change it to assert on scan status instead, and say so in the commit body.
- [ ] **Step 5: Commit** `git commit -m "Bump library revision only for visible content changes"`.

### Task 5: Filter fields `genre:`, `year:`, `albumartist:`

**Files:**
- Modify: `src/filter.rs` (`Field`, tokenizer for `..`, `compile`)
- Test: `src/library/tests.rs` `structured_filters_*`

**Interfaces:**
- Produces: `Field::{Genre, Year, AlbumArtist}`.
  - `genre:x` is `EXISTS(SELECT 1 FROM library_track_genres tg JOIN library_genres g ON g.id=tg.genre_id WHERE tg.track_id=t.id AND instr(g.search,?)>0)`.
  - `albumartist:x` matches `library_albums.album_artist` lowercased, through `t.album_id`.
  - `year:1959` is `t.year=?`. `year:1970..1979` is `t.year BETWEEN ? AND ?`.
  - Year values must be 4-digit integers. Anything else is an `InvalidFilter` at the value's position.
- Lists keep working: `year:[1959, 1961]` and `genre:["Jazz","Folk"]`.

- [ ] **Step 1: Write failing tests** `structured_filters_support_genre_year_and_album_artist`. Expect exact ordered IDs over `structured_filter_fixture` extended with years and genres. Also `year_ranges_are_inclusive_and_malformed_years_are_located` (`year:19x9` gives an error at byte 5; `year:1979..1970` is an error).
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Add genre, year and album artist filter fields"`.

### Task 6: `ListDescription` and its SQL compiler

**Files:**
- Create: `src/listing.rs`
- Modify: `src/lib.rs` (`pub mod listing;`)
- Test: `src/listing.rs` `mod tests` (in-memory schema-5 fixture)

**Interfaces:**
- Produces wire types (serde `snake_case`, `deny_unknown_fields`):
```rust
#[serde(tag = "kind")]
pub enum ListSource { Library, Album { id: u64 }, Artist { id: u64 }, AppearsOn { artist_id: u64 },
    Compilations, Genre { id: u64 }, Recent, Playlist { id: u64, revision: u64 }, Smart { id: u64, revision: u64 } }
pub struct Browser { pub genre_ids: Vec<u64>, pub artist_ids: Vec<u64>, pub compilations: bool, pub album_ids: Vec<u64> } // #[serde(default)]
pub enum SortField { Title, Artist, AlbumArtist, Album, Duration, Year, Genre, Format, Plays, DateAdded, LastPlayed, AlbumOrder, PlaylistOrder }
pub struct SortKey { pub field: SortField, pub descending: bool }
pub enum Grouping { None, Artist, Album, ArtistAlbum }
pub struct ListDescription { pub source: ListSource, #[serde(default)] pub browser: Browser, pub query: String, pub sort: Vec<SortKey>, pub group: Grouping }
```
- Produces: `pub(crate) struct CompiledList { pub sql: String, pub values: Vec<Value> }` and `pub(crate) fn compile(list: &ListDescription, recent_cutoff: i64, first_parameter: usize) -> Result<CompiledList>`.
  - `sql` is one `SELECT` producing the columns `position, row_id, track_id, artist_key, album_key, available`, ordered by `position` (1-based, from `ROW_NUMBER() OVER (ORDER BY …)`).
  - Callers wrap it as a subquery.
- Rules the tests pin:
  - **Visibility:** library-type sources require `a.indexed=1 AND a.last_seen_scan>=r.completed_scan`. `Playlist` rows are occurrences (`row_id = playlist_entries.id`), include unavailable tracks, and report `available`. Every other source has `row_id = track_id` and `available = 1`.
  - **`artist_key`** is `0` for compilation albums, else `library_albums.artist_id` (album artist). **`album_key`** is `t.album_id`.
  - **Sources:**
    - `Artist { id }`: non-compilation albums whose album artist is `id`.
    - `AppearsOn { artist_id }`: tracks on compilation albums whose track artist is `artist_id`.
    - `Compilations`: compilation albums.
    - `Genre`: tracks having the genre.
    - `Recent`: `t.date_added > 0 AND t.date_added >= recent_cutoff`.
    - `Smart`: the stored rule AND `query`.
    - `Playlist` and `Smart` check the playlist revision. A mismatch is `PlaylistFailure` with `StalePlaylistRevision`.
  - **Browser:** each non-empty dimension filters with `IN`. `compilations: true` adds compilation albums to the artist dimension. Dimensions combine with AND, and AND with `query`.
  - **Sort:** explicit keys come first. Then the fixed tiebreak: album artist search, album `title_sort`, `album_id`, disc, track, `title_sort`, path, `row_id`. `Genre` sorts by the genre at `position=0`. `Format` sorts by codec, bit depth, sample rate, bitrate. `Plays` and `LastPlayed` come from `track_stats` (missing means 0 or NULL; NULLs sort last ascending and descending).
    - An empty `sort` means `AlbumOrder` for Album, Artist and Genre sources, `PlaylistOrder` for Playlist, `DateAdded desc` for Recent, and otherwise `AlbumArtist`.
  - **Grouping (spec 4.3 rule):** for each group level, the order key is `MAX(sort expr)` when the first sort key is descending, `MIN` when ascending, `OVER (PARTITION BY group key)`. Then the group key. Then the row sort. `ArtistAlbum` applies the artist level, then the album level.

- [ ] **Step 1: Write failing tests**, each asserting the exact ordered `row_id` vector on one fixture (3 album artists, 1 compilation with 2 track artists, 2 genres, years, 1 track with `date_added=0`, a regular playlist with a duplicate occurrence and an unavailable track):
  - `library_source_default_order_and_tiebreak`
  - `sort_by_each_field_ascending_and_descending` (one case per `SortField` except `PlaylistOrder`)
  - `album_grouping_orders_groups_by_newest_track_when_sorted_by_date_added_desc`
  - `artist_album_grouping_puts_compilations_under_key_zero`
  - `appears_on_lists_compilation_tracks_by_track_artist`
  - `browser_dimensions_and_query_combine_with_and`
  - `recent_excludes_unknown_and_old_date_added` (finishes Task 4's pending assertion)
  - `playlist_source_keeps_duplicates_unavailable_rows_and_checks_revision`
  - `smart_source_applies_rule_and_query`
  - `list_description_wire_shape_round_trips` (JSON literal for every `ListSource` kind; unknown fields rejected)
- [ ] **Step 2: Run** `cargo test --locked listing::` — expect FAIL.
- [ ] **Step 3: Implement** `compile` per the rules. The filter parser is `crate::filter::Filter::parse(query, first_parameter)`.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Compile list descriptions to ordered SQL"`.

### Task 7: List queries, summaries and totals over protocol v9

**Files:**
- Modify: `src/library_types.rs` (result types, `LibraryCommand`, `LibraryReply`, `LibraryState`)
- Modify: `src/library/query.rs`, `src/library/db.rs` (`state`)
- Modify: `src/protocol.rs` (`VERSION = 9`, commands, responses), `src/owner.rs` (routing, as for `LibraryTracks`)
- Test: `src/library/tests.rs`, `src/protocol.rs` `mod tests`, `tests/support/library_lists.rs` (create; add `#[path] mod library_lists;` in `tests/server_transport.rs`)

**Interfaces:**
- Consumes: `listing::{ListDescription, compile}` (Task 6).
- Produces types in `library_types.rs`:
```rust
pub struct TrackSummary { pub id: u64, pub album_id: u64, pub artist_id: u64, pub album_artist_id: u64, pub title: String,
  pub artist: String, pub album: String, pub album_artist: String, pub compilation: bool, pub disc_number: u32,
  pub track_number: u32, pub duration_ms: Option<u64>, pub year: Option<i32>, pub genre: String /* ", "-joined */,
  pub format: String /* format_label */, pub sample_rate: u32, pub bitrate_kbps: Option<u32>, pub play_count: u64,
  pub last_played: Option<i64>, pub date_added: i64, pub path: String }
pub struct ListRow { pub row_id: u64, pub artist_key: u64, pub album_key: u64, pub available: bool, pub track: TrackSummary }
pub struct ListTotals { pub tracks: u64, pub duration_ms: u64, pub size_bytes: u64 }
pub struct AlbumSummary { pub id: u64, pub title: String, pub album_artist: String, pub album_artist_id: u64, pub compilation: bool,
  pub year: Option<i32>, pub genre: String, pub track_count: u64, pub disc_count: u64, pub duration_ms: u64, pub format: String,
  pub album_gain_db: Option<f64>, pub date_added: i64, pub musicbrainz_release_id: Option<String> }
pub struct NamedCount { pub id: u64, pub name: String, pub album_count: u64, pub track_count: u64 }
pub enum AlbumSort { AlbumArtist, Year, DateAdded }
```
  - `AlbumSummary.format` is the most common track format, or `"Mixed"`. `genre` is the most common first genre. `date_added` is the newest track's.
- `LibraryState` gains `duration_ms: u64, size_bytes: u64`, which are totals over visible tracks, computed in `db::state`.
- Commands, with Response variants of the same names:
  - `ListTracks { list, revision: Option<u64>, offset, limit }` → `ListTracks { revision, total, offset, entries: Vec<ListRow>, totals: Option<ListTotals> }`. `totals` is present only when `offset == 0`.
  - `ListAlbums { list, sort: AlbumSort, descending: bool, revision, offset, limit }` → `ListAlbums { revision, total, offset, entries: Vec<AlbumSummary> }`. It covers albums with at least one row in `list`. `list.sort` and `list.group` are ignored.
  - `ListArtists { query, revision, offset, limit }` → `ListArtists { revision, total, offset, entries: Vec<NamedCount>, compilations: Option<NamedCount> }`. Entries are album artists of non-compilation albums. `compilations` (with `id: 0`, `name: "Compilations"`) is present at `offset == 0` when any exist.
  - `ListGenres { query, revision, offset, limit }` → `ListGenres { revision, total, offset, entries: Vec<NamedCount> }`.
  - `AlbumSummaries { album_ids: Vec<u64> /* 1..=500 */, revision }` → `AlbumSummaries { revision, entries }`, in request order, skipping unknown IDs.
  - `ListGroups { list, revision: Option<u64> }` → `ListGroups { revision, total, groups: Vec<GroupSpan> }`, where `GroupSpan { artist_key: u64, album_key: Option<u64>, start: u64, count: u64 }`. It covers the whole list, in list order. It needs `list.group != None`. `album_key` is `None` for `Artist` grouping. For `ArtistAlbum` there is one span per album; consecutive spans sharing `artist_key` form one artist group. The client needs this to size grouped tables without loading every row. The response is bounded by the number of groups, and more than 100,000 groups is `InvalidCommand`.
- `recent_cutoff` is `now_secs - RECENT_WINDOW_SECS` at request time.
- Page bounds and stale-revision errors behave exactly as for `LibraryTracks`. The existing `Library*` commands stay unchanged.

- [ ] **Step 1: Write failing tests**:
  - Library unit tests: `list_tracks_pages_are_contiguous_and_totals_only_on_first_page` (600 rows, pages of 500 and 100 concatenate to the `compile` order), `album_summaries_aggregate_format_genre_and_newest_date`, `artists_exclude_compilations_and_report_compilations_row`, `genres_count_albums_and_tracks`, `group_spans_cover_the_list_contiguously_in_order` (spans sum to `total`, each `start` equals the previous `start + count`, keys match the rows at those positions).
  - Protocol tests: `v9_list_commands_require_every_field`.
  - Wire integration in `library_lists.rs`: `list_tracks_round_trips_over_the_socket_and_rejects_stale_revision`.
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets`. Expect PASS. `tests/server_transport.rs` asserts `protocol_version == VERSION`, so it follows the bump automatically.
- [ ] **Step 5: Commit** `git commit -m "Serve list, album, artist and genre queries over protocol v9"`.

### Task 8: Queue entry source, run and played state

**Files:**
- Modify: `src/protocol.rs` (`QueueItem`, new `QueueSource`, `SourceKind`)
- Modify: `src/store.rs` (`Loaded.entries`, `Edit::Append`, `queue_page`), `src/store/playlists.rs` (`insert_tracks`, `enqueue_playlist` write the new columns)
- Create: `src/play_order.rs` (only `RunInfo`, `RepeatScope`); modify `src/lib.rs`
- Modify: `src/playback.rs` (`Entry` gains `run: Option<RunInfo>`), `src/owner.rs` (stamp sources)
- Test: `src/store.rs` tests, `tests/support/library_lists.rs`

**Interfaces:**
- Produces:
```rust
pub enum SourceKind { Album, Playlist, Smart, Songs, Genre, Artist, Recent, Compilations, Drag, Direct }
pub struct QueueSource { pub kind: SourceKind, pub name: String /* ≤ 256 bytes */ }
pub struct QueueItem { pub id: u64, pub path: String, pub track_id: Option<u64>,
    pub source: Option<QueueSource>, pub run_id: Option<u64>, pub played: bool,
    pub title: String, pub artist: String, pub album: String, pub album_id: Option<u64>, pub duration_ms: Option<u64> }
// NowPlaying (protocol.rs) also gains: pub track_id: Option<u64>, pub album_id: Option<u64>, pub format: String, pub gain_db: Option<f64>
pub struct RunInfo { pub run_id: u64, pub scope: RepeatScope }  // create src/play_order.rs with these two types; Task 11 extends the module
pub enum RepeatScope { Run, Queue } // Run for Album | Playlist | Smart; Queue otherwise
```
- `Loaded.entries: Vec<LoadedEntry { id, path, run: Option<RunInfo>, played_seq: Option<u64> }>`.
- `played` is `played_seq IS NOT NULL`.
- `QueueItem` display fields come from `library_tracks` through `track_id` (`queue_page` joins it). Entries without a track, or whose track is gone, use the file stem as the title and empty strings for artist and album.
- `NowPlaying.format` is `format_label` of the indexed track, or `""`. `gain_db` is the gain the decoder applied (`Decoder::gain_db`), or `None` when the mode is off.
- One enqueue gives every new entry `run_id = first new entry id`.
- `EnqueuePlaylist` stamps `Playlist` or `Smart` with the playlist name. `Append` stamps `Direct`. `AppendTrack` and `InsertTracks` stamp `Drag`.

- [ ] **Step 1: Write failing tests**: `queue_rows_persist_source_run_and_played_through_restart` (store), `queue_page_returns_track_display_fields_and_stem_fallback` (store), `enqueue_playlist_stamps_source_and_one_run_id` (wire), and `now_playing_reports_format_and_applied_gain` (wire, album gain mode on a fixture with gain tags).
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Record queue entry source, run and played state"`.

### Task 9: Selections and `EnqueueTracks`

**Files:**
- Create: `src/store/selection.rs`
- Modify: `src/protocol.rs`, `src/owner.rs`, `src/playback.rs` (`prepare_replace_upcoming`, `commit_replace_upcoming`), `src/store.rs` (`replace_upcoming` persistence)
- Test: `src/store/selection.rs` tests, `src/playback.rs` `batch_tests`, `tests/support/library_lists.rs`

**Interfaces:**
- Consumes: `listing::compile` (Task 6), `QueueSource` (Task 8).
- Produces wire types:
```rust
#[serde(tag = "kind")]
pub enum TrackSelection {
    Ids { library_revision: u64, track_ids: Vec<u64> },                       // 1..=500
    List { list: ListDescription, library_revision: u64,
           ranges: Vec<[u64; 2]>,          // half-open row positions (0-based), sorted, disjoint, 1..=64
           excluded: Vec<u64>,             // row_ids, ≤ 500
           first_row_id: Option<u64> },    // must equal the row at ranges[0][0]
}
pub enum Placement { Append, Next, ReplaceUpcoming }
Command::EnqueueTracks { selection: TrackSelection, expected_revision: u64, placement: Placement, play: bool, source: QueueSource }
// → Response::Applied { snapshot, entry_id: Some(first new entry) }
```
- Produces: `impl Store { pub fn resolve_selection(&mut self, selection: &TrackSelection, recent_cutoff: i64) -> Result<Vec<(u64, PathBuf)>> }`. It runs in one read transaction, checks the library revision, and checks the playlist revision for playlist sources.
  - A selection that resolves to 0 rows, or to more than `MAX_RUN_ENTRIES`, is `InvalidCommand`.
  - A `first_row_id` mismatch is `StaleLibraryRevision` (`"list changed; reload"`).
  - An unavailable playlist row inside the selection is `InvalidCommand`, matching `playlist_sources`.
  - `u64::MAX` as a range end means "to the end".
- Placement semantics:
  - `Append`: after the last entry.
  - `Next`: immediately after the current entry, or at the end if nothing is current.
  - `ReplaceUpcoming`: remove every entry after the current one, then append. With nothing current, it replaces the whole queue except played entries.
  - `play: true` selects the first new entry, as `Command::Play { entry_id }` does.
- The whole command is one store transaction plus one infallible player commit, the same prepare/commit pattern as `insert_tracks`.

- [ ] **Step 1: Write failing tests**:
  - `ids_and_list_selections_resolve_identically_for_the_same_rows`.
  - `ranges_and_exclusions_select_exact_rows_in_list_order`.
  - `first_row_id_mismatch_is_stale`.
  - `selection_limits_are_enforced` (0 rows, 64+ ranges, 501 ids).
  - `replace_upcoming_keeps_current_pcm_and_played_entries`, a `batch_tests` style test.
  - `run_enqueue_of_600_rows_plays_from_clicked_row` (wire): list of 600, range `[[120, u64::MAX]]`, `ReplaceUpcoming`, `play`. Assert the queue has 480 new entries starting at row 120, `current_id` is the first, and history is kept.
  - `keep_mode_inserts_run_before_existing_upcoming` (wire, `Next` + `play`).
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Enqueue list selections atomically"`.

### Task 10: `PlaylistAddSelection`

**Files:**
- Modify: `src/protocol.rs`, `src/owner.rs` (`edit_playlist`), `src/store/playlists.rs` (`PlaylistEdit::Add` takes resolved IDs up to `MAX_RUN_ENTRIES` when they come from a selection)
- Test: `tests/support/library_lists.rs`

**Interfaces:**
- Consumes: `TrackSelection`, `Store::resolve_selection` (Task 9).
- Produces: `Command::PlaylistAddSelection { playlist_id, expected_revision, selection: TrackSelection, before: Option<u64> }` → `PlaylistApplied`. Smart and unavailable-source rules match `PlaylistAdd`.

- [ ] **Step 1: Write failing test** `playlist_add_selection_adds_all_matching_rows_atomically` (700 rows through a `List` selection, one revision bump, order kept) and `playlist_add_selection_rejects_smart_destinations`.
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Add list selections to playlists"`.

### Task 11: `play_order` module

**Files:**
- Modify: `src/play_order.rs` (created in Task 8)
- Test: `src/play_order.rs` `mod tests`

**Interfaces:**
- Produces:
```rust
#[serde(rename_all = "snake_case")] pub enum RepeatMode { #[default] Off, All, One }
pub trait ShuffleStrategy: Send { /// index into `candidates`, which is non-empty
    fn choose(&mut self, candidates: &[u64]) -> usize; }
pub struct UniformShuffle { state: u64 } // xorshift64*; no new dependency
impl UniformShuffle { pub fn seeded(seed: u64) -> Self; pub fn from_entropy() -> Self /* SystemTime nanos ^ pid */ }
pub struct Slot { pub id: u64, pub run: Option<RunInfo>, pub played: bool }
pub struct PlayOrder { /* repeat, shuffle: Option<Box<dyn ShuffleStrategy>>, planned: Option<u64>, history: VecDeque<u64> (≤ 100) */ }
impl PlayOrder {
    pub fn new(repeat: RepeatMode, shuffle: Option<Box<dyn ShuffleStrategy>>) -> Self;
    pub fn set_repeat(&mut self, mode: RepeatMode); pub fn set_shuffle(&mut self, s: Option<Box<dyn ShuffleStrategy>>);
    pub fn invalidate(&mut self);                                   // after any queue edit or mode change
    pub fn next(&mut self, slots: &[Slot], current: Option<usize>) -> Option<usize>; // decided once, cached until invalidate/advance
    pub fn previous(&self, slots: &[Slot], current: usize) -> Option<usize>;
    pub fn advanced(&mut self, to: u64);                            // record actual play for history and clear the plan
    pub fn wrapped(&self, slots: &[Slot], current: usize, next: usize) -> Option<std::ops::Range<usize>>; // context to reset played flags
}
```
- Rules:
  - **Context of index `i`:** if `slots[i].run.scope == Run`, the maximal contiguous range sharing `run_id`; else `0..len`.
  - **Off, no shuffle:** `i+1`, or `None` at the end.
  - **One:** `i`.
  - **All, no shuffle:** `i+1` inside the context, else the context start.
  - **Shuffle:** the candidates are unplayed slots in the context, excluding `i`, in index order. `choose` picks one. With no candidates: `None` under Off, or under All the whole context minus `i` (the context's played flags reset, signalled through `wrapped`).
  - **`previous`:** under shuffle, the most recent `history` entry still in `slots`; else the spike rule (`i-1` when the position is under 3 s, which the caller handles).

- [ ] **Step 1: Write failing tests**: `sequential_off_ends_at_queue_end`, `repeat_one_returns_current`, `repeat_all_wraps_within_album_run_not_whole_queue`, `repeat_all_wraps_whole_queue_for_songs_source`, `shuffle_never_repeats_until_context_exhausted` (seeded; 20 slots, collect 20 picks, all distinct), `shuffle_with_all_wraps_and_reports_reset_range`, `next_is_stable_until_invalidated`, `previous_follows_actual_history_under_shuffle`, `uniform_shuffle_seeded_is_deterministic`.
- [ ] **Step 2: Run** `cargo test --locked play_order::` — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Add play order with repeat contexts and pluggable shuffle"`.

### Task 12: Clear Upcoming, Shuffle Upcoming, Save Upcoming, played-prefix trim

**Files:**
- Modify: `src/protocol.rs`, `src/owner.rs`, `src/playback.rs` (`prepare_remove_batch`/`commit_remove_batch` if no batch remove exists), `src/store.rs`, `src/store/playlists.rs`
- Test: `tests/support/library_lists.rs`, `src/playback.rs` `batch_tests`

**Interfaces:**
- Produces commands (each → `Applied`, except `SaveUpcoming` → `PlaylistApplied`):
  - `ClearUpcoming { expected_revision }`: removes every entry after the current one, and the current one keeps playing. With nothing current, it removes every unplayed entry.
  - `ShuffleUpcoming { expected_revision }`: a uniform permutation of the entries after the current one, using `UniformShuffle` and `ShuffleStrategy::choose` from Task 11.
  - `SaveUpcoming { expected_revision, expected_playlists_revision, name }`: creates a regular playlist from the entries after the current one that have `track_id`. Fails `InvalidCommand` if there are none.
- Played-prefix trim: when an entry becomes current (natural advance, Next, Previous, Play, or a run enqueue with `play`), the owner sets its `played_seq`, the next value of `MAX(played_seq)+1`. In the same store transaction, it deletes played entries beyond the newest `PLAYED_RETENTION` by `played_seq`, never the current one. The trim bumps the queue revision like any removal.

- [ ] **Step 1: Write failing tests**: `clear_upcoming_keeps_current_audible`, `shuffle_upcoming_permutes_only_entries_after_current`, `save_upcoming_creates_regular_playlist`, `played_prefix_is_trimmed_to_100` (a 105-entry queue of 0.05 s WAVs played through the clocked capture, which leaves 100 played, the current one, and the unplayed).
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Add upcoming-only queue commands and played trim"`.

### Task 13: Player uses `PlayOrder`; repeat and shuffle commands

**Files:**
- Modify: `src/playback.rs` (`produce`, `snapshot`, `relocate_decoder`, `rebuild_future`), `src/owner.rs`, `src/protocol.rs` (`Settings`)
- Test: `tests/playback.rs`, `tests/support/library_lists.rs`

**Interfaces:**
- Consumes: Task 11.
- `Player` holds a `PlayOrder`.
  - `produce` advances with `order.next(..)` instead of `cursor += 1`, and `None` ends the queue.
  - `snapshot().next_id` and `previous_id` come from the order.
  - Any queue mutation and any mode change calls `order.invalidate()` and rebuilds future tokens, as edits do now.
- `Slot.played` comes from the entry's `played_seq` (Task 12). When `order.wrapped(..)` returns a range, the owner clears `played_seq` for those entries in the same store transaction as the advance. Repeat All therefore starts a fresh pass, and HISTORY empties for that context.
- **Fresh token per pass:** when the next index's entry has a token that was submitted or consumed, it gets `TrackToken::new(id)` before decoding. This covers Repeat One, Repeat All wrap, and shuffle revisits.
- `Settings` gains `#[serde(default)] repeat: RepeatMode` and `#[serde(default)] shuffle: bool`.
- Commands `SetRepeat { mode }` and `SetShuffle { enabled }` → `Applied`. They persist through `store.checkpoint` and apply to the player.

- [ ] **Step 1: Write failing tests** (clocked capture, as in `tests/playback.rs`):
  - `repeat_one_replays_the_entry_gaplessly_with_correct_position`: two passes of a 0.2 s WAV produce exactly 2× its PCM. Mid-pass-2, `current_id` is unchanged and `position_frames` is below the pass length.
  - `repeat_all_album_run_wraps_without_reaching_following_entries`.
  - `shuffle_plays_each_upcoming_entry_once_then_stops_under_off`.
  - `repeat_and_shuffle_settings_survive_restart` (wire).
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS. The existing gapless, seek and move tests must stay green unchanged.
- [ ] **Step 5: Commit** `git commit -m "Drive playback order through repeat and shuffle modes"`.

### Task 14: Output volume

**Files:**
- Modify: `src/output.rs` (`Shared` gains `volume_bits: AtomicU32`; `BlockConsumer::consume` scales samples), `src/owner.rs`, `src/protocol.rs`
- Test: `src/output.rs` tests, `tests/playback.rs`

**Interfaces:**
- Produces: `impl Output { pub fn set_volume(&self, percent: u8) }`. It stores the amplitude `(percent/100)²` as `f32` bits.
  - `consume` ramps linearly from the previous amplitude to the new one across the frames of one call, then holds it.
  - Capture (clocked) output is scaled identically, so tests observe it.
- `Settings` gains `#[serde(default = "full")] volume: u8` (100). `SetVolume { percent }` (≤ 100, else `InvalidCommand`) → `Applied`. It is persisted, and applied on open.

- [ ] **Step 1: Write failing tests**: `volume_scales_captured_pcm_by_square_law` (50% gives 0.25× amplitude after the ramp; 0% gives silence; 100% is bit-identical to today), `volume_change_ramps_without_a_step`, `volume_setting_persists_and_rejects_over_100`.
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Apply server output volume after buffering"`.

### Task 15: Play history and counts

**Files:**
- Create: `src/store/history.rs`
- Modify: `src/owner.rs` (counting in the pump/publish loop), `src/protocol.rs`, `src/listing.rs` (Plays/LastPlayed already join `track_stats`)
- Test: `src/store/history.rs` tests, `tests/support/library_lists.rs`

**Interfaces:**
- Produces: `impl Store { pub fn record_play(&mut self, play: &PlayRecord) -> Result<()>; pub fn history_page(&self, offset: usize, limit: usize) -> Result<Page<HistoryEntry>>; pub fn history_revision(&self) -> Result<u64> }`. `record_play` writes the `play_history` row and upserts `track_stats` (`play_count+1`, `last_played`) in one transaction.
- `PlayRecord { entry_id, track_id: Option<u64>, path, title, artist, album, duration_ms, source: Option<QueueSource>, played_at: i64 }`.
- `HistoryEntry { id, played_at, track_id: Option<u64>, title, artist, album, duration_ms: Option<u64>, source: Option<QueueSource> }`, newest first.
- Owner rule: per pass (per `TrackToken`), when position reaches `min(duration/2, 240 s)`, or the entry ends naturally, record once. A seek does not create a new pass. Entries without `track_id` record history without stats.
- `ControlState.history_revision: u64` is `MAX(play_history.id)`, or 0.
- `Command::History { revision: Option<u64>, offset, limit }` → `Response::History { revision, total, offset, entries }`.
- Recording does not change the library revision.

- [ ] **Step 1: Write failing tests**: `a_pass_counts_once_at_half_duration_despite_seeks`, `a_long_track_counts_at_four_minutes` (unit test of the threshold function `fn count_threshold_frames(duration_frames: Option<u64>) -> Option<u64>`), `repeat_one_counts_each_pass`, `unindexed_entries_record_history_without_stats`, `history_pages_newest_first_and_revision_tracks_inserts`, `plays_sort_uses_track_stats`.
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test --locked --all-targets` — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Record play history and counts"`.

### Task 16: v9 compatibility for existing clients and gates

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmoke/WireClient.swift` (version 9 in hello check and messages), `macos/Sources/TyrannyLikesSmoke/WireModels.swift` (decode new `ControlState`, `Settings`, `LibraryState`, `QueueItem` fields), `src/bin/tyranny-likes-smoke-ctl.rs`, `scripts/server_gate.py` (`expected_version`)
- Test: Swift builds; existing Python gates

**Interfaces:**
- Swift additions are decode-only: `Settings.volume: UInt8`, `repeat: String`, `shuffle: Bool`; `LibraryState.duration_ms`, `size_bytes`; `ControlState.history_revision`; `QueueItem.source`, `run_id`, `played`. The UI is unchanged.

- [ ] **Step 1: Run** `swift build --package-path macos --scratch-path target/swift-client -Xswiftc -warnings-as-errors`. Expect success before the edits; this is the baseline.
- [ ] **Step 2: Edit** the files named above.
- [ ] **Step 3: Run** the same Swift build in debug and in release (`-c release`) — expect success.
- [ ] **Step 4: Run** `python3 scripts/server_gate.py --server target/debug/tyranny-likes-smoke-server` (after `cargo build --locked --bins`). Expect it to pass. Fix only version and field expectations.
- [ ] **Step 5: Commit** `git commit -m "Speak protocol v9 from the existing client and gates"`.

### Task 17: Scale measurements for lists and runs

**Files:**
- Modify: `scripts/scale_gate.py` (`seed` fills `year`, `codec`, `bit_depth`, `date_added`, genres and `track_stats`; new measurement families)
- Test: the gate itself

**Interfaces:**
- New families, 30 samples each, p50/p95/max reported:
  - `list_tracks_first_window` (`Library`, default sort, offset 0, limit 200)
  - `list_tracks_deep_window` (offset 49,800)
  - `list_tracks_sorted_year_desc`
  - `list_tracks_grouped_artist_album`
  - `list_tracks_filtered_genre`
  - `list_albums_by_year`
  - `enqueue_run_50k`, which is 3 samples of a `ReplaceUpcoming` of the whole library, reporting wall time and asserting queue length.
- Targets: p95 < 100 ms for list families. `enqueue_run_50k` is reported without a target (spec 8).

- [ ] **Step 1: Extend** `seed` and add the families.
- [ ] **Step 2: Run** `python3 scripts/scale_gate.py --server target/release/tyranny-likes-smoke-server --app "target/tyranny likes smoke.app" --work-dir target/scale/gate` after `scripts/build_macos.sh`. Record the results in `spike-results/scale-gate.json`.
- [ ] **Step 3: If** any list family misses p95 < 100 ms, report it with the query plan (`EXPLAIN QUERY PLAN`). Fix it only by adding indexes or rewriting the SQL inside `listing::compile`. Do not change the wire. Rerun.
- [ ] **Step 4: Commit** `git commit -m "Measure list and run enqueue at 50k tracks"`.

### Task 18: Server verification

- [ ] **Step 1: Run** `cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked --all-targets`. Expect every gate to pass, with no new ignored tests.
- [ ] **Step 2: Run** `python3 scripts/library_gate.py`, `playlist_gate.py`, `smart_playlist_gate.py`, `filter_gate.py`, `artwork_gate.py` in server-only mode (no `--app`). They should pass, or fail only on documented v9 field changes; fix those.
- [ ] **Step 3: Append** a "Library redesign — server" section to `SPIKE_RESULTS.md` with the test counts, gate results, and scale numbers.
- [ ] **Step 4: Commit** `git commit -m "Record server verification for library redesign"`.
