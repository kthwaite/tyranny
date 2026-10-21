# Retrieve Tags from MusicBrainz and Write Them to Files

Date: 2026-10-21. Scope: Rust server (`src/`), protocol, macOS client (`macos/`). Task:
TSK-swift-yarn-vxap5.

REQUIREMENTS.md, Interface: "Automatically retrieve tags from MusicBrainz". The user decided:

- corrections are **written into the audio files** (not kept as database overrides);
- matching uses **the MusicBrainz IDs already in the files, then a text search; AcoustID
  fingerprints are optional**;
- a lookup starts **on demand, for one album**, and the user reviews the changes before anything
  is written.

Every other decision below is Claude's call and is marked "(Claude)".

## Intent

A user with badly tagged albums picks one album, asks MusicBrainz what it really is, sees exactly
what would change in each file, and applies it. The files then carry clean tags and MusicBrainz
IDs that every other player can read, and the library shows the new tags at once.

## Goals

1. Look up one album on MusicBrainz and return a reviewable proposal: candidate releases, and
   per track the fields that would change (old → new).
2. Write the accepted changes into the files safely, then re-index those files.
3. Keep a journal of every write so any edit can be reverted.
4. Optional AcoustID fingerprint lookup when the user has entered an AcoustID API key.
5. Client: an album action that opens a review sheet; an AcoustID key field in Settings; a list
   of recent tag edits with Revert.

## Non-goals

- No background or whole-library lookups.
- No genre, label, catalogue number, ISRC, composer, sort tags, or cover art writing (Claude).
  Cover art already comes from the Cover Art Archive by release ID.
- No AcoustID submission.
- No tag editing by hand (only MusicBrainz values).
- No writing for formats lofty cannot write (below).

## Anti-goals

- **Never damage a file.** A failed or interrupted write must leave the original file intact.
- Never write a field the user did not see in the review.
- Never break MusicBrainz or AcoustID usage rules: at most 1 request per second to
  `musicbrainz.org`, at most 3 per second to `api.acoustid.org`, and a meaningful User-Agent.
- Never block playback, the library worker, or other requests while a lookup runs.

## Server

### Module and threads (Claude)

A new module `src/tagging/`, shaped like `src/lyrics/` and `src/artwork/`:

- one worker OS thread `tagging-0` behind a `sync_channel`, with an async `Handle::request` that
  awaits a tokio `oneshot`;
- a read-only SQLite connection to the state DB for album and track rows;
- its own SQLite file `<db>.tagging.sqlite` for the edit journal. This is not a cache. Only the
  latest 200 edits are kept.
- `server.rs` treats lookups as "discovery" commands: if the client disconnects while waiting, the
  worker is told to stop, as lyrics does today.

### Providers (Claude)

`reqwest::blocking` clients in the same style as `src/lyrics/provider.rs`:

- `https_only`, redirects disabled, 6 s request timeout, 3 s connect timeout, 2 MiB response cap.
- User-Agent `Tyranny/<version> (https://github.com/kthwaite/tyranny)` (the lyrics UA).
- MusicBrainz origin `https://musicbrainz.org/ws/2/`, `fmt=json`. A start-spacing gate gives at
  most 1 request per second, process-wide. A `503` or `Retry-After` stops the lookup with a
  "MusicBrainz is busy, try again shortly" error. No retries.
- AcoustID origin `https://api.acoustid.org/v2/`, at most 3 requests per second.
- A `#[cfg(test)] for_test(origin)` constructor that accepts only a loopback origin, as the
  existing providers do. Tests use a hand-rolled `TcpListener` fixture.

### Lookup

Input: an album ID, the library revision it was read at, optionally a release ID chosen by the
user, and optionally an AcoustID key with `fingerprint: true`.

1. Read the album's tracks from the DB: track ID, path, duration, disc and track number, and the
   stored MusicBrainz IDs. These are used only to find and match candidates. The **current
   values** shown in the review and stored in the journal come from reading the file itself with
   lofty (Claude). The DB values are normalised for display ("Unknown Album", a title from the file
   name, year only) and cannot tell an absent tag from a placeholder.
2. Find candidate releases:
   - **Explicit:** a release ID given in the request is the only candidate.
   - **By ID:** if the tracks carry a release ID (most common value), fetch it.
   - **By search** otherwise: `release` search with album title, album artist and track count
     (Lucene query; escape special characters). Keep up to 5 candidates.
   - **By fingerprint**, only when `fingerprint: true` and a key is present: fingerprint every
     track (first 120 s, decoded with `Decoder` and fed to `rusty-chromaprint` with
     `Configuration::preset_test2()`, compressed with `FingerprintCompressor` and URL-safe base64),
     query AcoustID `lookup` with `meta=recordingids+releaseids`, and rank releases by how many
     tracks voted for them. Keep up to 5.
3. Score candidates (Claude): track count equal to the album's (strong), total duration within
   2 %, title similarity, then MusicBrainz's own search score. The best candidate is chosen
   unless the request named one.
4. Fetch the chosen release:
   `release/<id>?inc=recordings+artist-credits+release-groups+media`.
5. Match files to release tracks (Claude), in order:
   - the file's release track ID or recording ID;
   - its disc and track number, when the release has that position;
   - duration within 3 s, with the closest title.
   Each release track is matched at most once. A file with no match gets no proposal and
   says so.
6. Build the proposed values for each matched file, and keep only fields whose value differs from
   the file's current value.

Fields (Claude). The library reader (`src/metadata.rs`) shows all of these except the full date
and original date, which it reads as a year only. `TrackMetadata` and `METADATA_VERSION` stay
unchanged (Claude): extending them would force every user's whole library to be re-probed. Dates
are compared as full strings, as lofty reads them from the file.

| Field | Value from MusicBrainz |
|---|---|
| title | track title |
| artist | track artist credit, names joined with their join phrases |
| album | release title |
| album artist | release artist credit, joined the same way |
| track number / track total | position / track count of the medium |
| disc number / disc total | medium position / medium count |
| date | release date as given (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`) |
| original date | release group first-release date |
| MB recording ID | recording ID |
| MB release track ID | track ID |
| MB release ID | release ID |
| MB release group ID | release group ID |
| MB artist ID | first credited artist of the track |
| MB album artist ID | first credited artist of the release |

The proposal is stored in the worker's memory under a random `proposal_id` for 15 minutes. It
holds the exact values and each file's size and modification time at lookup. The reply
carries the candidates, the chosen release, and per track: track ID, file name, `writable` with a
reason when false, and the field changes (field, old, new).

The proposal is kept server-side so that the apply request stays small (the request frame limit
is 16 KiB) and the server writes exactly the values it showed.

### Apply

Input: `proposal_id`, the track IDs to write, and optionally field names to leave out. The
server writes the proposal's values only. It never takes values from the client.

For each file:

1. Skip it with "changed since the lookup" if its size or modification time differ from the
   proposal's.
2. Clone it with `std::fs::copy` to a temporary file in the same directory (`.<name>.tyranny-tmp`).
   On APFS this is a cheap clone, and on macOS it keeps permissions and extended attributes.
3. Write the changed fields with lofty into the file's primary tag type, creating it if missing.
   Keep every other tag item and picture. The library reader uses FFmpeg, which merges every tag
   in the file and accepts alias keys, so an old copy of a written field could still win. So,
   for each written field only (Claude):
   - remove its alias keys in the primary tag (for example `TOTALTRACKS` when writing
     `TRACKTOTAL`, `MUSICBRAINZ_RELEASEID` beside `MUSICBRAINZ_ALBUMID`, `ORIGINALYEAR` beside
     the original date, an ID3 `UFID` beside the `TXXX` recording ID);
   - remove that field from the file's other tags (ID3v1 or APEv2 on MP3, ID3 in WAV).
     lofty can't edit an ID3 tag inside FLAC, APE or Musepack, only strip it whole. A file
     whose such tag holds a field the proposal changes is not writable: "Has an old ID3 tag
     Tyranny can't edit" (amended 2026-10-21, plan preflight).

   Other fields keep their values. lofty may normalise how untouched items are spelled (for
   example `TOTALTRACKS` → `TRACKTOTAL`, ID3v2.4 frame forms), which is accepted (amended
   2026-10-21). The review shows "Also removes old copies of these fields from
   other tags" on files where this applies. The journal records the removed values too, so revert
   restores them.
4. Flush, `fsync`, check the original is still unchanged, then `rename` the clone over the
   original. A playing file stays playable, because the decoder keeps the old inode open.
5. On any error, delete the temporary file and report the error for that file. The original is
   untouched.

Before the first write, append a journal edit: `edit_id`, time, release ID and title, and per file
the path plus the before and after values of the written fields. After the writes, record which
files succeeded.

Finally, ask the library to re-index the written paths (below). The reply lists per track
`written`, `skipped` (reason), or `failed` (message), plus the `edit_id`.

### Writable formats

lofty 0.25 writes AAC (ADTS), AIFF, APE, FLAC, MP3/MP2, MP4 (`m4a`, `m4b`, `mp4`, `alac` when it
is MP4), Musepack, Opus, Ogg Vorbis, Speex, WAV and WavPack. A file is writable only if lofty
detects one of these types from its content. CAF, WMA, Matroska/WebM, MOV, DSF/DFF, TTA, TAK,
Shorten, AC3, DTS, AMR, AU, RealAudio and Ogg FLAC are not writable. They appear in the review,
disabled, as "Tyranny can't write tags to this format".

The fixture round-trip test (Verification) decides the final list. The deny list is per format
and field (Claude). If a field fails its round-trip in one format, that field is left out for that
format and the review shows "can't write <field> to this format". A format with no field that
round-trips is not writable.

### Revert

`revert_tag_edit {edit_id}` writes each file's "before" values back, with the same safe-write
steps. A field that was absent before is removed. It skips any file whose current values
for those fields no longer equal the "after" values ("changed since"). A reverted edit is marked
as such and can't be reverted twice. `tag_edits` lists the latest 50 edits: ID, time,
release title, file count, and reverted flag.

### Re-index

A new `LibraryCommand::Reindex { paths }` re-probes exactly those assets, without enumerating the
root (Claude). Album grouping (`src/library/grouping.rs`) only runs as part of a root scan's
finish step, so Reindex reuses that existing finish path (grouping pass and revision bump) for the
affected roots. It does not write new folder-limited grouping. Its cost grows with the root's
size, which is acceptable for an on-demand action: one root's grouping pass is what every scan
already pays. If a scan is running, Reindex queues the paths and probes them when the scan
finishes. Retagged files keep their track ID (identity is the asset path), so play
stats and queues survive. Their album ID can change. For example, adding a release ID moves the
album from folder identity to MusicBrainz identity.

### Protocol

Bump `VERSION` 12 → 13, here and in `WireModels.swift`. New commands, all request/response:

- `musicbrainz_lookup { album_id, library_revision, release_id?, fingerprint?, acoustid_key? }`
  returns kind `tag_proposal`.
- `apply_tags { proposal_id, track_ids, omit_fields? }` returns kind `tag_apply`.
- `tag_edits {}` returns kind `tag_edits`.
- `revert_tag_edit { edit_id }` returns kind `tag_apply` (same per-file result shape).

The client stores the AcoustID key and sends it with each fingerprint lookup (Claude). The server
does not persist it.

Errors use existing codes. A stale library revision is `StaleLibraryRevision`. An unknown or
expired proposal is `InvalidCommand` with "lookup expired, look the album up again". Network,
MusicBrainz and AcoustID failures use a new `ErrorCode::Lookup` with a readable message (Claude),
mirrored in Swift.

## Client

- **Entry points:** "Look Up on MusicBrainz…" in the album context menus (album grid item, album
  block header in grouped lists), and a command-language command `:musicbrainz` for the selected
  album.
- **Review sheet** (`TagLookupSheet`):
  - While looking up: "Asking MusicBrainz…" with a progress spinner. The request deadline is
    60 s, not the default 10 s.
  - A release picker. Each candidate reads "Title — Artist · year · country · N tracks".
    Picking one re-runs the lookup with that `release_id`.
  - A table with one row per file: a checkbox (on by default for writable files with
    changes), track number, file name, and the changes as "field: old → new" lines.
    Rows that can't be written are disabled and show their reason. Files with nothing to change
    say "Already matches".
  - Buttons: "Identify by Audio Fingerprint" (only when an AcoustID key is set), Cancel, and
    "Write Tags to N Files". After applying, the sheet shows the per-file result and an Undo
    button (revert this edit).
  - Errors are shown in the sheet: no match, MusicBrainz busy, network, expired lookup.
- **Settings → Library:** an "AcoustID API key" field, with the text "Get a key at
  acoustid.org/new-application" shown as selectable text. Below it, "Recent Tag Edits": up to
  50 rows (date, release, file count), each with a Revert button.
- **TyrannyCore holds the pure parts, with tests:** wire models for the four replies, the row
  selection state and the apply request it builds, the candidate label, and the Settings
  edit row text.

## Verification

- **Rust unit tests with the loopback fixture:**
  - lookup by ID, by search and by fingerprint, with hand-written MusicBrainz and AcoustID JSON that follows the real ws/2 and v2 schemas in
    `tests/fixtures/musicbrainz/`;
  - candidate scoring;
  - file-to-track matching: by ID, by position, by duration;
  - only changed fields proposed;
  - rate gate spacing ≥ 1 s;
  - `503` and `Retry-After` handling;
  - proposal expiry;
  - apply rejects a file changed since the lookup;
  - apply never uses client values.
- **Write round-trip, for every writable fixture in `tests/fixtures/formats`:**
  - copy it, apply a proposal of every field, and read it back. With lofty every field reads
    back equal. With `metadata::probe` every field the library reads is equal (date and original
    date at year precision);
  - a fixture with a stale alias or secondary tag (for example an MP3 with ID3v1 and APEv2) reads
    back the new value through `metadata::probe`;
  - the audio still decodes to the manifest's frame count;
  - existing tags not in the proposal survive.
  - Unwritable fixtures are reported as not writable.
- **Safety tests:**
  - a write failing after the clone leaves the original byte-identical, with no temp file left;
  - a file open in a `Decoder` during apply keeps decoding;
  - revert restores the before values and removes added fields;
  - a double revert is refused.
- **Re-index test:** after apply, `Reindex` updates the track's metadata and bumps the revision,
  and the track ID is unchanged.
- **Swift:** TyrannyCore tests for the models and selection; debug and release builds with
  `-warnings-as-errors`; `swift test`; `client_gate.py --model-only`.
- **Standard gates:** `cargo fmt --check`, clippy `-D warnings`, `cargo test`, and the format tests
  against the release FFmpeg prefix.
- **Not testable here:** a live lookup against musicbrainz.org (run once by hand if network allows,
  otherwise it is the user's), and the native sheet smoke (needs Accessibility).

## Dependencies

- `lofty = "0.25.4"` (2026-09-20, MIT/Apache-2.0, rust-version 1.89; rustc here is 1.99).
- `rusty-chromaprint = "0.3.0"` (MIT, pure Rust, brings `rubato`).
- `base64` and `serde_json` are already present.

## Left to the builder

The module's internal layout, the Lucene escaping details, the exact similarity measure (any
normalised edit distance), the journal schema, and the sheet's layout within the description
above.
