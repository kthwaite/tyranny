# Listening Stats and Wrapped

Date: 2026-10-21. Scope: server (`src/`) and macOS client (`macos/`). Builds on the play
history (`src/store/history.rs`) and the library UI design (`2026-10-18-library-ui-design.md`).

## Intent

The user has a stats dashboard for their old Spotify export in despotify
(`~/source/despotify`, `despotify/stats.py`, `frontend/src/routes/StatsPage.tsx`). They want
the same picture of their listening habits for what they play in tyranny. They also want
something like Spotify Wrapped at the end of the year, generated from tyranny's own history.

The screen is for looking back. It is not for managing the library.

Stats cover only what tyranny records from now on. Nothing is imported from Spotify or
despotify. So tracking quality matters: nothing older backfills it.

## Goals

- Record every play that starts, with the time actually listened and how it ended
  (finished, skipped, stopped).
- Record internet radio listening as plays, one play per song the station announces.
- A **Stats** destination in the source list: one scrolling page, scoped by a period picker
  (Day, Week, Month, Year, All, with ‹ › to step between periods).
- Panels the user named: top artists, top tracks, heatmap, new artists, lapsed loves,
  shallow dives. Plus listening time and skips, which the user said both matter.
- **Wrapped**: a full-window, animated, auto-advancing slideshow. One data point per slide,
  leading with top tracks and top artists. Generated live from the history.
- Wrapped is offered with a call to action on the Stats page from 1 December. At any other
  time it is reachable from a menu item, for the year so far or any past year.

## Non-goals

- No import of Spotify or despotify history.
- No genre, mood or "era" stats. despotify's Wrapped-eras panel came from Spotify's export.
- No "heard a lot, never saved" panel.
- No splitting of multi-artist credits ("A feat. B" stays one artist name).
- No sharing, export or image rendering of Wrapped slides.
- No stats in tyranny-likes-smoke-ctl beyond sending the raw protocol commands.
- No changes to the History view's look. It keeps showing counted plays only.

## Anti-goals

- A stats query must never stall playback or the owner. Queries run off the owner (see
  "Server: queries").
- Recording must never pause playback. Today's rule in `write_plays` still holds: history
  writes retry, and playback never waits for them.
- Stats must not be confidently wrong about time. Rows with unknown listening time are
  left out of time sums. They are not estimated.
- The tracking change must not change which plays count, or how `track_stats` play counts
  and last-played values behave.

## Decisions (made with the user)

| Topic | Decision |
|---|---|
| Data source | Only tyranny's own history, from now on. |
| Granularity | Every started play, with listened time and end reason. Skips and accurate listening time both matter. |
| Radio | Feeds stats. One play per announced song. Accepted "as a first cut". |
| Stats layout | One scrolling page with a period picker (mockup option A). |
| Periods | Top lists by day, week, month, year and all time. |
| Wrapped form | Full-window, Spotify-style: bold gradients, big type, story-style progress bars, auto-advance. |
| Wrapped content | A focused digest: top tracks, top artists, then the most interesting insights, one per slide. |
| Wrapped availability | A call to action from some point in December. A buried menu item at any other time. |

## Terms

- **Play**: one `play_history` row. It is one pass of the playback owner over one queue
  entry, or one announced song on a radio stream.
- **Counted play**: a play that reached the count threshold (`count_threshold_frames`: half
  the duration or 240 s). Radio has its own threshold, below. Play counts, top lists, new
  artists, lapsed loves and shallow dives use counted plays. Listened-time rankings (top
  artists and tracks by time) are the exception: they sum the listened time of all timed
  plays that carry the key, counted or not, so a track heard only as skips can still rank.
- **Listened time**: audio of the play that output actually rendered. Paused time is not
  listened time. Seeking forward skips the time jumped over. Seeking back hears it again.
- **End reason**: how a play ended.
  - `finished`: playback reached the natural end.
  - `skipped`: the user moved to another entry (next, previous, selecting another entry,
    removing the current entry).
  - `stopped`: stop, quit, server shutdown, crash recovery, or leaving a radio station.
- **Skip**: a play that ended `skipped` before it counted. A late skip (ended `skipped`
  after counting) is a counted play and not a skip. *(My call: this keeps "always skips the
  last 10 seconds" from looking like a skip.)*
- **Skim**: a play with less than 5 s listened. It is still recorded, and its listened time
  still counts. But it is left out of skip stats entirely, from both the skips and the
  "plays started" denominator. *(My call, from the builder check: holding next to scrub
  through the queue would otherwise flood skip stats.)*
- **Artist key** and **track key**: case-folded, whitespace-trimmed artist name, and
  artist key plus case-folded title. Stats group by these keys, not by library IDs, so radio
  and library plays merge. Display uses the most frequent spelling in the group. *(My call:
  this also merges one song that appears on two albums.)*

## Server: recording

### Store schema 8 (`src/store.rs` migration ladder)

`SCHEMA_VERSION` is 7 today. This adds step 8 to the ladder in `src/store.rs`. (`db.rs`
holds only the library migrations.) Add columns to `play_history`:

| Column | Type | Meaning |
|---|---|---|
| `listened_ms` | INTEGER NULL | Listened time. NULL for rows written before schema 8. |
| `ended_at` | INTEGER NULL | UTC seconds. NULL while the play is still open. |
| `end_reason` | TEXT NULL | `finished`, `skipped` or `stopped`. NULL while open, and for pre-schema-8 rows. |
| `counted` | INTEGER NOT NULL DEFAULT 1 | Whether the play counted. Pre-schema-8 rows are all counted. |
| `raw_title` | TEXT NULL | Radio only: the station's now-playing text before it was split. |

Add an index on `played_at`. Add an index that serves grouping by artist key, if the query
plans need it (the builder decides).

`history_page` filters on `counted = 1`, so the History view is unchanged.

**Two revisions.** *(My call, from the builder check.)* `MAX(id)` no longer works as a change
signal. A row is inserted when it opens and counts later, and `MAX(id)` does not move when
it counts.
- `history_revision` keeps its meaning: it changes when a play counts, because that is when
  History and the `track_stats` play-count columns change. It becomes a counter the owner
  bumps on each count commit, not `MAX(id)`. It must be monotonic across restarts: derive it
  at start from the store, e.g. a stored counter or `COUNT(*) WHERE counted = 1` plus a
  persisted offset. The builder decides.
- A new `stats_revision` in the snapshot changes on every history write: open, count,
  checkpoint and close. Only the Stats page watches it, and it is debounced (see Protocol).
  Library lists and History do not watch it, so 30 s checkpoints never make them refetch.

### Lifecycle of a play

1. **Opens** when output first renders the pass. This is where `Pass::started_at` is set
   today (`hear` in `src/playback/order.rs`). The owner inserts a row with `counted = 0`,
   `listened_ms = 0`, `ended_at = NULL`.
2. **Counts** when today's rules say it counts (`count` in `order.rs`). The row is updated
   to `counted = 1`, and `track_stats` changes in the same commit, as `record_play` does
   today.
3. **Checkpoints**: while open, `listened_ms` is updated at most every 30 s. This bounds
   what a crash can lose.
4. **Closes** with `ended_at`, the final `listened_ms` and an `end_reason`.
5. **Restart and crash recovery** *(my call, from the builder check)*: at server start, the
   open row for the restored selection's entry **stays open**. When playback resumes that
   entry, the resumed pass continues that row: listened time keeps accumulating, and its
   `counted` state comes from the row, not a fresh threshold check. That keeps today's rule
   that a resumed play does not count twice (`restored_selection`), and it means a resume
   neither double counts nor leaves a false skip. If the restored selection changes to
   another entry before it resumes, that row closes as `stopped`. Every other open row
   (`ended_at IS NULL`) closes as `stopped` at start. A closed row keeps its last
   checkpointed `listened_ms`, and `ended_at` is `played_at + listened_ms`.

Pending writes use the same buffer-and-retry model as `PendingPlays`. It becomes a queue of
open, count, checkpoint and close events instead of whole records. Each event addresses its
row by the play's row ID, or by a play key the owner holds until the insert succeeds (the
builder chooses).

Pausing keeps the play open. A play paused for hours and then resumed is still one play.
A play that is paused when the server stops stays open across the restart if it is the
restored selection (step 5). Otherwise it closes as `stopped`.

Measuring listened time: the owner already knows rendered frames per pass (`OutputSnapshot`,
`TrackToken::consumed_by`). Listened time is the frames output rendered for the pass's
token, converted at `SAMPLE_RATE`. Seeks continue the same pass (`continued_pass`). The
builder must make sure a seek neither double counts nor loses frames. Re-selecting the
current entry keeps today's rule (a continued pass).

### Radio

Today a stream entry has an unknown duration, so its pass counts only when the queue ends
there. Radio is almost never recorded. The change:

- A stream entry's pass is split into **radio plays** at each change of the station's
  now-playing text (`update_icy` in `src/stream.rs`, which the owner sees through
  `take_metadata` and `stream_title`).
- When the text changes, the current radio play closes as `finished` and a new one opens.
- The now-playing text is split into artist and title at the first `" - "`. With no
  separator, the whole text is the title and the artist is empty.
- **Junk text** gets an empty artist and title and keeps `raw_title`. Junk means blank
  text, text equal to the station name (`StreamSpec.title`), or text with no letters. These
  plays add listened time to the station only. They never count, and they never appear in
  artist or track stats.
- A radio play **counts after 60 s listened**.
- Leaving the station (selecting another entry, stop, next) closes the current radio play
  as `stopped`, never `skipped`. Radio never adds to skip stats.
- Radio rows have `track_id = NULL`, `source_kind = 'stream'`, `source_name` = the station
  name, and `duration_ms = NULL`. Counted radio plays appear in History like any counted
  play.

## Server: queries

### Where they run

Stats queries open their own read-only SQLite connection to `state.sqlite3`. The database
is in WAL mode (`src/store.rs:313`), and the station directory already searches on its own
connection. Queries run on a blocking task, not on the owner. A stats request must not wait
for the owner, and the owner must not wait for a stats query.

### Time

Buckets use the server machine's local time zone. The server and the client run on the same
Mac. Weeks start on Monday. *(My call: the user writes British English.)* A period is a kind
(`day`, `week`, `month`, `year`, `all`) plus an anchor date (`YYYY-MM-DD`, local). The
server computes the period's UTC bounds and handles DST.

### Protocol (`src/protocol.rs`)

Two new commands. Field names below are indicative, and the builder may refine them:

- `stats { period: {kind, anchor} }` → `stats { stats_revision, report: StatsReport }`
- `wrapped { year }` → `wrapped { stats_revision, report: WrappedReport }`

Also add a third command, `stats_years` → `stats_years { years: [u16] }`. It returns the
years that have at least one counted play, and the Wrapped menu uses it.

The client refetches the Stats page when the snapshot's `stats_revision` changes. Refetches
are debounced to at most one every 10 s while the page is visible. Wrapped is fetched once
when it opens.

Version: follow `CLAUDE.md`. The branch keeps the `VERSION` it starts from (14 at the time of
writing). Bump it in the merge into `dev` to one more than `dev`'s version then, and update
every copy listed there.

### `StatsReport` (one period)

| Field | Definition |
|---|---|
| `plays` | Counted plays in the period. |
| `listened_ms` | Sum of `listened_ms` over plays with known listened time, radio included. |
| `untimed_plays` | Counted plays with NULL `listened_ms` (pre-schema-8; history began 2026-10-18, so few). The client shows a footnote when this is more than 0. |
| `artists`, `tracks` | Distinct artist keys and track keys among counted plays (radio junk excluded). |
| `skip_rate` | Skips ÷ plays started, over non-radio, post-schema-8 plays that are not skims. NULL when there are none. |
| `top_artists` | Top 25 by counted plays and top 25 by listened time: two lists. Each entry has the display name, plays, listened time, and a library artist ID when the key matches exactly one library artist (so the row can link). |
| `top_tracks` | The same shape, by track key, with a library track ID when exactly one matches. |
| `over_time` | Listened time per sub-bucket: All → years, Year → months, Month and Week → days, Day → hours. |
| `heatmap` | 7 × 24 listened time, weekday × local hour, bucketed by `played_at`. *(My call: by time, not by play count, because accurate time is a goal.)* |
| `new_artists` | Artist keys whose first counted play ever falls in the period, ordered by plays in the period, up to 25. |
| `most_skipped` | Tracks with at least 3 non-radio, non-skim plays started in the period, with at least one skip, ordered by skip rate then by skips, up to 10. |
| `stations` | Listened time per station in the period, junk included. |
| `lapsed_loves` | All history, not scoped to the period. See the thresholds below. |
| `shallow_dives` | All history, not scoped to the period. See the thresholds below. |
| `history_span_days` | Days from the first counted play to now. The client uses it for "not enough history yet" states. |

### Thresholds

All thresholds live in one struct, like despotify's `Thresholds`, so they can be tuned in
one place. The history starts young, so the despotify numbers (≥150 plays, 3 years) scale
with how much history exists. S is the span from the first counted play to now.

- **Lapsed loves**: empty until S ≥ 90 days. The gap is G = clamp(S ÷ 4, 30 days, 3 years).
  An artist is lapsed when it has at least `max(10, 150 × min(S, 5 y) ÷ 5 y)` counted plays
  and none in the last G. Order by plays. Up to 25.
- **Shallow dives**: artists with at least `max(20, 150 × min(S, 5 y) ÷ 5 y)` counted plays
  whose top 3 track keys hold at least 60% of them. Order by that share, then by plays. For
  a library artist, include how many library tracks the artist has ("3 of 42 tracks"). Up
  to 25.

### `WrappedReport` (one calendar year, 1 January to now or to the year's end)

The server computes every candidate data point. The client decides which slides to show.
Each data point is optional and absent when its data falls below a minimum.

| Field | Content | Minimum |
|---|---|---|
| `listened_ms`, `plays`, `artists`, `tracks` | Year totals. | — |
| `top_artists` | Top 5 by listened time, each with plays, listened time, and share of the year's time. | 1 artist |
| `top_tracks` | Top 5 by counted plays, each with plays. | 1 track |
| `discovery` | Among artists first heard this year (first counted play ever) with at least 5 counted plays this year, the one with the most listened time this year, with first-play date. | ≥ 5 counted plays |
| `peak_time` | The busiest weekday × 3-hour slot by listened time ("Tuesday evenings"). Ties go to the earlier weekday, then the earlier band. | ≥ 10 h listened in the year |
| `most_skipped` | The track with the most skips this year, with its skip count. | ≥ 5 skips |
| `comeback` | A lapsed love that returned: an artist with at least the lapsed-love play floor `max(10, 150 × min(S, 5 y) ÷ 5 y)` of counted plays before the year and no plays for G before its first play this year, with the most plays this year among such artists. S and G are measured to the report's `through`. None while S < 90 days. | ≥ 10 counted plays this year |
| `top_station` | The station with the most listened time. | ≥ 1 h |
| `months` | For each month with a counted play (sparse, not zero-filled), its listened time and top artist. | ≥ 3 months with plays |

If the year has fewer than 50 counted plays, the client shows one slide, "Not enough
listening yet for a Wrapped", and an exit.

## Client: Stats page

### Placement

- A new `Destination.stats` ("Stats", SF Symbol `chart.bar`), in the `playing` group after
  History: `[.upNext, .history, .stats, .radio]`. *(My call.)*
- It is one scrolling page, not a track list. It has its own view, like `RadioView`, not
  `VirtualTrackTable`.

### Layout (top to bottom, from mockup A)

1. **Header bar**: ‹, period segmented control (Day / Week / Month / Year / All), ›, the
   period's label ("13–19 Oct 2026"), and the Wrapped call to action when it applies.
   › is disabled on the current period. All has no stepping. The default is Week.
   *(My call.)* The last chosen kind is remembered in preferences.
2. **Tiles**: plays, listened time, artists, skip rate. When `untimed_plays` > 0, the
   listened-time tile has a footnote: "N earlier plays have no listening time".
3. **Top artists** and **Top tracks**, side by side, each with a toggle between plays and
   time. Each row shows a horizontal bar. Rows with a library ID open that artist or track in
   the library (double-click), like other lists.
4. **Listening over time**: a bar chart of `over_time`. Clicking a bar narrows the period
   to that bucket (a year bar sets Year, a month bar sets Month, a day bar sets Day).
5. **When you listen**: the 7 × 24 heatmap, Monday first.
6. A row of three panels: **New artists**, **Most skipped**, **Stations**.
7. A row of two panels, labelled "Across all your history": **Lapsed loves**, **Shallow
   dives**. Each has a "Not enough history yet" state that says when it will start filling
   in (from `history_span_days`).

Empty periods show the panels with an empty state, not a blank page. The deployment target
is macOS 14, so Swift Charts is available. Use it where it fits. Colours follow the app's existing palette work
(`2026-10-21-colours-and-cover-tint-design.md`).

### Wrapped call to action

From 1 December to 31 December, the header shows a prominent "Your 2026 Wrapped ▶" button.
It also shows a dismissible banner the first time the page opens in that window. The
banner's dismissal is remembered per year. *(My call: the dates. The user said "some point
in December".)*

### Wrapped menu item

`Go › Wrapped` is a submenu with "2026 so far" (the current year) and each past year from
`stats_years`. It is disabled when no year has plays. *(My call: the Go menu. The user asked
for "a buried menu item".)*

## Client: Wrapped slideshow

- It covers the whole main window's content, sidebar and player bar included, as an overlay
  in the same window. It does not enter macOS full screen and does not open a new window.
- Story-style progress bars along the top, one per slide.
- Each slide auto-advances after 6 s. Click or → goes forward, ← goes back, space
  pauses and resumes, esc closes. The last slide does not auto-close.
- Bold gradient backgrounds, a different one per slide. Big type. Elements animate in,
  staggered: label, then headline, then detail. Ranked lists reveal row by row.
- Reduce Motion (accessibility setting) replaces movement with fades. Auto-advance stays.
- Playback continues underneath, unchanged.

Slide order. A slide whose data point is absent is left out.

1. **Your year**: total listened hours and plays.
2. **Top artist**: name, hours, plays, share of the year.
3. **Top 5 artists**.
4. **Top track**: title, artist, plays.
5. **Top 5 tracks**.
6. **Biggest discovery**: the artist, and when you first heard them.
7. **When you listen**: "You listen most on Tuesday evenings".
8. **Your year in months**: each month's top artist.
9. **Most skipped**: a light-hearted line with the count.
10. **A lapsed love returns**: the comeback artist.
11. **Top station**.
12. **Summary card**: top 5 artists, top 5 tracks and total hours on one card.

## Testing

TDD throughout, using the existing layout: inline `mod tests` and sibling `*_tests.rs` under
`src/store/`, integration tests in `tests/`, Swift tests in
`macos/Tests/TyrannyLikesSmokeCoreTests/`, and a native smoke for the new views.

At minimum:

- Migration: schema 7 rows survive as counted, with NULL listened time and end reason.
- Lifecycle: a skipped-early play, a late skip, a natural finish, a stop while paused, a seek
  back and forward (listened time correct), crash recovery of an open row, a restart that
  resumes the restored entry (one row, counted once), and a skim (excluded from skip stats).
- Revisions: `history_revision` moves on count only; `stats_revision` moves on every write.
- `track_stats` and History are unchanged by uncounted plays.
- Radio: a text change splits plays, junk text never counts, the 60 s threshold applies, and
  leaving a station is `stopped`.
- Each `StatsReport` and `WrappedReport` field over a fixed fixture history, including DST
  boundaries and Monday-first weeks.
- Threshold scaling at S = 30 days, 1 year and 5 years.
- Stats queries do not block the owner: a slow query and a transport command at the same
  time.
- Protocol serde round-trips for the new commands, and the Swift wire models match
  (`tests/swift_constants.rs`).
- Swift: period stepping and labels, Wrapped slide selection from a report with absent
  fields, the December call-to-action window, and keyboard control of the slideshow.

## Left to the builder

- Exact protocol field names and the Rust and Swift type layout.
- How pending play events address their row before the insert succeeds.
- How `history_revision` stays monotonic across restarts.
- Indexes beyond `played_at`, from query plans.
- Gradient palette and animation timing details within the description above.
- Whether `tyranny-likes-smoke-ctl` help lists the new commands. It should, if it lists the others.
