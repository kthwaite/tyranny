# Listening Stats and Wrapped Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record every play with listened time and end reason (radio included), aggregate the
history into a Stats page and a yearly Wrapped slideshow.

**Architecture:** The player emits play lifecycle events (open, count, close). The owner turns
them into `play_history` row writes through a retrying queue. Stats queries run on their own
read-only connection on a worker thread. They load play rows and aggregate them in pure Rust
functions, so tests need no clock or time zone. The Swift client adds a Stats destination and
a full-window Wrapped overlay, with the period and slide logic in `TyrannyLikesSmokeCore` so it can be
tested.

**Tech Stack:** Rust 2024, rusqlite (bundled), serde, tokio; Swift 6.2, SwiftUI, Swift Charts
(macOS 14), swift-testing.

**Spec:** `docs/superpowers/specs/2026-10-21-listening-stats-design.md`. Read it first; this plan
does not repeat its definitions (play, counted play, listened time, end reason, skip, skim,
artist key, track key).

## Global Constraints

- Branch `feature/listening-stats`, from `dev`. Merge back into `dev` only.
- Protocol `VERSION` stays at the branch's starting value (14) on the branch. Task 13 bumps it in the
  merge into `dev`, to one more than `dev`'s version then. That task also updates every copy
  `CLAUDE.md` lists.
- Store schema goes 7 → 8 through the ladder in `src/store.rs`.
- Count threshold for library tracks is unchanged: `count_threshold_frames` (half or 240 s).
- Radio count threshold: 60 s listened. Skim: under 5 s listened (5_000 ms).
- History checkpoint: listened time written at most every 30 s while a play is open.
- Weeks start Monday. Buckets use the Mac's local time zone.
- Top lists: 25. Most skipped: 10, minimum 3 non-radio non-skim starts.
- Wrapped: auto-advance 6 s; fewer than 50 counted plays → single "not enough" slide.
- Wrapped call to action: 1–31 December, current year.
- Stats queries never run on the owner thread. Playback never waits for history writes.
- No new crate dependencies. Time-zone offsets come from `libc::localtime_r` (`tm_gmtoff`).
- Gates, run before every commit that ends a task:
  - `just test`
  - `prefix="$(python3 scripts/build_ffmpeg.py)" && FFMPEG_DIR="$prefix" MACOSX_DEPLOYMENT_TARGET=14.0 cargo clippy --locked --target-dir target/portable-rust --all-targets -- -D warnings`
  - `cargo fmt --check`
  - `just build-client`

  Run all four once before Task 1, and record any failures that already exist. A task may not
  add new failures.

## Review Focus

1. **Holding ⌘→ to race through the queue.** It opens many sub-second plays. They must be skims:
   recorded, but absent from skip rate and most skipped. (Task 6, `skims_are_left_out_of_skip_stats`.)
2. **Removing the playing entry or clearing the queue.** The row was opened at start, so it must
   close (`skipped` and `stopped` respectively) and not be left open. (Task 4,
   `removing_the_current_entry_closes_its_play_as_skipped`, `clearing_the_queue_closes_its_play_as_stopped`.)
3. **Pausing for an hour, then quitting.** The pause is not listened time. The play stays open
   across the restart and continues when playback resumes. (Task 3, `pause_is_not_listened_time`;
   Task 4, `restart_continues_the_restored_play`.)
4. **A station that never sends titles, or repeats the same title.** One junk radio play builds up
   station time. It never counts, and a repeated identical title does not split it. (Task 3,
   `repeated_radio_text_does_not_split_the_play`, `junk_radio_play_never_counts`.)
5. **A brand-new install with no history.** Every report field is empty or zero, `skip_rate` is
   `None`, lapsed loves is empty, and Wrapped is "not enough". The client shows empty states, not
   errors. (Task 6, `empty_history_gives_an_empty_report`; Task 7,
   `empty_year_has_no_optional_points`; Task 11, `fewerThanFiftyPlaysGiveTheNotEnoughSlide`.)

---

### Task 1: Store schema 8 and the play row API

**Files:**
- Modify: `src/store.rs` (ladder step 8; `SCHEMA_VERSION = 8`; re-exports)
- Modify: `src/store/history.rs` (replace `record_play`, `PendingPlays`)
- Test: `src/store/history.rs` (`mod tests`), `src/store.rs` (migration test next to the
  existing `table_exists` test ~line 1193)

**Interfaces:**
- Produces (in `crate::store`, re-exported from `history.rs`):
  - `pub(crate) enum EndReason { Finished, Skipped, Stopped }` with `fn as_str(self) -> &'static str` (`"finished"`, `"skipped"`, `"stopped"`) and `fn parse(&str) -> Option<Self>`.
  - `PlayRecord` gains `pub raw_title: Option<String>`. `PlayRecord::of` sets it `None`.
  - `pub(crate) struct OpenPlayRow { pub row: i64, pub entry_id: u64, pub counted: bool, pub listened_ms: u64 }`
  - `Store::open_play(&mut self, record: &PlayRecord) -> Result<i64>`: inserts `counted=0, listened_ms=0, ended_at NULL`. Returns the row ID.
  - `Store::count_play(&mut self, row: i64) -> Result<()>`: sets `counted=1` and updates `track_stats` (using the row's `track_id`, `played_at`) in one commit. A no-op if the row has already counted.
  - `Store::checkpoint_play(&mut self, row: i64, listened_ms: u64) -> Result<()>`
  - `Store::close_play(&mut self, row: i64, listened_ms: u64, reason: EndReason, ended_at: i64) -> Result<()>`
  - `Store::open_play_rows(&self) -> Result<Vec<OpenPlayRow>>`, oldest first.
  - `Store::close_open_plays_except(&mut self, keep: Option<i64>) -> Result<usize>`: closes each other open row as `stopped` with `ended_at = played_at + listened_ms / 1000`.
  - `Store::history_revision(&self) -> Result<u64>`: `COUNT(*) WHERE counted=1`.
  - `Store::stats_revision(&self) -> Result<u64>`: a persisted counter.
  - `pub(crate) enum HistoryEvent { Open { play: u64, record: PlayRecord }, Count { play: u64 }, Checkpoint { play: u64, listened_ms: u64 }, Close { play: u64, listened_ms: u64, reason: EndReason, ended_at: i64 } }`
  - `pub(crate) struct PendingHistory` with:
    - `push(&mut self, event: HistoryEvent) -> Option<HistoryEvent>`: returns the oldest event, dropped once the queue exceeds `MAX_PENDING_PLAYS`.
    - `adopt(&mut self, play: u64, row: i64)`
    - `write(&mut self, store: &mut Store) -> HistoryWrite`, where `pub(crate) struct HistoryWrite { pub wrote: bool, pub counted: bool, pub error: Option<anyhow::Error> }`
    - `len()` and `is_empty()`.
- Removes: `Store::record_play` and `PendingPlays`. Task 4 updates their callers. Until then,
  keep `owner.rs` compiling with a minimal adapter (open, then count, then close `finished`
  for each old counted play), and mark it `// Task 4 replaces this`.

- [ ] **Step 1: Write the failing tests**

In `src/store/history.rs` tests, reuse `Database::open()` and `play(...)`:

```rust
#[test] fn schema_7_rows_survive_as_counted_and_untimed()  // in src/store.rs: migrate a target-7 db with one play_history row to 8;
    // assert counted=1, listened_ms IS NULL, end_reason IS NULL, ended_at IS NULL; index on played_at exists
#[test] fn an_open_play_is_absent_from_history_and_stats_until_it_counts() -> Result<()>
    // open_play(track 7) → history_page total 0, track_stats empty, history_revision 0, stats_revision > before
    // count_play(row) → history total 1, track_stats (7,1,played_at), history_revision 1
    // count_play(row) again → still (7,1,…), history_revision 1
#[test] fn closing_records_listened_time_and_reason() -> Result<()>
    // checkpoint_play(row, 12_000) then close_play(row, 31_500, Skipped, t+40) → row has 31500, "skipped", t+40
#[test] fn close_open_plays_except_keeps_one_and_stops_the_rest() -> Result<()>
    // three open rows with listened 10_000/20_000/0; keep = second → returns 2;
    // first: "stopped", ended_at = played_at + 10; second still open
#[test] fn pending_history_writes_in_order_and_maps_rows() -> Result<()>
    // push Open{play:1}, Count{1}, Close{1, 200_000, Finished, t}; write → wrote, counted, error None; row closed and counted
#[test] fn pending_history_drops_events_of_a_play_whose_open_was_dropped()
    // push MAX_PENDING_PLAYS+1 events where the first is Open{play:9}; later Count{9} is discarded on write, no panic, no row
#[test] fn stats_revision_moves_on_every_write_and_history_revision_only_on_count() -> Result<()>
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test --locked --target-dir target/portable-rust --lib store::` (with the FFmpeg env prefix from the gates)
Expected: compile errors for the missing items.

- [ ] **Step 3: Implement**

Ladder step 8, in one transaction:
- add the columns `listened_ms INTEGER`, `ended_at INTEGER`, `end_reason TEXT`,
  `counted INTEGER NOT NULL DEFAULT 1`, `raw_title TEXT`;
- `CREATE INDEX play_history_played_at ON play_history(played_at)`;
- `CREATE TABLE history_control(singleton INTEGER PRIMARY KEY CHECK(singleton=1), writes INTEGER NOT NULL)`,
  seeded with `writes = COALESCE(MAX(play_history.id),0)`.

Every write method bumps `history_control.writes` in its own transaction. `history_page`
filters `counted=1`, in both its `COUNT` and its `SELECT`. `PendingHistory::write` stops at the
first error and leaves that event and the later ones queued. A `Close` removes its play from
the row map.

- [ ] **Step 4: Run them and confirm they pass**

Run the same command. Expected: PASS. Then run the full gates.

- [ ] **Step 5: Commit** — `git commit -m "Record plays as open rows with listened time and end reason"`

---

### Task 2: Radio text

**Files:**
- Create: `src/playback/radio.rs` (declare `mod radio;` in `src/playback.rs`)

**Interfaces:**
- Produces: `pub(crate) struct RadioText { pub artist: String, pub title: String, pub raw: String, pub junk: bool }` and `pub(crate) fn parse(raw: &str, station: &str) -> RadioText`.

- [ ] **Step 1: Write the failing tests** (inline `mod tests`)

```rust
#[test] fn splits_at_the_first_spaced_dash()
    // parse("Burial - Archangel - Edit", "NTS") → artist "Burial", title "Archangel - Edit", junk false
#[test] fn without_a_separator_the_text_is_the_title()
    // parse("Archangel", "NTS") → artist "", title "Archangel", junk false
#[test] fn blank_station_name_and_letterless_text_are_junk()
    // "", "   ", "NTS" (== station, case-insensitive "nts"), "- 03:00 -" → junk true, artist "", title "", raw kept
#[test] fn trims_both_parts()
    // "  Burial  -  Archangel " → "Burial", "Archangel"
```

- [ ] **Step 2: Confirm they fail.** Then **Step 3: Implement `parse`.** Then **Step 4: Confirm they pass**, and run the gates.
- [ ] **Step 5: Commit** — `git commit -m "Parse station now-playing text into artist and title"`

---

### Task 3: Player play events

**Files:**
- Modify: `src/playback/order.rs` (`Pass`, `hear`, `count_heard`, `count`, `restored_selection`, `CountedPlay`)
- Modify: `src/playback.rs` (stream title change at the `take_update` site ~line 532; delegate methods)
- Test: `src/playback/order.rs` tests (reuse the `silence(...)` fixture helper ~line 661). Use
  `tests/playback.rs` with `tests/support/http_streams.rs` for radio.

**Interfaces:**
- Consumes: `EndReason` (Task 1), `radio::{parse, RadioText}` (Task 2).
- Produces, on `Player`:
  - `pub enum PlayEvent { Opened { play: u64, entry_id: u64, started_at: i64, radio: Option<RadioText> }, Counted { play: u64 }, Closed { play: u64, listened_ms: u64, reason: EndReason, ended_at: i64 } }`
  - `pub fn take_play_events(&mut self) -> Vec<PlayEvent>`, oldest first. It replaces `take_counted_plays`.
  - `pub fn open_play(&self) -> Option<(u64, u64)>`: (play key, listened ms) of the open play, for checkpoints.
  - `pub fn end_play(&mut self, reason: EndReason)`: closes the open play.
  - `pub fn restore_play(&mut self, entry_id: u64, counted: bool, listened_ms: u64) -> u64`: registers the open row's play for the restored selection. Returns its play key.
- Removes: `CountedPlay` and `take_counted_plays`.

Rules (from the spec, stated here as they apply to the player):
- **Play keys** are a monotonic `u64` counter in the player. A play key lives on the `Pass` and is
  carried into continued passes (seek, gain change) the same way `started_at` and `counted` are
  today.
- **Opening**: a play opens where `hear` sets `started_at`. A pass that is already counted when
  its play opens (the `restored_selection` path with no restored row) opens uncounted. Its close
  reason is then forced to `Stopped`.
- **Listened time**: on each `hear` or `count_heard`, the delta of `OutputSnapshot.rendered_frames`
  since the last call goes to the play of the latest heard valid pass. Convert at `SAMPLE_RATE`
  to milliseconds.
- **Counting**: `Counted` is emitted where `count` sets `pass.counted` today, for file entries.
  A radio play emits `Counted` once its listened time reaches 60 s and it is not junk. Radio
  plays never count on the queue-end path.
- **Closing**:
  - `Finished` when a pass before `latest` is dropped in `hear`, or when the queue ends drained.
  - `end_play(reason)` closes with `reason`, with three exceptions:
    - `Skipped` becomes `Stopped` for a radio play;
    - `Skipped` becomes `Stopped` for a restored play that was never heard since the restart;
    - `Skipped` becomes `Stopped` for the forced case above.
  - A play whose passes all drop without either of these closes as `Stopped`.
- **Radio splits**: when `take_update` changes a heard stream entry's title, the current radio play
  closes `Finished` and a new one opens with `parse(new_title, station)`. An update with the same
  title does nothing. The first radio play of a stream pass opens on first hear, parsed from the
  current title. Before any ICY data arrives, that title is the station name, so the play is junk.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_track_heard_to_its_end_opens_counts_and_finishes()
    // events == [Opened{play:p, entry_id}, Counted{p}, Closed{p, listened_ms≈duration (±50ms), Finished}]
#[test] fn end_play_skipped_before_the_threshold_closes_uncounted()
    // play 1 s of a 10 s file, end_play(Skipped) → no Counted, Closed{reason: Skipped, listened_ms≈1000}
#[test] fn pause_is_not_listened_time()
    // play 1 s, pause, pump for 2 s wall time, play 1 s → open_play().1 ≈ 2000 (±100)
#[test] fn seeking_continues_the_same_play_without_double_counting()
    // seek forward 5 s of 10 s then back 3 s: one play key throughout; listened ≈ frames rendered, not position
#[test] fn restored_play_keeps_its_key_and_counted_state()
    // restore_play(entry, true, 90_000) then play 1 s → no Opened, no Counted; open_play() == (key, ≈91_000)
#[test] fn restored_play_never_heard_closes_as_stopped()
    // restore_play(...) then end_play(Skipped) without playing → Closed{reason: Stopped}
// tests/playback.rs, local HTTP stream from tests/support/http_streams.rs with ICY titles:
#[test] fn radio_text_change_finishes_one_radio_play_and_opens_the_next()
#[test] fn repeated_radio_text_does_not_split_the_play()
#[test] fn junk_radio_play_never_counts()
// order.rs unit test: feed listened frames to the radio count check directly, no wall clock:
#[test] fn radio_play_counts_after_sixty_seconds()  // 59.9 s → no Counted; 60 s → Counted once; junk at 120 s → never
#[test] fn leaving_a_station_closes_as_stopped()      // end_play(Skipped) on radio → Stopped
```

- [ ] **Step 2: Confirm they fail.** Then **Step 3: Implement**, following the rules above.
- [ ] **Step 4: Confirm they pass**, and run the gates. The existing order and playback tests must
  stay green, and none of them may be weakened.
- [ ] **Step 5: Commit** — `git commit -m "Emit play open, count and close events from the player"`

---

### Task 4: Owner records the play lifecycle

**Files:**
- Modify: `src/owner.rs`:
  - fields `pending_plays`, `retrying_plays`, `history_revision`, plus a new `stats_revision`;
  - `count_play`, `collect_plays`, `write_plays`;
  - `select` (~line 875), the remove, clear and stop command paths;
  - `run` (~line 2054), `Actor::open` (~line 642), `state()`.
- Modify: `src/protocol.rs`: `ControlState.stats_revision: u64`. Fix the doc comment on
  `history_revision` to say it changes when a play counts.
- Test: `src/owner/tests.rs`

**Interfaces:**
- Consumes: Tasks 1 and 3.
- Produces: `ControlState.stats_revision`. Snapshots publish whenever history writes change either revision.

Wiring:
- `PlayEvent::Opened` → describe the entry now with `store.queue_entry(entry_id)`, as
  `count_play` does today. Then push `HistoryEvent::Open`. For radio, build the record with
  `PlayRecord::of`, then override `artist`/`title` from `RadioText` (empty if junk), set
  `raw_title = Some(raw)`, `duration_ms = None`, and source `{kind: Stream, name: station}` if the
  item has none.
- `Counted` → `Count`. `Closed` → `Close`.
- Every 30 s (`HISTORY_CHECKPOINT_INTERVAL`), push `Checkpoint` from `player.open_play()`.
- `select` with a non-seek jump to an entry other than the open play's entry calls
  `player.end_play(Skipped)` before `commit_selection`.
- Removing the current entry calls `end_play(Skipped)`. Clearing the queue and stopping call
  `end_play(Stopped)`.
- Shutdown and disconnect push a final `Checkpoint` and write. They do **not** close the open play.
- `Actor::open`:
  - read `open_play_rows()`;
  - keep the newest row whose `entry_id` is the checkpoint's `current_id`;
  - call `close_open_plays_except(keep)`;
  - if a row was kept, call `pending.adopt(player.restore_play(entry_id, counted, listened_ms), row)`.
- The "take events before advances" ordering comment in `run` still holds. Take play events
  before `take_advances` and before any trim (`collect_plays` call sites).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn skipping_mid_track_records_an_uncounted_skipped_play()
    // play entry A 1 s, Next → A's row: counted 0, end_reason "skipped"; history page empty; history_revision unchanged; stats_revision moved
#[test] fn removing_the_current_entry_closes_its_play_as_skipped()
#[test] fn clearing_the_queue_closes_its_play_as_stopped()
#[test] fn restart_continues_the_restored_play()
    // play past threshold, shut down, start owner on same db, resume → still one row for that play, counted 1, listened grows, track_stats play_count 1
#[test] fn restart_closes_other_open_rows_as_stopped()
#[test] fn history_write_failures_queue_events_without_stalling_playback()
    // follow the existing write-failure test pattern for pending plays if present; otherwise make the db read-only mid-play and assert transport replies still arrive
```

- [ ] **Step 2: Confirm they fail.** Then **Step 3: Implement**, and delete the Task 1 adapter.
- [ ] **Step 4: Confirm they pass**, and run the gates. Run `scripts/server_gate.py` if it
  exercises history. Check the gate's header for how to invoke it.
- [ ] **Step 5: Commit** — `git commit -m "Record every play's lifecycle from the owner"`

---

### Task 5: Calendar and periods

**Files:**
- Create: `src/stats/mod.rs` (module root; `pub(crate) mod period;`), `src/stats/period.rs`
- Modify: `src/lib.rs` (`mod stats;`)
- Create: `src/stats_types.rs`, with only `PeriodKind` and `PeriodRequest` for now (Task 6 adds the rest). Add `pub mod stats_types;` in `src/lib.rs`, next to `library_types`.

**Interfaces:**
- Produces:
  - `stats_types`: `#[serde(rename_all="snake_case")] pub enum PeriodKind { Day, Week, Month, Year, All }`; `pub struct PeriodRequest { pub kind: PeriodKind, pub anchor: String }` (`YYYY-MM-DD`).
  - `period`: `pub(crate) trait Zone { fn offset_secs(&self, utc: i64) -> i32; }`; `pub(crate) struct SystemZone;` (`libc::localtime_r`, `tm_gmtoff`); `pub(crate) struct FixedZone(pub i32);` (tests)
  - `pub(crate) struct Date { pub year: i32, pub month: u8, pub day: u8 }` with `fn parse(&str) -> Option<Date>`
  - `pub(crate) struct Local { pub date: Date, pub weekday: u8 /* 0 = Monday */, pub hour: u8 }`, `pub(crate) fn local(zone: &dyn Zone, utc: i64) -> Local`
  - `pub(crate) fn start_of_day(zone: &dyn Zone, date: Date) -> i64` (UTC seconds of local midnight)
  - `pub(crate) struct Bounds { pub start: i64, pub end: i64 }` (UTC, `[start, end)`)
  - `pub(crate) fn bounds(zone: &dyn Zone, kind: PeriodKind, anchor: Date, first_play: Option<i64>, now: i64) -> Bounds`. `All` is `[first_play.unwrap_or(now), now + 1)`.
  - `pub(crate) fn buckets(zone: &dyn Zone, kind: PeriodKind, bounds: Bounds) -> Vec<i64>` (sub-bucket starts: All → years, Year → months, Month/Week → days, Day → hours)

- [ ] **Step 1: Write the failing tests** (inline `mod tests`)

```rust
// A test zone with a DST rule: +0 before 2026-03-29T01:00Z, +3600 until 2026-10-25T01:00Z, +0 after (Europe/London 2026).
#[test] fn week_starts_on_monday()            // anchor 2026-10-21 (Wed) → start = start_of_day(2026-10-19), end = start_of_day(2026-10-26)
#[test] fn week_across_dst_is_169_hours()      // anchor 2026-10-21 under the London zone → end - start == 169 * 3600
#[test] fn month_and_year_bounds()             // 2026-02-10 → Feb 1..Mar 1; 2024-02 has 29 days; year 2026 → Jan 1..Jan 1 2027
#[test] fn day_buckets_are_hours_and_dst_day_has_25()
#[test] fn local_weekday_and_hour()            // 2026-10-19T23:30Z under +3600 → Tuesday (1), hour 0
#[test] fn rejects_bad_anchor()                // Date::parse("2026-13-01"), ("2026-02-30"), ("x") → None
```

- [ ] **Steps 2–4: confirm they fail, implement, confirm they pass, run the gates.** Use the civil-from-days and days-from-civil algorithms. Local midnight to UTC is `l - offset(l - offset(l))`.
- [ ] **Step 5: Commit** — `git commit -m "Add local calendar periods for stats"`

---

### Task 6: Stats report aggregation

**Files:**
- Create: `src/stats/report.rs`, `src/stats/rows.rs`
- Modify: `src/stats_types.rs` (report wire types)

**Interfaces:**
- Consumes: Task 5.
- Produces:
  - `rows.rs`:
    - `pub(crate) struct PlayRow { pub played_at: i64, pub listened_ms: Option<u64>, pub counted: bool, pub end_reason: Option<EndReason>, pub track_id: Option<u64>, pub artist: String, pub title: String, pub radio: bool, pub station: Option<String> }`
    - `pub(crate) struct LibraryIndex { pub artists: HashMap<String, u64>, pub tracks: HashMap<(String, String), u64>, pub artist_tracks: HashMap<u64, u64> }`. The maps hold unique keys only: a key that matches more than one library ID is left out.
    - `pub(crate) fn artist_key(name: &str) -> String` (trim, lowercase). Track key = `(artist_key(artist), artist_key(title))`.
    - `pub(crate) fn load(connection: &rusqlite::Connection) -> Result<(Vec<PlayRow>, LibraryIndex)>`, with rows ordered by `played_at`.
  - `report.rs`: `pub(crate) struct Thresholds { … }` with `impl Default` holding the spec's values:
    - `list_limit 25`, `most_skipped_limit 10`, `most_skipped_min_started 3`, `skim_ms 5_000`;
    - `lapsed_min_span_days 90`, `lapsed_gap_min_days 30`, `lapsed_gap_max_days 1_095`, `lapsed_plays_floor 10`;
    - `shallow_plays_floor 20`, `scale_plays 150`, `scale_span_days 1_826`, `shallow_top_n 3`, `shallow_share 0.6`.
  - `report.rs`: `pub(crate) fn report(rows: &[PlayRow], library: &LibraryIndex, zone: &dyn Zone, period: Bounds, kind: PeriodKind, now: i64, thresholds: &Thresholds) -> StatsReport`
  - `stats_types`: `StatsReport` with exactly the spec's fields. The spec's `top_artists` and
    `top_tracks` become `top_artists_by_plays`, `top_artists_by_time`, `top_tracks_by_plays` and
    `top_tracks_by_time`. It also carries `period_start` and `period_end`. The entry types are:
    - `RankedArtist { name, plays, listened_ms, artist_id: Option<u64> }`
    - `RankedTrack { title, artist, plays, listened_ms, track_id: Option<u64> }`
    - `Bucket { start: i64, listened_ms }`
    - `SkippedTrack { title, artist, started, skips, track_id }`
    - `StationTime { name, listened_ms }`
    - `LapsedLove { name, plays, last_played: i64, artist_id }`
    - `ShallowDive { name, plays, top_share: f64, top_tracks: Vec<String>, library_tracks: Option<u64>, artist_id }`

    `heatmap: Vec<[u64; 24]>` has 7 rows, Monday first. `skip_rate: Option<f64>`.

Rules not already pinned by the spec table:
- Rows with an empty artist key never enter artist stats. Rows with an empty title never enter track stats.
- Display name: the most frequent spelling in the key's group. Ties go to the latest play.
- Lapsed-love and shallow-dive thresholds scale per the spec. They count over all rows, whatever the period.
- Ranking ties break by the other measure, then by name.

- [ ] **Step 1: Write the failing tests** (`src/stats/report.rs` `mod tests`; build `PlayRow` fixtures with a small helper)

```rust
#[test] fn empty_history_gives_an_empty_report()        // all counts 0, lists empty, skip_rate None, heatmap 7×24 zeros
#[test] fn tiles_count_counted_plays_and_sum_known_time() // untimed_plays counts NULL-listened counted rows; listened_ms excludes them
#[test] fn skips_are_uncounted_skipped_plays()            // a late skip (counted, Skipped) is not a skip
#[test] fn skims_are_left_out_of_skip_stats()             // 20 rows listened 300 ms Skipped + 1 full play → skip_rate == Some(0.0)
#[test] fn radio_never_enters_skip_stats_and_junk_never_enters_artists()
#[test] fn radio_and_library_merge_by_case_insensitive_name() // "Burial" library + "burial" radio → one artist, artist_id from LibraryIndex
#[test] fn top_artists_by_plays_and_by_time_differ()
#[test] fn heatmap_buckets_listened_time_by_local_weekday_and_hour()
#[test] fn new_artists_are_first_heard_in_the_period()
#[test] fn most_skipped_needs_three_starts()
#[test] fn lapsed_loves_wait_for_ninety_days_of_history() // span 30 d → empty; span 400 d: gap = 100 d, min plays = max(10, 150*400/1826)=32
#[test] fn shallow_dives_need_sixty_percent_in_top_three() // includes library_tracks from artist_tracks
#[test] fn over_time_buckets_follow_the_period_kind()
```

- [ ] **Steps 2–4: confirm they fail, implement, confirm they pass, run the gates.** Add a `load` test against a real migrated store, using `Database::open()` from Task 1's tests or an equivalent helper.
- [ ] **Step 5: Commit** — `git commit -m "Aggregate play history into a stats report"`

---

### Task 7: Wrapped aggregation and stats years

**Files:**
- Create: `src/stats/wrapped.rs`
- Modify: `src/stats_types.rs`

**Interfaces:**
- Consumes: Tasks 5 and 6 (`PlayRow`, `LibraryIndex`, `Thresholds`, `artist_key`).
- Produces:
  - `pub(crate) fn wrapped(rows: &[PlayRow], library: &LibraryIndex, zone: &dyn Zone, year: i32, now: i64, thresholds: &Thresholds) -> WrappedReport`
  - `pub(crate) fn years(rows: &[PlayRow], zone: &dyn Zone) -> Vec<i32>`: years with at least one counted play, newest first.
  - `stats_types`:
    - `WrappedReport { year, through: i64, listened_ms, plays, artists, tracks, top_artists: Vec<WrappedArtist>, top_tracks: Vec<RankedTrack>, discovery: Option<Discovery>, peak_time: Option<PeakTime>, most_skipped: Option<SkippedTrack>, comeback: Option<Comeback>, top_station: Option<StationTime>, months: Option<Vec<MonthTop>> }`
    - `WrappedArtist { name, plays, listened_ms, share: f64 }`
    - `Discovery { name, first_played: i64, plays, listened_ms }`
    - `PeakTime { weekday: u8, band_start_hour: u8 }`, where bands are 0, 3, 6, …, 21
    - `Comeback { name, plays, gap_days: u64 }`
    - `MonthTop { month: u8, listened_ms, top_artist: Option<String> }`
  - Minima are constants in `wrapped.rs`, with the spec's values: discovery ≥ 5 counted plays,
    peak ≥ 10 h, most skipped ≥ 5 skips, comeback ≥ 10 plays, station ≥ 1 h, months ≥ 3 months
    with plays.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn empty_year_has_no_optional_points()       // every Option None, lists empty
#[test] fn top_artists_rank_by_time_with_share()     // shares sum ≤ 1.0, top first
#[test] fn top_tracks_rank_by_counted_plays()
#[test] fn discovery_is_first_heard_this_year()      // an artist first heard last year never qualifies
#[test] fn peak_time_picks_weekday_and_three_hour_band()
#[test] fn comeback_needs_a_gap_before_this_year()
#[test] fn months_need_three_months_with_plays()
#[test] fn through_is_now_for_the_current_year_and_year_end_otherwise()
#[test] fn years_lists_years_with_counted_plays_newest_first()
```

- [ ] **Steps 2–4: confirm they fail, implement, confirm they pass, run the gates.**
- [ ] **Step 5: Commit** — `git commit -m "Build the Wrapped report from a year of plays"`

---

### Task 8: Stats protocol commands and workers

**Files:**
- Modify: `src/protocol.rs`:
  - `Command::{Stats { period: PeriodRequest }, Wrapped { year: i32 }, StatsYears}`;
  - `Response::{Stats { stats_revision: u64, report: Box<StatsReport> }, Wrapped { stats_revision: u64, report: Box<WrappedReport> }, StatsYears { years: Vec<i32> }}`;
  - serde tests alongside `history_requires_every_field_and_replies_as_a_page`.
- Modify: `src/stats/mod.rs`: `StatsWorkers` and its `Handle`, modelled on `LyricsWorkers`
  (`src/lyrics/mod.rs:79-160`), but with one worker thread. It opens a read-only connection
  (`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`) on first use.
- Modify: `src/owner.rs`:
  - `Controller` gets a `stats` handle;
  - `Owner::start` starts `StatsWorkers::start(database)`;
  - `Controller::request` routes the three commands to it before the owner, as it does for
    `Command::Lyrics`;
  - shutdown stops the workers.
- Modify: `src/bin/tyranny-likes-smoke-ctl.rs` help, if it lists commands.
- Test: `tests/server_transport.rs`

**Interfaces:**
- Consumes: Tasks 5–7. Its `stats_revision` comes from `Store::stats_revision` semantics,
  read on the worker's connection as `SELECT writes FROM history_control`.
- Produces: the wire commands `stats`, `wrapped` and `stats_years`, used by Tasks 9–12.
  - Request: `{"command":"stats","period":{"kind":"week","anchor":"2026-10-21"}}`, reply `{"kind":"stats","stats_revision":N,"report":{…}}`.
  - A bad anchor → `Reply::Error` with message `"invalid period anchor"`.

- [ ] **Step 1: Write the failing tests**

```rust
// protocol.rs tests
#[test] fn stats_commands_require_every_field_and_reply_with_a_report() // deny_unknown_fields; kind snake_case
// tests/server_transport.rs
#[test] fn stats_reply_reflects_recorded_plays()        // play a fixture past threshold, then stats(week, today) → plays == 1
#[test] fn stats_does_not_wait_for_the_owner()          // issue stats while a slow owner command is in flight; stats replies first (follow how the directory tests prove this, if they do)
#[test] fn invalid_anchor_is_an_error_reply()
```

- [ ] **Steps 2–4: confirm they fail, implement, confirm they pass, run the gates.**
- [ ] **Step 5: Commit** — `git commit -m "Serve stats, Wrapped and stats years off the owner"`

---

### Task 9: Swift wire models and period logic

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmokeCore/WireModels.swift`: add `ControlState.stats_revision`, in the
  init, the `CodingKeys` and the decoder, following `history_revision`.
- Create: `macos/Sources/TyrannyLikesSmokeCore/Stats.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/StatsTests.swift`; extend `WireModelTests.swift`

**Interfaces:**
- Consumes: the wire shapes from Tasks 5–8. Property names are snake_case, as in the existing wire models.
- Produces:
  - Codable `StatsReport`, `RankedArtist`, `RankedTrack`, `Bucket`, `SkippedTrack`, `StationTime`, `LapsedLove`, `ShallowDive`, `StatsResponse { stats_revision, report }`, `WrappedReport` and its parts, `WrappedResponse`, `StatsYearsResponse { years: [Int] }`
  - `public enum StatsPeriodKind: String, CaseIterable, Sendable { case day, week, month, year, all }`
  - `public struct StatsPeriod: Equatable, Sendable { public var kind: StatsPeriodKind; public var anchor: Date }` with:
    - `static func current(_ kind: StatsPeriodKind, now: Date) -> StatsPeriod`
    - `func stepped(_ delta: Int, calendar: Calendar) -> StatsPeriod`: a no-op for `.all`.
    - `func canStepForward(now: Date, calendar: Calendar) -> Bool`
    - `func anchorString(calendar: Calendar) -> String`: `"yyyy-MM-dd"`.
    - `func label(calendar: Calendar) -> String`, e.g. `"Wed 21 Oct 2026"`, `"19–25 Oct 2026"`, `"28 Sep – 4 Oct 2026"`, `"29 Dec 2025 – 4 Jan 2026"`, `"October 2026"`, `"2026"`, `"All time"`.
    - `func drillDown(bucketStart: Date, calendar: Calendar) -> StatsPeriod?`: All → Year, Year → Month, Month and Week → Day, Day → nil.
  - `public extension Calendar { static func statsCalendar(timeZone: TimeZone = .current) -> Calendar }`: Gregorian, `firstWeekday = 2`.

- [ ] **Step 1: Write the failing tests** (swift-testing, fixed time zone, like `HistoryTests.swift`)

```swift
@Test func weekLabelsWithinOneMonth()       // anchor Wed 21 Oct 2026 → "19–25 Oct 2026"
@Test func weekLabelsAcrossMonthsAndYears()
@Test func steppingMovesByOnePeriod()       // month: 31 Jan + 1 → February; year −1; day +1
@Test func cannotStepPastTheCurrentPeriod()
@Test func drillDownNarrowsByOneLevel()
@Test func decodesAStatsReply()             // a JSON fixture matching Task 8's serde test output, including null skip_rate and a 7×24 heatmap
@Test func controlStateDecodesStatsRevision()
```

- [ ] **Steps 2–4:** `just test-client` fails, then implement, then `just test-client` passes. Run the gates.
- [ ] **Step 5: Commit** — `git commit -m "Model stats replies and periods in the client"`

---

### Task 10: Stats page

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmoke/Shell/Destination.swift`:
  - add `case stats`, with key `"stats"`, title `"Stats"`, symbol `"chart.bar"`;
  - `playing = [.upNext, .history, .stats, .radio]`;
  - add it to every exhaustive switch (`defaultGrouping` → `.none`).
- Modify: `macos/Sources/TyrannyLikesSmoke/Shell/MainWindowView.swift` (~line 181): add `case .stats` → `StatsView`.
- Modify: `macos/Sources/TyrannyLikesSmoke/Models/ListModel.swift` (~line 519), if its switch needs the new case.
- Create: `macos/Sources/TyrannyLikesSmoke/Views/StatsView.swift`, containing `StatsModel` (`@MainActor @Observable`, structured like `HistoryModel`) and the view.
- Create: `macos/Sources/TyrannyLikesSmoke/NativeStatsSmoke.swift`, following `NativeHistorySmoke.swift`. Register it where the other smokes are registered.

**Interfaces:**
- Consumes: Task 9.
- Produces:
  - `StatsModel` with `period: StatsPeriod`, `report: StatsReport?`, `error: String?`, `func sync()`, and `wrappedYears: [Int]`.
  - The app model owns one `StatsModel`, as it does `model.stations`, so the Go menu (Task 12) can read `wrappedYears`.

Behaviour:
- Fetch on appear, and when `period` changes. While the page is visible, refetch when
  `stats_revision` changes, at most once every 10 s. Refresh `wrappedYears` from `stats_years`
  on connect and when `stats_revision` changes, debounced the same way.
- The remembered period kind is stored under the preference key `stats.periodKind`. Default `.week`.
- Layout, from spec "Layout" items 1–7: the header, tiles, top artists and top tracks with
  plays/time toggles, a Swift Charts bar chart (tapping a bar applies `drillDown`), the
  heatmap, a row of three panels, and the all-history row. Empty states use the spec's copy.
  The lapsed-loves empty state is "Not enough history yet — fills in after 90 days". It shows
  the date it fills in, computed from `history_span_days`.
- Double-clicking a row with `artist_id` or `track_id` opens it the way other lists do. Find the
  existing navigation call, e.g. from `ArtistsView`.

- [ ] **Step 1:** Write the smoke assertions: Stats is reachable at ⌘8, the header shows a period
  label, and an empty-history server shows the empty states without an error.
- [ ] **Step 2:** Confirm the smoke fails. Follow how `NativeHistorySmoke` is run (see the smoke
  harness or `scripts/client_gate.py`).
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Confirm the smoke passes, and run the gates. Capture a screenshot of the page
  with a seeded history using `scripts/render_views.py`, if it supports new views. Read the
  screenshot.
- [ ] **Step 5: Commit** — `git commit -m "Add the Stats page"`

---

### Task 11: Wrapped deck logic

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/Wrapped.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/WrappedTests.swift`

**Interfaces:**
- Consumes: Task 9's `WrappedReport` types.
- Produces:
  - `public enum WrappedSlide: Equatable, Sendable { case year(listenedMs: UInt64, plays: UInt64), topArtist(WrappedArtist), topArtists([WrappedArtist]), topTrack(RankedTrack), topTracks([RankedTrack]), discovery(Discovery), peakTime(PeakTime), months([MonthTop]), mostSkipped(SkippedTrack), comeback(Comeback), topStation(StationTime), summary(artists: [WrappedArtist], tracks: [RankedTrack], listenedMs: UInt64), notEnough }`
  - `public enum WrappedDeck { public static func slides(_ report: WrappedReport) -> [WrappedSlide] }`, in the spec's order 1–12, leaving out absent points.
  - `public enum WrappedOffer`:
    - `static func callToActionYear(now: Date, calendar: Calendar) -> Int?`: the current year in December, otherwise nil.
    - `static func showsBanner(year: Int, dismissed: Set<Int>) -> Bool`
  - `public enum WrappedPhrase { static func peak(_ time: PeakTime) -> String }`.
    - Bands: 0 "late nights", 3 "small hours", 6 "early mornings", 9 "mornings", 12 "afternoons", 15 "late afternoons", 18 "evenings", 21 "late evenings".
    - Weekday 0 = Monday, so `{weekday: 1, band_start_hour: 18}` gives "Tuesday evenings".
  - `public struct SlideshowClock: Equatable, Sendable`:
    - `init(count: Int, interval: TimeInterval = 6)`
    - `index`, `progress` (0…1), `paused`
    - `mutating tick(_ dt: TimeInterval)`, `next()`, `previous()`, `togglePause()`
    - It never advances past the last slide.

- [ ] **Step 1: Write the failing tests**

```swift
@Test func fewerThanFiftyPlaysGiveTheNotEnoughSlide()   // plays 49 → [.notEnough]
@Test func absentPointsAreLeftOut()               // discovery nil, comeback nil → neither slide present; order otherwise 1…12
@Test func callToActionOnlyInDecember()           // 30 Nov → nil, 1 Dec → 2026, 31 Dec → 2026, 1 Jan → nil
@Test func dismissedBannerStaysDismissedForThatYear()
@Test func peakPhraseNamesDayAndBand()            // "Tuesday evenings"
@Test func clockAutoAdvancesEverySixSeconds()     // tick(5.9) → index 0; tick(0.2) → index 1, progress ≈ 0.0167
@Test func clockPausesAndStopsAtTheLastSlide()
```

- [ ] **Steps 2–4:** `just test-client` fails, then implement, then it passes. Run the gates.
- [ ] **Step 5: Commit** — `git commit -m "Choose Wrapped slides and pace the slideshow"`

---

### Task 12: Wrapped slideshow, call to action and menu

**Files:**
- Create: `macos/Sources/TyrannyLikesSmoke/Views/WrappedView.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/Shell/MainWindowView.swift` (overlay at the window root, above
  the sidebar and the player bar), `macos/Sources/TyrannyLikesSmoke/TyrannyLikesSmokeApp.swift` (the Go menu ~line 178),
  `macos/Sources/TyrannyLikesSmoke/Views/StatsView.swift` (the call to action and banner)
- Extend: `NativeStatsSmoke.swift`

**Interfaces:**
- Consumes: Tasks 10 and 11.
- Produces: a navigation state `wrappedYear: Int?` on the navigation object the menu already
  uses. Setting it opens the overlay; esc clears it.

Behaviour (spec "Client: Wrapped slideshow"):
- The overlay fetches `wrapped` once on open, and builds the slides with `WrappedDeck.slides`.
- A `TimelineView` drives `SlideshowClock`. Progress bars sit along the top.
- Keys, with `.onKeyPress`: → or click goes next, ← goes back, space pauses, esc closes.
- Each slide gets its own gradient. Animate in staggered: label, then headline, then detail,
  with list rows one by one. Under `accessibilityReduceMotion`, use fades only.
- Go menu: a `Menu("Wrapped")` submenu after the destinations. It has "\(year) so far" for the
  current year, then each past year from `wrappedYears`. It is disabled when `wrappedYears` is empty.
- On the Stats page, when `WrappedOffer.callToActionYear` is non-nil:
  - a header button "Your \(year) Wrapped ▶";
  - a banner the first time the page opens, unless the year is dismissed. Dismissed years are
    stored under the preference key `stats.wrappedBannerDismissed` (an array of `Int`).

- [ ] **Step 1:** Write the smoke assertions:
  - Go › Wrapped › "2026 so far" opens the overlay;
  - → moves to slide 2;
  - esc closes it;
  - with fewer than 50 plays, the overlay shows the "not enough" slide.
- [ ] **Step 2:** Confirm the smoke fails. Then **Step 3: Implement.** Then **Step 4:** confirm it
  passes, and run the gates. Capture a screenshot of two slides and the summary card, and read them.
- [ ] **Step 5: Commit** — `git commit -m "Add the Wrapped slideshow, call to action and menu"`

---

### Task 13: Merge into dev

**Files:**
- Modify: `src/protocol.rs` (`VERSION` and its `assert_eq!` test), `WireProtocol.version` in the
  Swift sources (find it via `tests/swift_constants.rs`), the version defaults in
  `scripts/*_gate.py`, and the version tags in `tyranny-likes-smoke-ctl` help and in doc comments.

- [ ] **Step 1:** Run `git switch dev && git log --oneline -1`
  and read `VERSION` in `src/protocol.rs` on `dev`. Call it D.
- [ ] **Step 2:** `git switch feature/listening-stats && git merge dev`. Resolve conflicts with
  superpowers:resolving-merge-conflicts if there are any.
- [ ] **Step 3:** Set `VERSION` to D+1. Grep for the old number (`grep -rn "\bD\b"`, scoped to the
  places listed above) and update each copy.
- [ ] **Step 4:** Run all four gates. Expected: all pass, including `tests/swift_constants.rs`.
- [ ] **Step 5:** Commit `Bump the protocol to D+1 for listening stats`. Then run
  `git switch dev && git merge --no-ff feature/listening-stats -m "Merge branch 'feature/listening-stats' into dev"`.
