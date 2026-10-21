# MusicBrainz Tag Lookup and File Writing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Look up one album on MusicBrainz, show every per-file tag change for review, write the accepted changes into the audio files safely, journal them for revert, and re-index the written files.

**Architecture:** A new server module `src/tagging/` runs one worker thread (`tagging-0`), shaped like `src/lyrics/`. It reads album rows from the state DB, reads and writes tags with lofty, asks MusicBrainz (and optionally AcoustID) through rate-gated blocking HTTP clients, keeps proposals in memory, and journals every write in `state.tagging.sqlite`. Writes go to an APFS clone that is renamed over the original. The library gets a path-scoped `Reindex` that reuses the scan machinery. Protocol 13 adds four commands. The macOS client adds a review sheet, album menu items, a `:musicbrainz` command and a Settings section; TyrannyCore holds the pure parts.

**Tech Stack:** Rust 2024 (lofty 0.25.4, rusty-chromaprint 0.3.0, reqwest blocking, rusqlite, serde, tokio), FFmpeg via ffmpeg-next (reading only), SwiftUI/AppKit + TyrannyCore (swift-testing).

**Spec:** `docs/superpowers/specs/2026-10-21-musicbrainz-tags-design.md` (binding; read it with this plan).

**Spec amendments** (accepted at preflight; the controller amends the spec separately):

- **ID3 in FLAC.** The spec says to remove a written field from "ID3 in FLAC". lofty 0.25.4 can only strip ID3v2 in FLAC, APE and Musepack (and ID3v1 in Musepack) whole. So a file whose read-only tag holds a changed field is not writable (decision 3).
- **Normalisation.** lofty's normalisation of untouched items is accepted (decision 5), although the spec says "Other fields are never touched". Their values must survive.
- **Fixtures.** The MusicBrainz and AcoustID fixtures are hand-written to the documented response shapes. They are not recorded from live traffic, although the spec's Verification says "recorded".

**Decisions this plan adds where the spec leaves room** (a reviewer may reject any of them):

1. **Current value of a field** = the value at the field's primary key in the primary tag. When that is absent, the first alias or other-tag copy (lofty tag order) is used. The journal "before" is the primary value only. The copies that will be removed are journalled separately.
2. **ID3v2 recording ID** lives in `TXXX:MusicBrainz Track Id`, because FFmpeg reads it there. `UFID:http://musicbrainz.org` is treated as its alias, as the spec's alias example implies. lofty already maps `ItemKey::MusicBrainzRecordingId` to that UFID frame, so the alias is the field's `CopyKey::Field` copy, not a raw alias.
3. **Read-only secondary tags.** lofty can only strip ID3v2 in FLAC, APE and Musepack whole. If such a tag holds a field the proposal changes, the file is not writable. Its reason reads "Has an old ID3 tag Tyranny can't edit".
4. **MOV** is MP4 to lofty. A file whose `ftyp` major brand is `qt  ` is not writable.
5. **lofty's own normalisation of untouched items is accepted.** Examples: canonical key spelling such as `TOTALTRACKS` → `TRACKTOTAL`, and ID3v2.4 output. Their values must survive. ID3v2.3 files stay v2.3.
6. **Candidate source precedence:** explicit `release_id`, then fingerprint (when requested), then the stored release ID, then search.
7. **Search.** The first query is title + album artist + `tracks:N`. When it returns nothing, a second query drops the track count. Every candidate (≤ 5) is fetched in full, so duration can be scored. Fetched releases are cached in memory for 15 minutes.
8. **MusicBrainz values that are absent or empty are never proposed.** A lookup never blanks a tag.
9. **Number fields** (track/disc number/total) are compared numerically. Every other field is compared as an exact string.
10. **Apply consumes the proposal.** A track ID that is not in the proposal rejects the whole request before any write.
11. **Revert** creates no new journal edit. Files left `pending` by a crash are revertible when their values still equal "after".
12. **Client connections.** The lookup uses its own 60 s connection and is cancelled by disconnecting it. Apply, revert and the edit list use a second 60 s connection, which is never cancelled mid-write.
13. **AcoustID key storage.** The client keeps the key in `UserDefaults` under `acoustidAPIKey`.
14. **`Reindex`** is internal. Its reply never reaches the wire.

## Global Constraints

- Work only in the worktree `/Users/kit/Source/_p.music/tyranny-musicbrainz`, on branch `feature/musicbrainz-tags` (cut from `dev`). It merges back into `dev` after the final review. Never commit to `prod`.
- Dependencies, added to `Cargo.toml` exactly as written: `lofty = "0.25.4"` and `rusty-chromaprint = "0.3.0"`. `base64` and `serde_json` are already present. Add no other crates.
- Protocol version goes 12 → 13 in Task 10, in the same commit as every hard-coded 12:
  - `src/protocol.rs:1` (the module comment "Version 12 local control protocol"), `src/protocol.rs:16`, and the `assert_eq!(VERSION, 12)` in `v9_list_commands_require_every_field`;
  - `macos/Sources/TyrannyCore/WireModels.swift:6` (`tests/swift_constants.rs` checks it);
  - `scripts/playlist_gate.py:59,64`, `scripts/server_gate.py:26` and `scripts/smart_playlist_gate.py:63`;
  - `SPIKE.md:9,89`;
  - the comment in `macos/Sources/TyrannyCore/StationCatalog.swift:4`, left as "(protocol v12)" because stations arrived in v12. `src/bin/tyrannyctl.rs:46` "(protocol 12)" is historical in the same way and also stays.
  - Find them with `grep -rn "v12\|Version 12\|version=12\|VERSION: u32 = 12\|version: UInt32 = 12" src macos/Sources scripts SPIKE.md`.
- Wire names (snake_case JSON):
  - commands: `musicbrainz_lookup`, `apply_tags`, `tag_edits`, `revert_tag_edit`;
  - reply kinds: `tag_proposal`, `tag_apply`, `tag_edits`;
  - error code: `lookup`;
  - field names: the `TagField` table in Task 1.
- Limits:
  - `MAX_ALBUM_TRACKS = 500`.
  - Proposals live 15 min (`PROPOSAL_TTL`); at most 8 are held (`MAX_PROPOSALS`), and the oldest is evicted first.
  - Release cache: 32 releases, 15 min.
  - Journal keeps 200 edits; `tag_edits` lists 50.
  - Server lookup deadline is 50 s; the client request deadline is 60 s.
  - Fingerprints use the first 120 s.
  - At most 5 candidates.
- HTTP:
  - `https_only`, redirects disabled, 6 s request timeout, 3 s connect timeout, 2 MiB response cap.
  - User-Agent `Tyranny/<CARGO_PKG_VERSION> (https://github.com/kthwaite/tyranny)`.
  - Origins `https://musicbrainz.org/` and `https://api.acoustid.org/`.
  - Request starts are spaced by at least 1 s for MusicBrainz and 334 ms for AcoustID.
  - A `503`, a `429`, or any `Retry-After` header stops the lookup. There are no retries.
- Tests never touch the network. Providers are built in tests only by `for_test(origin, interval)`, which accepts only a loopback `http://` origin with path `/`.
- Exact user-visible copy (the client shows server reasons verbatim):
  - "Asking MusicBrainz…" and "Look Up on MusicBrainz…";
  - "Tyranny can't write tags to this format" and "can't write <Field Label> to this format";
  - "Also removes old copies of these fields from other tags";
  - "Already matches" and "No matching track on this release";
  - "Has an old ID3 tag Tyranny can't edit";
  - "MusicBrainz is busy, try again shortly" and "MusicBrainz has no matching release";
  - "lookup expired, look the album up again" and "changed since the lookup";
  - "Write Tags to N Files" (singular "Write Tags to 1 File") and "Identify by Audio Fingerprint";
  - "AcoustID API key", "Get a key at acoustid.org/new-application" and "Recent Tag Edits".
- Until Task 10 routes the module, `src/tagging/mod.rs` starts with an unconditional `#![allow(dead_code)]`. Without it, clippy `-D warnings` fails in two cases: crate-private items that only tests use, and items that nothing uses until Task 10 (for example `MusicBrainz::new`, `AcoustId::new`, `TaggingWorkers::start` and the `Handle.library` field). The second case also fails the `cfg(test)` build. Task 10 removes the attribute and fixes any dead code left.
- `cargo test` accepts one `[TESTNAME]` filter. To run several filters, pass them after `--`, for example `cargo test --locked --lib -- tagging::acoustid tagging::fingerprint`.
- Commits: one plain imperative sentence, no prefix, no attribution or Co-Authored-By lines.
- **Gates.** Every task ends with all of these green. Run them from the worktree root.

  ```bash
  cargo fmt --check
  cargo clippy --all-targets --locked -- -D warnings
  cargo test --locked --all-targets
  (cd macos && swift build -Xswiftc -warnings-as-errors)
  (cd macos && swift build -c release -Xswiftc -warnings-as-errors)
  (cd macos && swift test)
  cargo build --locked --bins && scripts/build_macos.sh debug   # builds what the next line launches
  python3 scripts/client_gate.py --model-only
  # Format and round-trip tests against the pinned release FFmpeg. Do NOT run
  # scripts/build_ffmpeg.py here: its cache key includes the install root, so in this
  # worktree it would rebuild FFmpeg. Use the cloned prefix (newest of the two):
  FFMPEG_PREFIX="$PWD/target/native/ffmpeg-9.0.2-arm64-34cf41683b39bf25a0658e7a"
  PKG_CONFIG_LIBDIR="$FFMPEG_PREFIX/lib/pkgconfig" CARGO_TARGET_DIR=target/release-ffmpeg cargo test --test ffmpeg_formats
  ```

## Review Focus

1. **A stale `.<name>.tyranny-tmp` left by a crash, or a Unicode or space-filled file name.** The next write replaces the stale temporary file and succeeds. It never treats the temporary as the user's file. (Task 2: `stale_temp_file_from_a_crash_is_replaced`, `unicode_file_names_write`.)
2. **Permissions.** A file's mode (for example 0640) survives the write. A directory without write permission fails every file cleanly: the originals stay byte-identical and no temporary is left. (Task 2: `permissions_survive_and_unwritable_directory_fails_cleanly`; Task 8: `unwritable_directory_fails_every_file_and_leaves_originals`.)
3. **A file deleted, replaced or moved between lookup and apply.** It is skipped with "changed since the lookup". It is not "failed", and nothing panics. (Task 2: `deleted_file_is_changed`; Task 8: `deleted_file_between_lookup_and_apply_is_skipped`.)
4. **Sparse MusicBrainz data.** Cases: `date: ""`, a release group without a first-release date, `length: null`, and files without disc numbers. None of these blanks a tag or panics. Missing disc numbers count as disc 1. (Task 6: `absent_or_empty_musicbrainz_values_are_not_proposed`, `missing_disc_number_is_disc_one_and_null_lengths_are_skipped`.)
5. **A 500-track album.** Its apply request with 500 maximum-width track IDs fits the 16 KiB request frame. (Task 11: `applyRequestForFiveHundredTracksFitsOneFrame`.)

---

## File Structure

Server (`src/tagging/`, declared `pub mod tagging;` in `src/lib.rs`; only `tags` is `pub` inside it, for `tests/ffmpeg_formats.rs`):

| File | Responsibility |
|---|---|
| `tags.rs` (pub) | `TagField` ↔ lofty mapping, alias table, `read_tags`, `edit_tags` in place, writable-format detection, per-format deny list |
| `safe_write.rs` | `FileStamp`, clone → edit → fsync → verify → rename |
| `journal.rs` | `state.tagging.sqlite`: edits, per-file before/after/removed, status |
| `mod.rs` | `Cancellation`, `LookupError`, `Failure`, requests/outcomes, `TaggingWorkers`, `Handle` |
| `http.rs` | `StartGate`, bounded JSON GET, status mapping, shared client builder |
| `musicbrainz.rs` | MusicBrainz provider, ws/2 models, Lucene query |
| `acoustid.rs` | AcoustID provider, release vote ranking |
| `fingerprint.rs` | Decoder → chromaprint → URL-safe base64 |
| `matching.rs` | candidate score, file ↔ release-track matching, proposed values, changes |
| `proposal.rs` | in-memory proposals with expiry; release cache |
| `engine.rs` | lookup, apply, revert, edits (single-threaded, owns DB + journal) |
| `test_support.rs` (cfg(test)) | loopback HTTP fixture, fixture JSON loader, tone WAV writer, library fixture |
| `tests.rs` (cfg(test)) | engine and worker tests |

Also touched: `src/cancel.rs` (new, shared `Cancellation`), `src/lyrics/mod.rs`, `src/protocol.rs`, `src/owner.rs`, `src/server.rs`, `src/library/{mod.rs,db.rs}`, `src/library_types.rs`, `src/scanner.rs`, `tests/ffmpeg_formats.rs`, `tests/server_transport.rs`, `tests/fixtures/musicbrainz/*.json`, `tests/fixtures/tag_roundtrip_values.json`.

Client: `macos/Sources/TyrannyCore/Tagging.swift` + `macos/Tests/TyrannyCoreTests/TaggingTests.swift`; `macos/Sources/Tyranny/Models/TaggingModel.swift`; `macos/Sources/Tyranny/Views/TagLookupSheet.swift`; edits to `WireClient.swift`, `PlayerModel.swift`, `Shell/MainWindowView.swift`, `Shell/SettingsView.swift`, `Albums/AlbumGridItem.swift`, `Albums/AlbumGridView.swift`, `Lists/TrackTableInput.swift`, `Lists/VirtualTrackTable.swift`, `TyrannyCore/CommandLanguage.swift`, `CommandDispatcher.swift`.

---

### Task 1: Tag fields in files with lofty, and the format round-trip

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock` (via `cargo add`)
- Modify: `src/lib.rs` (`pub mod tagging;`)
- Create: `src/tagging/mod.rs` (for now only `#![allow(dead_code)]`, from the Global Constraints, and `pub mod tags;`), `src/tagging/tags.rs`
- Create: `tests/fixtures/tag_roundtrip_values.json` (the round-trip values below)
- Modify: `src/protocol.rs` (add `TagField` only)
- Modify: `tests/ffmpeg_formats.rs` (round-trip tests)

**Interfaces:**
- Produces, in `src/protocol.rs`:
  ```rust
  #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
  #[serde(rename_all = "snake_case")]
  pub enum TagField { Title, Artist, Album, AlbumArtist, TrackNumber, TrackTotal, DiscNumber, DiscTotal,
      Date, OriginalDate, MusicbrainzRecordingId, MusicbrainzTrackId, MusicbrainzReleaseId,
      MusicbrainzReleaseGroupId, MusicbrainzArtistId, MusicbrainzAlbumArtistId }
  impl TagField { pub const ALL: [TagField; 16]; pub fn is_number(self) -> bool }
  ```
  The field's lofty `ItemKey`, in declaration order: `TrackTitle, TrackArtist, AlbumTitle, AlbumArtist, TrackNumber, TrackTotal, DiscNumber, DiscTotal, RecordingDate, OriginalReleaseDate, MusicBrainzRecordingId, MusicBrainzTrackId, MusicBrainzReleaseId, MusicBrainzReleaseGroupId, MusicBrainzArtistId, MusicBrainzReleaseArtistId`.
- Produces, in `src/tagging/tags.rs` (all `pub`):
  ```rust
  pub enum WritableFormat { Aac, Aiff, Ape, Flac, Mpeg, Mp4, Mpc, Opus, Vorbis, Speex, Wav, WavPack }
  pub fn writable_format(path: &Path) -> anyhow::Result<Option<WritableFormat>>
  pub fn unwritable_fields(format: WritableFormat) -> &'static [TagField]
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub enum TagKind { Ape, Id3v1, Id3v2, Mp4Ilst, VorbisComments, RiffInfo, AiffText } // mirrors lofty TagType
  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub enum CopyKey { Field, Year, Raw(String) } // the field's ItemKey; ItemKey::Year (a Date alias); a raw key
  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub struct TagCopy { pub tag: TagKind, pub field: TagField, pub key: CopyKey, pub value: String, pub read_only: bool }
  pub struct FileTags { pub format: WritableFormat, pub primary_kind: TagKind, pub id3v23: bool,
      pub primary: BTreeMap<TagField, String>, pub copies: Vec<TagCopy> }
  /// `unwritable_fields(format)` plus `ID3V23_UNWRITABLE` when `id3v23`. Lookups use this one.
  pub fn unwritable_fields_in(tags: &FileTags) -> Vec<TagField>
  impl FileTags {
      pub fn current(&self, field: TagField) -> Option<&str>          // decision 1
      pub fn copies_of(&self, fields: &BTreeSet<TagField>) -> Vec<TagCopy>
  }
  pub struct TagEdit { pub set: BTreeMap<TagField, Option<String>>, pub restore: Vec<TagCopy> }
  pub fn read_tags(path: &Path) -> anyhow::Result<FileTags>             // errors for unwritable formats
  pub fn edit_tags(path: &Path, edit: &TagEdit) -> anyhow::Result<Vec<TagCopy>> // in place; returns removed copies
  ```
- Raw aliases. This table is the data the spec pins; copy it as written. Raw key spelling:
  - Vorbis and APE use the item key.
  - ID3v2 uses `TXXX:<description>`.
  - MP4 uses `----:<mean>:<name>`.

  ```rust
  const ALIASES: &[(TagKind, TagField, &str)] = &[
      (TagKind::VorbisComments, TagField::MusicbrainzReleaseId, "MUSICBRAINZ_RELEASEID"),
      (TagKind::VorbisComments, TagField::MusicbrainzRecordingId, "MUSICBRAINZ_RECORDINGID"),
      (TagKind::Ape, TagField::MusicbrainzReleaseId, "MUSICBRAINZ_RELEASEID"),
      (TagKind::Ape, TagField::MusicbrainzRecordingId, "MUSICBRAINZ_RECORDINGID"),
      (TagKind::Ape, TagField::TrackTotal, "TRACKTOTAL"), (TagKind::Ape, TagField::TrackTotal, "TOTALTRACKS"),
      (TagKind::Ape, TagField::DiscTotal, "DISCTOTAL"), (TagKind::Ape, TagField::DiscTotal, "TOTALDISCS"),
      (TagKind::Ape, TagField::OriginalDate, "ORIGINALDATE"),
      (TagKind::Id3v2, TagField::MusicbrainzReleaseId, "TXXX:MusicBrainz Release Id"),
      (TagKind::Id3v2, TagField::TrackTotal, "TXXX:TRACKTOTAL"), (TagKind::Id3v2, TagField::TrackTotal, "TXXX:TOTALTRACKS"),
      (TagKind::Id3v2, TagField::DiscTotal, "TXXX:DISCTOTAL"), (TagKind::Id3v2, TagField::DiscTotal, "TXXX:TOTALDISCS"),
      (TagKind::Id3v2, TagField::OriginalDate, "TXXX:ORIGINALYEAR"),
      (TagKind::Mp4Ilst, TagField::MusicbrainzReleaseId, "----:com.apple.iTunes:MusicBrainz Release Id"),
      (TagKind::Mp4Ilst, TagField::MusicbrainzRecordingId, "----:com.apple.iTunes:MusicBrainz Recording Id"),
      (TagKind::Mp4Ilst, TagField::OriginalDate, "----:com.apple.iTunes:ORIGINALYEAR"),
  ];
  /// The primary key is a raw key, not the field's ItemKey; the ItemKey copy is then an alias.
  const PRIMARY_RAW: &[(TagKind, TagField, &str)] =
      &[(TagKind::Id3v2, TagField::MusicbrainzRecordingId, "TXXX:MusicBrainz Track Id")];
  ```
  `ItemKey::Year` (`CopyKey::Year`) is an alias of `Date` in every tag kind.

  UFID is not a raw alias. lofty maps `ItemKey::MusicBrainzRecordingId` to the `UFID` frame owned by `http://musicbrainz.org` (`id3/v2/tag.rs:775-780`), so it is already the field's `CopyKey::Field` copy. A raw entry would list the same frame twice. Never remove UFID frames with `FrameList::remove(&FrameId)`: that removes the frames of every owner.

  ```rust
  /// ID3v2.3 writes TDOR as TORY (year only), so these fields cannot round-trip in a v2.3 file.
  /// Step 2's v2.3 test decides the list; it starts as [OriginalDate].
  const ID3V23_UNWRITABLE: &[TagField] = &[TagField::OriginalDate];
  ```

- [ ] **Step 1: Add the dependencies**

Run `cargo add lofty@0.25.4 rusty-chromaprint@0.3.0`. This needs network: the local registry has both crates but not their dependencies (`lofty_attr`, `ogg_pager`, `rubato`, `rustfft`, …). Do not use `--offline`. If the index is unreachable, stop and report it. Then check that `Cargo.toml` reads `lofty = "0.25.4"` and `rusty-chromaprint = "0.3.0"`, and that `cargo build --locked` succeeds.

- [ ] **Step 2: Write the failing unit tests in `src/tagging/tags.rs`**

Each test copies a fixture from `tests/fixtures/formats/` into a temp dir. The tests prepare stale copies with lofty's concrete tags (`Id3v1Tag`, `ApeTag`, `lofty::ogg::tag::VorbisComments`, `Id3v2Tag`).
- `writes_every_field_into_the_primary_tag_and_reads_it_back`: on `tone.flac`, set all 16 fields to the round-trip values below. Then `read_tags(..).primary` equals them exactly.
- `stale_id3v1_and_apev2_copies_are_removed_and_probe_reads_the_new_values`:
  - Setup: `tone.mp3` with ID3v1 and APEv2 copies of title "Old Title", artist "Old Artist" and album "Old Album".
  - Before: `read_tags(..).copies` lists them, with `read_only == false`.
  - After setting the 3 fields: the returned removed copies equal the pre-read `copies_of`.
  - `crate::metadata::probe` returns title "New Title", artist "New Artist", album "New Album".
  - lofty finds no copies.
- `vorbis_release_id_alias_is_removed`: FLAC with raw `MUSICBRAINZ_RELEASEID=old`. Before writing, `read_tags(..).copies` lists it as a `CopyKey::Raw` copy. After setting `MusicbrainzReleaseId`, the returned removed copies contain it, `probe(..).musicbrainz_release_id == Some(new)`, and the raw key is gone.
- `unmapped_items_survive_in_vorbis_and_ape`: a FLAC with Vorbis comment `ASIN=B000TEST` and an APE file with APE item `ASIN=B000TEST`. lofty maps neither key to an `ItemKey`. After setting `Title`, each file still has `ASIN=B000TEST`, read through the concrete tag.
- `id3_recording_id_is_written_as_txxx_and_ufid_is_removed`: MP3 tagged through lofty's generic `ItemKey::MusicBrainzRecordingId`, which lofty writes as a UFID `http://musicbrainz.org` frame. After writing, the `TXXX:MusicBrainz Track Id` value equals the new ID, no UFID frame owned by `http://musicbrainz.org` remains, a UFID frame with another owner survives, and `probe(..).musicbrainz_recording_id == Some(new)`.
- `restore_puts_removed_copies_back`: after the MP3 edit above, `edit_tags` with `set` = the before primary values and `restore` = the removed copies. The ID3v1 and APE copies read back as before.
- `untouched_items_and_pictures_survive`: genre "Keep Genre", comment "Keep Comment" and a 1×1 PNG front cover survive setting only `Title`.
- `read_only_id3_in_flac_is_flagged`:
  - Setup: a copy of `tone.flac` with a leading ID3v2 tag. Build an `Id3v2Tag` with title "Old Title", serialise it with `TagExt::dump_to`, and prepend the bytes.
  - `read_tags` reports a copy with `tag == TagKind::Id3v2, read_only == true`.
  - `edit_tags` that sets `Title` returns an error, and the file is unchanged.
- `id3v23_files_stay_v23`: `tone.mp3` is ID3v2.4 (header byte 3 is `4`). So first make a v2.3 copy: save an `Id3v2Tag` with `WriteOptions::default().use_id3v23(true)`. After writing a title, header byte 3 is still `3` and `read_tags(..).id3v23` is true.
- `id3v23_round_trip_follows_its_deny_list`: on that v2.3 copy, `edit_tags` with every round-trip value.
  - Every field outside `ID3V23_UNWRITABLE` reads back equal through `read_tags(..).primary`.
  - Every field in `ID3V23_UNWRITABLE` really reads back different. For `OriginalDate` it reads back "1999".
  - If `Date` also reads back different, add it to `ID3V23_UNWRITABLE` with a one-line comment.

Round-trip values (also used in Step 6). They live once, in `tests/fixtures/tag_roundtrip_values.json`: a JSON object keyed by the `TagField` wire names. The unit tests here and `tests/ffmpeg_formats.rs` both load it into a `BTreeMap<TagField, String>` from `env!("CARGO_MANIFEST_DIR")`. Neither copies the values into code.

```json
{"title": "Round Trip Title ü", "artist": "Round Artist", "album": "Round Album",
 "album_artist": "Round Album Artist", "track_number": "3", "track_total": "12", "disc_number": "1", "disc_total": "2",
 "date": "2001-05-14", "original_date": "1999-02-01",
 "musicbrainz_recording_id": "b1000000-0000-4000-8000-000000000001", "musicbrainz_track_id": "a1000000-0000-4000-8000-000000000001",
 "musicbrainz_release_id": "11111111-1111-4111-8111-111111111111", "musicbrainz_release_group_id": "99999999-9999-4999-8999-999999999999",
 "musicbrainz_artist_id": "c1000000-0000-4000-8000-000000000001", "musicbrainz_album_artist_id": "c1000000-0000-4000-8000-000000000002"}
```

Below, `VALUES` means this loaded map.

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test --locked --lib tagging::tags`
Expected: compile errors for the missing `tags` items.

- [ ] **Step 4: Implement `src/tagging/tags.rs`**

- **Parsing:** use `Probe::new(BufReader::new(File::open(path)?)).guess_file_type()?.read()`. This detects the type from content only, never from the extension. Keep `ParseOptions` defaults: `read_cover_art` must stay on, or pictures are dropped on save. Do not call `apply_global_options`; `preserve_format_specific_items` must stay true.
- **Vorbis comments and APE tags never go through the generic `Tag` alone.** In lofty 0.25.4, `From<VorbisComments> for Tag` and `From<ApeTag> for Tag` keep only `split_tag().1` and drop every unmapped key (`ogg/tag.rs:686-690`, `ape/tag/mod.rs:611-615`). Only ID3v2 and MP4 keep such items, in a companion tag. `TaggedFile` is built through these conversions. So a `TaggedFile` cannot see the Vorbis/APE raw aliases, and saving its generic tag deletes unmapped items such as `ASIN`.
  - Read these tags from the concrete `AudioFile` type with `AudioFile::read_from(&mut reader, ParseOptions::new())`, through its `LoftyFile` accessors (for example `vorbis_comments()`/`vorbis_comments_mut()`, `ape()`/`ape_mut()`):
    - Vorbis comments from `FlacFile`, `VorbisFile`, `OpusFile` or `SpeexFile`;
    - APE tags from `ApeFile`, `MpcFile`, `WavPackFile` or `MpegFile`.
  - Edit raw keys on the concrete tag (`VorbisComments::{get_all, remove, push}`, `ApeTag::{get, remove, insert}`).
  - Edit `ItemKey` fields through `SplitTag::split_tag` → generic `Tag` → `MergeTag::merge_tag`. Unmapped items then stay in the remainder.
  - Save the concrete tag with `TagExt::save_to`.
  - FLAC keeps its pictures in `FlacFile`, not in the tag. Saving Vorbis comments to FLAC rewrites every PICTURE block from the tag's pictures (`flac/write.rs:76-80`). So first move the file's pictures into the Vorbis comments with `OggPictureStorage::insert_picture`.
  - ID3v2, ID3v1, MP4 ilst, RIFF INFO and AIFF text use the generic `Tag` from the `TaggedFile`.
- **`writable_format`:** map lofty `FileType` to `WritableFormat`. Return `None` for anything else, and for `Mp4` whose bytes 8..12 are `qt  `.
- **`read_tags`:**
  - The primary tag is `TaggedFileExt::primary_tag_type()`.
  - `id3v23` is true when the file starts with `ID3` and byte 3 is `3`.
  - Read raw ID3v2 and MP4 keys through concrete conversions of a clone of the generic tag. Use `Id3v2Tag::from`, which derefs to `FrameList` (`get_user_text`, `remove_user_text`, `insert_user_text`), and `Ilst::from` (`AtomIdent::Freeform`). Read raw Vorbis and APE keys from the concrete tags above.
  - `read_only` is `file_type.tag_support(tag_type) == TagSupport::ReadOnly`.
  - Extra items of the primary key in the primary tag become `CopyKey::Field` copies.
- **`edit_tags`, for each field in `set`:**
  - First remove the field everywhere: its ItemKey items, `ItemKey::Year` for `Date`, and its raw aliases, in every writable tag.
  - Then insert the primary value into the primary tag (`Tag::new(primary)` when absent), at the ItemKey or at its `PRIMARY_RAW` key.
  - A `None` value only removes.
- **`edit_tags`, `restore`:** each copy goes back to its tag (create the tag when absent; `push` for `Field`/`Year`, raw insert for `Raw`).
- **Read-only copies:** if a read-only tag holds a copy of a field in `set`, return an error. Never modify a read-only tag.
- **Saving:** open the file read+write once. For each modified tag, `rewind` and then `TagExt::save_to(&mut file, options)` on that tag (the concrete tag for Vorbis comments and APE). An empty tag is saved empty, so lofty strips it. Use `options = WriteOptions::default()`, plus `.use_id3v23(true)` when the file starts with `ID3` and byte 3 is `3`. Return the copies actually removed.
- **`unwritable_fields`:** an empty match for now. Step 7 fills it.

- [ ] **Step 5: Run the unit tests and see them pass**

Run: `cargo test --locked --lib tagging::tags`
Expected: PASS.

- [ ] **Step 6: Write the failing round-trip tests in `tests/ffmpeg_formats.rs`**

They reuse `manifest()`, `Scratch` and the Owner-bootstrap from `every_format_fixture_indexes_without_failures`.
- `writable_formats_are_detected_from_content`: `writable_format` must give exactly this map per fixture file:
  - `aac`→Aac;
  - `aif`, `aiff`, `aifc`→Aiff;
  - `alac`, `m4a`, `m4b`, `mp4`→Mp4;
  - `ape`→Ape; `flac`→Flac;
  - `mp2`, `mp3`→Mpeg;
  - `mpc`→Mpc; `ogg`→Vorbis; `opus`→Opus; `spx`→Speex;
  - `wav`, `wave`→Wav; `wv`→WavPack;
  - and `None` for `ac3`, `amr`, `au`, `awb`, `tone.caf`, `tone-heaac.caf`, `dff`, `dsf`, `dts`, `mka`, `mov`, `oga`, `ra`, `shn`, `tta`, `webm`, `wma`.
- `every_writable_fixture_round_trips_every_field`:
  - Setup: copy each writable fixture, add genre "Keep Genre" and a PNG cover, then `edit_tags` with `VALUES` minus `unwritable_fields(format)`.
  - lofty: `read_tags(..).primary` equals the written values.
  - Genre and cover survive.
  - `Decoder::open(.., GainMode::Off)` decodes exactly the manifest's `frames`.
  - Every field in `unwritable_fields(format)` really fails to read back (through lofty or through the library). This keeps the deny list honest.
- `library_reads_written_fields`: scan the written copies with `Library` (as the existing indexing test does), then page `LibraryCommand::Tracks { album_id: None, .. }`. For each track, compare against the expected values. Skip fields deny-listed for that format.
  - `title`, `artist`, `album`;
  - `album_artist_tag == Some("Round Album Artist")`;
  - track and disc number/total;
  - `year == Some(2001)`;
  - all six `musicbrainz_*` fields;
  - `genres == ["Keep Genre"]`.

- [ ] **Step 7: Run them, then record each real failure in `unwritable_fields`**

Run: `cargo test --locked --test ffmpeg_formats`
For every (format, field) that fails its round-trip, first check that it is not a bug in `tags.rs`. If it is a real format limit, add it to `unwritable_fields` with a one-line comment naming what failed (lofty read-back or FFmpeg). A format whose every field fails must instead return `None` from `writable_format`. Re-run until it passes.

- [ ] **Step 8: Run against the release FFmpeg prefix**

Run: the last two lines of the Gates block.
Expected: PASS. A field that differs only under the release FFmpeg also goes in the deny list, with a comment.

- [ ] **Step 9: Run all gates, then commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/tagging src/protocol.rs tests/ffmpeg_formats.rs tests/fixtures/tag_roundtrip_values.json
git commit -m "Read and write MusicBrainz tag fields with lofty"
```

---

### Task 2: Safe write through a verified clone

**Files:**
- Create: `src/tagging/safe_write.rs` (with `#[cfg(test)] mod tests`)
- Modify: `src/tagging/mod.rs` (`mod safe_write;`)

**Interfaces:**
- Consumes: `tags::{edit_tags, TagEdit, TagCopy}`.
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub(crate) struct FileStamp { pub size: u64, pub modified_ns: i64 }
  pub(crate) fn stamp(path: &Path) -> std::io::Result<FileStamp>      // symlink_metadata; regular files only
  #[derive(Debug)] pub(crate) enum WriteError { Changed, Failed(String) }
  pub(crate) fn temp_path(path: &Path) -> PathBuf                     // <dir>/.<file name>.tyranny-tmp
  pub(crate) fn write_safely(path: &Path, expected: FileStamp, edit: &TagEdit) -> Result<Vec<TagCopy>, WriteError>
  fn write_with(path: &Path, expected: FileStamp, edit: &TagEdit,
                before_commit: impl FnOnce(&Path) -> anyhow::Result<()>) -> Result<Vec<TagCopy>, WriteError> // test seam
  ```
- Order, from the spec:
  1. A stamp mismatch or a missing file gives `Changed`.
  2. Remove a stale temporary file, then `std::fs::copy` the original to the temporary path.
  3. `edit_tags` on the temporary file.
  4. `before_commit(&temp)`, then `File::open(temp)?.sync_all()`.
  5. Re-stamp the original; a mismatch gives `Changed`.
  6. `rename(temp, path)`, then `sync_all` the parent directory.
  7. On any error, remove the temporary file. `Failed` carries the readable error.

- [ ] **Step 1: Write the failing tests**
- `failed_write_after_clone_leaves_original_identical_and_no_temp`: the hook returns `Err`. The original bytes equal the bytes before, `!temp_path(..).exists()`, and the result is `Failed`.
- `original_changed_before_rename_is_reported_changed`: the hook appends one byte to the original. The result is `Changed`, the original equals the hook-modified bytes, and no temporary file remains.
- `stale_stamp_is_changed_without_touching_file`.
- `stale_temp_file_from_a_crash_is_replaced` (Review Focus 1): pre-create the temporary file with junk; the write succeeds and the temporary file is gone.
- `unicode_file_names_write` (Review Focus 1): a file named `Ünïcødé – 日本 song.flac`.
- `permissions_survive_and_unwritable_directory_fails_cleanly` (Review Focus 2): mode 0640 stays 0640. With the directory at 0555 the result is `Failed`, the original is identical and no temporary file remains. Restore the directory to 0755 before the scratch dir is dropped.
- `deleted_file_is_changed` (Review Focus 3).
- `decoder_open_during_write_keeps_decoding`: open a `Decoder` on a copy of `tone.flac` and read 4096 frames. Then `write_safely` a title and read to EOF. The total is 12000, the manifest's frames.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib tagging::safe_write`
Expected: compile errors.

- [ ] **Step 3: Implement `safe_write.rs` as specified above**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --lib tagging::safe_write`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src/tagging
git commit -m "Write tags through a verified clone and rename"
```

---

### Task 3: Edit journal

**Files:**
- Create: `src/tagging/journal.rs` (with tests)
- Modify: `src/tagging/mod.rs` (`mod journal;`), `src/protocol.rs` (add `TagEditSummary`)

**Interfaces:**
- Produces, in `src/protocol.rs`:
  ```rust
  #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
  pub struct TagEditSummary { pub edit_id: u64, pub created_at: i64, pub release_title: String, pub file_count: u32, pub reverted: bool }
  ```
- Produces, in `journal.rs`:
  ```rust
  #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
  pub(super) struct JournalFile { pub track_id: u64, pub path: PathBuf,
      pub before: BTreeMap<TagField, Option<String>>, pub after: BTreeMap<TagField, Option<String>>,
      pub removed: Vec<TagCopy> }
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub(super) enum FileStatus { Pending, Written, Failed, Reverted }
  pub(super) struct EditRecord { pub edit_id: u64, pub release_id: String, pub release_title: String,
      pub reverted: bool, pub files: Vec<(JournalFile, FileStatus)> }
  pub(super) const KEPT_EDITS: u64 = 200;
  pub(super) struct Journal { connection: rusqlite::Connection }
  impl Journal {
      pub(super) fn open(path: &Path) -> anyhow::Result<Self>
      pub(super) fn begin(&mut self, release_id: &str, release_title: &str, files: &[JournalFile]) -> anyhow::Result<u64>
      pub(super) fn set_status(&mut self, edit_id: u64, track_id: u64, status: FileStatus) -> anyhow::Result<()>
      pub(super) fn load(&self, edit_id: u64) -> anyhow::Result<Option<EditRecord>>
      pub(super) fn mark_reverted(&mut self, edit_id: u64) -> anyhow::Result<()>
      pub(super) fn recent(&self, limit: usize) -> anyhow::Result<Vec<TagEditSummary>>
  }
  ```
- Storage:
  - **File:** opened like `src/lyrics/cache.rs::Cache::open`: mode 0600, `O_NOFOLLOW | O_CLOEXEC`, must be a regular file. The open code may be duplicated from the lyrics cache; that duplication is accepted.
  - **Identity:** `PRAGMA application_id = 0x7479_7467`, `user_version = 1`. A fresh file (`application_id` 0 and `user_version` 0) is accepted and stamped with this identity, as `lyrics/cache.rs:39` does. Any other identity is refused.
  - **Pragmas:** `journal_mode=DELETE`, `synchronous=FULL`, `foreign_keys=ON`.
  - **Tables:**
    - `edits(id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, release_id TEXT NOT NULL, release_title TEXT NOT NULL, reverted INTEGER NOT NULL DEFAULT 0)`;
    - `edit_files(edit_id INTEGER NOT NULL REFERENCES edits(id) ON DELETE CASCADE, position INTEGER NOT NULL, track_id INTEGER NOT NULL, body TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pending','written','failed','reverted')), PRIMARY KEY(edit_id, position))`. `body` is the JSON of `JournalFile`.
  - **`begin`:** inserts all files as `pending` in one transaction. Then it deletes edits with `id <= max(id) - KEPT_EDITS`.
  - **`file_count`:** counts files whose status is not `failed`.
  - **`recent`:** newest first.

- [ ] **Step 1: Write the failing tests**
- `begin_load_round_trips_files_and_statuses`.
- `only_the_latest_200_edits_are_kept`: after 205 `begin`s, `load(1..=5)` is `None`, `load(6)` is `Some`, and `recent(50).len() == 50` with the newest first.
- `file_count_excludes_failed_files`.
- `mark_reverted_sets_the_flag`.
- `journal_is_private_and_refuses_symlinks_and_foreign_databases`: mode 0600; a symlink path errors; a SQLite file with another `application_id` errors; an empty new path opens and then reads back `application_id = 0x7479_7467`.
- `reopen_keeps_edits`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib tagging::journal`
Expected: compile errors.

- [ ] **Step 3: Implement `journal.rs` and `TagEditSummary`**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --lib tagging::journal`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src/tagging src/protocol.rs
git commit -m "Journal tag edits for revert"
```

---

### Task 4: Rate-gated HTTP and the MusicBrainz provider

**Files:**
- Create: `src/tagging/http.rs`, `src/tagging/musicbrainz.rs`, `src/tagging/test_support.rs`
- Create: `src/cancel.rs` (declared `mod cancel;` in `src/lib.rs`)
- Create: `tests/fixtures/musicbrainz/release-a.json`, `release-b.json`, `release-c.json`, `search.json`, `search-empty.json`
- Modify: `src/lyrics/mod.rs` (use `crate::cancel`), `src/tagging/mod.rs` (`check_lookup`, `LookupError`, module declarations)

**Interfaces:**
- Produces, in `src/cancel.rs` (crate-private). `Cancellation` and `CancelOnDrop` move here from `src/lyrics/mod.rs:37-68`, and lyrics and tagging both use them. Lyrics' behaviour, messages and tests stay unchanged. Lyrics keeps its own inherent `fn check(&self, deadline) -> anyhow::Result<()>`, with today's two messages, in `lyrics/mod.rs`, built on `stopped`. `use crate::cancel::{CancelOnDrop, Cancellation};` in `lyrics/mod.rs` keeps `super::Cancellation` and `Cancellation::new()` working in `discovery.rs`, `provider.rs` and the lyrics tests.
  ```rust
  #[derive(Clone)] pub(crate) struct Cancellation { stop: Arc<AtomicBool>, abandoned: Arc<AtomicBool> }
  pub(crate) enum Stop { Cancelled, Expired }
  impl Cancellation {
      pub(crate) fn with_flags(stop: Arc<AtomicBool>, abandoned: Arc<AtomicBool>) -> Self;
      #[cfg(test)] pub(crate) fn new() -> Self;                       // both flags false
      pub(crate) fn cancelled(&self) -> bool;                         // stop || abandoned
      pub(crate) fn stopped(&self, deadline: Instant) -> Option<Stop> // Cancelled first, then Expired at/after deadline
  }
  pub(crate) struct CancelOnDrop(pub(crate) Arc<AtomicBool>);       // sets the flag on drop
  ```
- Produces, in `src/tagging/mod.rs`:
  ```rust
  impl Cancellation { pub(super) fn check_lookup(&self, deadline: Instant) -> Result<(), LookupError> } // Cancelled / TimedOut
  #[derive(Debug)] pub(super) enum LookupError { Busy(&'static str), Cancelled, TimedOut, Failed(String) }
  // Display: Busy(s) => "{s} is busy, try again shortly", Cancelled => "lookup cancelled",
  //          TimedOut => "lookup took too long", Failed(m) => m
  ```
  Wherever this plan says `cancel.check(deadline)` for tagging code, it means `check_lookup`.
- Produces, in `http.rs`:
  ```rust
  pub(super) const USER_AGENT: &str = concat!("Tyranny/", env!("CARGO_PKG_VERSION"), " (https://github.com/kthwaite/tyranny)");
  pub(super) const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
  #[derive(Clone)] pub(super) struct StartGate { next: Arc<Mutex<Instant>>, interval: Duration }
  impl StartGate { pub(super) fn new(interval: Duration) -> Self;
      pub(super) fn wait(&self, cancel: &Cancellation, deadline: Instant) -> Result<(), LookupError> }
  pub(super) fn client(https_only: bool) -> reqwest::blocking::ClientBuilder // UA, no redirects, 3 s/6 s; callers .build()
  pub(super) fn loopback_origin(origin: &str) -> anyhow::Result<Url>          // for_test validation (may duplicate lyrics' check; accepted)
  pub(super) fn get_json(client: &Client, gate: &StartGate, url: Url, service: &'static str,
      cancel: &Cancellation, deadline: Instant) -> Result<Vec<u8>, LookupError>
  ```
  - `wait` reserves the next start like `src/artwork/provider.rs:179-200`, but sleeps in ≤ 20 ms steps and checks `cancel` between them.
  - `get_json` sends `Accept: application/json`.
  - Status mapping:
    - `503`, `429`, or any `Retry-After` header gives `Busy(service)`;
    - `404` gives `Failed("{service} has no such record")`;
    - a redirect gives `Failed("{service} redirect rejected")`;
    - any other non-200 gives `Failed("{service} returned an unsuccessful response (HTTP {code})")`;
    - a non-JSON content type gives `Failed("{service} returned non-JSON content")`;
    - over 2 MiB (declared or streamed) gives `Failed("{service} response exceeds 2 MiB")`;
    - a connect or IO error gives `Failed("{service} could not be reached")`.
- Produces, in `musicbrainz.rs`:
  ```rust
  pub(super) struct MusicBrainz { client: Client, gate: StartGate, origin: Url }
  impl MusicBrainz {
      pub(super) fn new() -> anyhow::Result<Self>                         // https://musicbrainz.org/, 1 s
      #[cfg(test)] pub(super) fn for_test(origin: &str, interval: Duration) -> anyhow::Result<Self>
      pub(super) fn release(&self, id: &str, cancel: &Cancellation, deadline: Instant) -> Result<Release, LookupError>
      pub(super) fn search(&self, query: &SearchQuery<'_>, cancel: &Cancellation, deadline: Instant) -> Result<Vec<SearchHit>, LookupError>
  }
  pub(super) struct SearchQuery<'a> { pub title: &'a str, pub artist: &'a str, pub tracks: Option<usize> }
  pub(super) fn lucene(query: &SearchQuery<'_>) -> String
  pub(super) fn is_mbid(value: &str) -> bool                           // 8-4-4-4-12 lowercase hex
  // ws/2 models (serde, unknown keys ignored, "" dates → None):
  pub(super) struct Release { pub id: String, pub title: String, pub date: Option<String>, pub country: Option<String>,
      pub artist_credit: Vec<Credit>, pub release_group: Option<ReleaseGroup>, pub media: Vec<Medium> }
  pub(super) struct Credit { pub name: String, pub joinphrase: String, pub artist: CreditArtist }
  pub(super) struct CreditArtist { pub id: String }
  pub(super) struct ReleaseGroup { pub id: String, pub first_release_date: Option<String> }
  pub(super) struct Medium { pub position: u32, pub track_count: u32, pub tracks: Vec<ReleaseTrack> }
  pub(super) struct ReleaseTrack { pub id: String, pub position: u32, pub title: String, pub length: Option<u64>,
      pub artist_credit: Vec<Credit>, pub recording: Recording }
  pub(super) struct Recording { pub id: String, pub title: String, pub length: Option<u64>, pub artist_credit: Vec<Credit> }
  pub(super) struct SearchHit { pub id: String, pub score: u32 }
  pub(super) fn credit_name(credits: &[Credit]) -> String              // name + joinphrase, concatenated
  impl Release { pub fn artist(&self) -> String; pub fn track_count(&self) -> u32;
      pub fn duration_ms(&self) -> Option<u64> }  // sum of track.length.or(recording.length); None if any track has neither
  ```
  - Release URL: `<origin>ws/2/release/<id>?inc=recordings+artist-credits+release-groups+media&fmt=json`. A non-MBID `id` is refused before any request.
  - Search URL: `<origin>ws/2/release?query=<lucene>&limit=5&fmt=json`.
  - Lucene query: `release:"<title>" AND artist:"<artist>"`, plus ` AND tracks:<n>` when `tracks` is `Some`. Inside the quotes, escape `\` and `"` with a backslash.
  - `new` uses `http::client(true).build()`. `for_test` uses `http::client(false).no_proxy().build()` and `loopback_origin`.
- Produces, in `test_support.rs` (cfg(test)):
  ```rust
  pub(super) struct HttpFixture { /* TcpListener thread, Arc<Mutex<Vec<Seen>>> */ }
  pub(super) struct Seen { pub at: Instant, pub target: String, pub headers: String }
  impl HttpFixture {
      pub(super) fn new(route: impl Fn(&str) -> Reply + Send + 'static) -> Self // target → reply
      pub(super) fn origin(&self) -> String; pub(super) fn seen(&self) -> Vec<Seen> }
  pub(super) struct Reply { pub status: u16, pub headers: Vec<(&'static str, String)>, pub body: Vec<u8>, pub delay: Duration }
  pub(super) fn json(status: u16, body: Vec<u8>) -> Reply     // Content-Type: application/json
  pub(super) fn fixture(name: &str) -> Vec<u8>                // tests/fixtures/musicbrainz/<name>
  ```
- **Fixtures.** They are hand-written minimal JSON with the real ws/2 key names (`artist-credit`, `release-group`, `first-release-date`, `track-count`, `joinphrase`, `media`, `tracks`, `recording`, `length`), following https://musicbrainz.org/doc/MusicBrainz_API. Each keeps only the keys the parser reads, plus one unknown key (`"status": "Official"`) to prove unknown keys are ignored. They are not recorded from live traffic.
  - `release-a.json`:
    - release `11111111-1111-4111-8111-111111111111` "Fixture Album", credited to "Fixture Artist" (artist `c1000000-0000-4000-8000-000000000002`);
    - date `2001-05-14`, country `GB`;
    - release group `99999999-9999-4999-8999-999999999999` with `first-release-date` `1999-02-01`;
    - one medium with 3 tracks, ids `a1000000-…-00000000000N` and recordings `b1000000-…-00000000000N`, each 250 ms;
    - track 2 credited "Fixture Artist" + " feat. " + "Guest".
  - `release-b.json`: `22222222-2222-4222-8222-222222222222`, same title, date `2003`, country `US`, 4 tracks, `first-release-date: ""`, and one track with `length: null`.
  - `release-c.json`: `33333333-3333-4333-8333-333333333333`, 2 media of 2 tracks each. Disc 2 track 1 has `length: null` on both the track and its recording (Task 6 tests this).
  - `search.json`: `releases` = [A score 100, B score 90].
  - `search-empty.json`: `{"created":"2026-10-21T00:00:00.000Z","count":0,"offset":0,"releases":[]}`.

- [ ] **Step 1: Write the failing tests (in `musicbrainz.rs`)**
- `release_request_path_headers_and_parse`:
  - The fixture saw exactly `GET /ws/2/release/11111111-1111-4111-8111-111111111111?inc=recordings+artist-credits+release-groups+media&fmt=json`.
  - Headers contain `USER_AGENT` and `accept: application/json`.
  - Parsed: `artist() == "Fixture Artist"`, track 2 `credit_name == "Fixture Artist feat. Guest"`, `track_count() == 3`, `duration_ms() == Some(750)`, release group first date `Some("1999-02-01")`.
- `search_escapes_lucene_and_parses_scores`:
  - `lucene(&SearchQuery { title: r#"AC/DC "Live" \ Back"#, artist: "Guns N' Roses", tracks: Some(12) })` returns exactly `release:"AC/DC \"Live\" \\ Back" AND artist:"Guns N' Roses" AND tracks:12`.
  - The request target starts `/ws/2/release?query=` and ends `&limit=5&fmt=json`.
  - Hits are `[(A, 100), (B, 90)]`.
- `gate_spaces_request_starts_by_at_least_one_second`: `for_test(origin, Duration::from_secs(1))`, three `release` calls; consecutive `seen().at` differ by ≥ 1 s.
- `busy_statuses_stop_without_retry`: 503, then 429, then 500 with `Retry-After: 5`. Each gives `Busy("MusicBrainz")`, whose Display is "MusicBrainz is busy, try again shortly", and the fixture saw exactly one request.
- `redirects_404_oversize_and_non_json_fail_readably` (one case each, message as mapped).
- `cancelled_lookups_send_nothing`: a pre-cancelled `Cancellation` gives `Cancelled` and the fixture saw nothing.
- `empty_dates_are_absent`: release B has `release_group.first_release_date == None`; a `"date": ""` variant parses to `None`.
- `non_mbid_ids_are_refused_before_http` and `for_test_rejects_non_loopback_origins`.
- `duration_falls_back_to_recording_length`: a release whose track has `length: null` and whose recording has a length counts the recording length. Release C (no length on either) gives `None`.
- In `src/cancel.rs`: `stopped_reports_cancel_before_deadline`: a fresh `Cancellation` before its deadline gives `None`; after `stop` it gives `Cancelled`, even past the deadline; a fresh one past its deadline gives `Expired`; dropping a `CancelOnDrop` sets its flag.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib -- tagging::musicbrainz cancel`
Expected: compile errors.

- [ ] **Step 3: Implement `src/cancel.rs`, the lyrics switch to it, the `mod.rs` additions, `http.rs`, `musicbrainz.rs` and `test_support.rs`**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --lib -- tagging::musicbrainz cancel lyrics`
Expected: PASS, including every existing lyrics test, unchanged.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src/cancel.rs src/lib.rs src/lyrics src/tagging tests/fixtures/musicbrainz
git commit -m "Add the MusicBrainz provider with a one-second gate"
```

---

### Task 5: AcoustID lookup and fingerprints

**Files:**
- Create: `src/tagging/acoustid.rs`, `src/tagging/fingerprint.rs`
- Create: `tests/fixtures/musicbrainz/acoustid.json`, `acoustid-error.json`
- Modify: `src/tagging/test_support.rs` (`write_tone_wav`), `src/tagging/mod.rs`, `src/tagging/http.rs` (`get_json_or_error`)

**Interfaces:**
- Consumes: `http::{StartGate, get_json, client, loopback_origin}`, `Cancellation` (`check_lookup`), `LookupError`.
- Adds, in `http.rs`:
  ```rust
  /// As `get_json`, but a 4xx reply whose content type is JSON returns `Ok((status, body))` instead of `Failed`.
  /// Busy statuses, redirects, size and transport errors map exactly as in `get_json`.
  pub(super) fn get_json_or_error(client: &Client, gate: &StartGate, url: Url, service: &'static str,
      cancel: &Cancellation, deadline: Instant) -> Result<(u16, Vec<u8>), LookupError>
  ```
  AcoustID sends its error JSON with an HTTP 4xx status (verify against the API docs; a 400 for an invalid key is expected), so `AcoustId::lookup` uses `get_json_or_error`.
- Produces:
  ```rust
  // fingerprint.rs
  pub(super) const FINGERPRINT_SECONDS: u64 = 120;
  pub(super) struct Fingerprint { pub encoded: String, pub duration_s: u32 }
  pub(super) fn fingerprint(path: &Path, cancel: &Cancellation, deadline: Instant) -> anyhow::Result<Option<Fingerprint>>
  // acoustid.rs
  pub(super) struct AcoustId { client: Client, gate: StartGate, origin: Url }
  impl AcoustId {
      pub(super) fn new() -> anyhow::Result<Self>                          // https://api.acoustid.org/, 334 ms
      #[cfg(test)] pub(super) fn for_test(origin: &str, interval: Duration) -> anyhow::Result<Self>
      pub(super) fn lookup(&self, key: &str, print: &Fingerprint, cancel: &Cancellation, deadline: Instant)
          -> Result<Vec<String>, LookupError>                              // release IDs this file votes for, deduplicated
  }
  pub(super) fn rank_releases(per_file: &[Vec<String>]) -> Vec<(String, u32)> // by votes desc, then first seen; ≤ 5
  // test_support.rs
  pub(super) fn write_tone_wav(path: &Path, seconds: u32)                 // 48 kHz s16 stereo log chirp 200→2000 Hz
  ```
  - **Fingerprint:**
    - Decode with `Decoder::open(path, GainMode::Off)` for at most `FINGERPRINT_SECONDS * SAMPLE_RATE` frames.
    - Convert each `[f32; 2]` frame to i16 with `(s.clamp(-1.0, 1.0) * 32767.0) as i16`, interleaved.
    - Use `rusty_chromaprint::Fingerprinter::new(&Configuration::preset_test2())`, then `start(SAMPLE_RATE, 2)`, `consume`, and `finish`.
    - Encode with `FingerprintCompressor::from(&config).compress(fp)`, then `base64::engine::general_purpose::URL_SAFE_NO_PAD`.
    - `duration_s` comes from `Decoder::duration_frames()`, or the decoded frame count, divided by `SAMPLE_RATE`.
    - Return `None` when there are no sub-fingerprints. Check `cancel` every 1 s of audio.
  - **Request:** `GET <origin>v2/lookup?client=<key>&meta=recordingids+releaseids&duration=<s>&fingerprint=<fp>&format=json`.
    - `"status":"ok"` → `results[].recordings[].releases[].id`.
    - `"status":"error"`, with HTTP 200 or a JSON 4xx → `Failed("AcoustID: <error.message>")`. A 4xx without that JSON shape gives the `get_json` message.
    - `fingerprint` returns `anyhow::Error`. The engine maps an error that wraps a `LookupError` (cancel, timeout) to that `LookupError`, and any other error to `Failed`.
    - The key never appears in an error message.
  - **Fixtures**, hand-written to the AcoustID v2 lookup response shape:
    - `acoustid.json`: one result whose recordings list releases B and A;
    - `acoustid-error.json`: `{"status":"error","error":{"code":4,"message":"invalid API key"}}`.

- [ ] **Step 1: Write the failing tests**
- `fingerprint_of_a_generated_tone_is_url_safe_chromaprint`: `write_tone_wav(p, 10)`. Then `fingerprint(..)` is `Some` with `duration_s == 10`, `encoded.starts_with("AQ")`, and every char in `[A-Za-z0-9_-]`.
- `lookup_request_shape_and_release_ids`: the target contains `client=KEY123`, `meta=recordingids+releaseids`, `duration=10`, `format=json` and `fingerprint=`. The result is `[B, A]`.
- `acoustid_errors_are_readable_and_never_echo_the_key`: `acoustid-error.json` served with HTTP 400, and again with HTTP 200. Each time the Display is "AcoustID: invalid API key" and does not contain "KEY123".
- `gate_spaces_acoustid_requests_by_334_ms`: three lookups, gaps ≥ 334 ms.
- `busy_acoustid_stops`: a 503 gives "AcoustID is busy, try again shortly".
- `rank_releases_counts_votes_and_keeps_five`: `[[A,B],[B],[B,C],[D],[E],[F],[G]]` gives `[(B,3),(A,1),(C,1),(D,1),(E,1)]`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib -- tagging::acoustid tagging::fingerprint`
Expected: compile errors.

- [ ] **Step 3: Implement `acoustid.rs`, `fingerprint.rs` and `write_tone_wav`**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --lib -- tagging::acoustid tagging::fingerprint`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src/tagging tests/fixtures/musicbrainz
git commit -m "Add AcoustID fingerprint lookup"
```

---

### Task 6: Scoring, matching and proposed values

**Files:**
- Create: `src/tagging/matching.rs` (pure; with tests)
- Modify: `src/protocol.rs` (add `FieldChange`), `src/tagging/mod.rs`

**Interfaces:**
- Consumes: `musicbrainz::{Release, credit_name}`, `tags::FileTags`, `TagField`.
- Produces, in `src/protocol.rs`:
  ```rust
  #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
  pub struct FieldChange { pub field: TagField, pub old: Option<String>, pub new: String }
  ```
- Produces, in `matching.rs`:
  ```rust
  pub(super) struct AlbumTrack { pub track_id: u64, pub path: PathBuf, pub title: String, pub duration_ms: Option<u64>,
      pub disc_number: Option<u32>, pub track_number: Option<u32>, pub release_id: Option<String>,
      pub recording_id: Option<String>, pub release_track_id: Option<String> }
  pub(super) struct AlbumFacts { pub title: String, pub artist: String, pub track_count: usize, pub duration_ms: Option<u64> }
  #[derive(Clone, Copy, Default)] pub(super) struct Evidence { pub search_score: Option<u32>, pub votes: Option<u32> }
  pub(super) fn score(album: &AlbumFacts, release: &Release, evidence: Evidence) -> u32
  pub(super) fn similarity(a: &str, b: &str) -> f64
  pub(super) fn most_common_release(tracks: &[AlbumTrack]) -> Option<String>
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub(super) struct TrackRef { pub medium: usize, pub track: usize }
  pub(super) fn match_files(files: &[AlbumTrack], release: &Release) -> Vec<Option<TrackRef>>
  pub(super) fn proposed(release: &Release, at: TrackRef) -> BTreeMap<TagField, String>
  pub(super) fn changes(tags: &FileTags, proposed: &BTreeMap<TagField, String>) -> Vec<FieldChange>
  ```
  - **`score`** is the sum of:
    - `1000` if `release.track_count() == album.track_count`;
    - `300` if both durations are known and differ by ≤ 2 % of the album's duration;
    - `round(200 × similarity(album.title, release.title))`;
    - `search_score.unwrap_or(0)`;
    - `100 × votes / album.track_count` (integer).
  - **`similarity`:** `1 − levenshtein / max(char len)` over trimmed, lowercased chars. Two empty strings give `1.0`.
  - **`match_files`** runs three passes over all files, in order:
    1. `release_track_id == track.id`, or `recording_id == track.recording.id`;
    2. `(disc_number.unwrap_or(1), track_number) == (medium.position, track.position)`;
    3. `|duration − length| ≤ 3000 ms`, where `length` is `track.length.or(recording.length)` and a `None` length is skipped. Among those, take the lowest edit distance of titles, then the smallest duration gap.

    Each release track is used at most once.
  - **`proposed`** follows the spec table:
    - `artist` = `credit_name(track credits, else recording credits)`;
    - `album_artist` = `release.artist()`;
    - `track_number` = `track.position`; `track_total` = `medium.track_count`;
    - `disc_number` = `medium.position`; `disc_total` = `media.len()`;
    - `date` = `release.date`; `original_date` = the release group's `first_release_date`;
    - artist IDs come from the first credit.

    A value that is absent or empty is left out (decision 8).
  - **`changes`:** numbers are compared after parsing to `u32`; everything else as exact strings. `old` is `tags.current(field)`. The result is in `TagField` order.

- [ ] **Step 1: Write the failing tests**
- `score_prefers_track_count_then_duration_then_title_then_search_score`: album "Fixture Album", 3 tracks, 750 ms. Release A with `search_score: Some(100)` scores `1600`. Release B with `Some(90)` scores `290`.
- `similarity_bounds`: equal strings give 1.0, `("", "")` gives 1.0, and `("abc", "xyz")` gives 0.0.
- `files_match_by_id_then_position_then_duration`: against release A,
  - file 0 has `release_track_id` = track 3;
  - file 1 has disc None, track 1;
  - file 2 has no numbers, duration 251 ms, title "two";
  - file 3 has duration 10 000 ms.

  The result is `[Some(t3), Some(t1), Some(t2), None]`.
- `each_release_track_matches_at_most_once`: two files both at track 1 give `[Some(t1), None]` when the second's duration is far off.
- `missing_disc_number_is_disc_one_and_null_lengths_are_skipped` (Review Focus 4): release C with `length: None` on disc 2 track 1 does not panic, and a disc-less file at track 2 matches disc 1 track 2.
- `proposed_values_follow_the_spec_table`: all 16 values of release A track 2, including `"Fixture Artist feat. Guest"`, `track_total "3"`, `disc_total "1"`, `date "2001-05-14"` and `original_date "1999-02-01"`.
- `absent_or_empty_musicbrainz_values_are_not_proposed` (Review Focus 4): release B's track proposals lack `OriginalDate`. A `date: None` release lacks `Date`.
- `only_changed_fields_are_proposed_and_numbers_compare_numerically`: current `track_number "03"` against proposed `"3"` gives no change. Current album "Old Album" gives `FieldChange { field: Album, old: Some("Old Album"), new: "Fixture Album" }`.
- `most_common_release_ignores_blanks`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib tagging::matching`
Expected: compile errors.

- [ ] **Step 3: Implement `matching.rs` and `FieldChange`**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --lib tagging::matching`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src/tagging src/protocol.rs
git commit -m "Score releases and match files to release tracks"
```

---

### Task 7: Lookup engine, proposals and the worker

**Files:**
- Create: `src/tagging/engine.rs`, `src/tagging/proposal.rs`, `src/tagging/tests.rs`
- Modify: `src/tagging/mod.rs` (worker, `Handle`, requests), `src/tagging/test_support.rs` (`LibraryFixture`), `src/protocol.rs` (`TagCandidate`, `TrackProposal`, `TagProposal`, `ErrorCode::Lookup`)

**Interfaces:**
- Produces, in `src/protocol.rs`:
  ```rust
  pub enum ErrorCode { /* existing */ Lookup }   // wire "lookup"
  pub struct TagCandidate { pub release_id: String, pub title: String, pub artist: String, pub date: Option<String>,
      pub country: Option<String>, pub track_count: u32, pub score: u32 }
  pub struct TrackProposal { pub track_id: u64, pub file_name: String, pub track_number: Option<u32>, pub matched: bool,
      pub writable: bool, pub reason: Option<String>, pub removes_other_copies: bool,
      pub unwritable_fields: Vec<TagField>, pub changes: Vec<FieldChange> }
  pub struct TagProposal { pub proposal_id: String, pub release_id: String, pub candidates: Vec<TagCandidate>,
      pub tracks: Vec<TrackProposal> }
  ```
  These derive `Clone, Debug, Deserialize, Serialize, PartialEq, Eq`. `candidates` is sorted by score descending. `track_number` is the matched release position, else the stored track number.
- Produces, in `mod.rs` (Task 8 extends both enums):
  ```rust
  pub(crate) const MAX_ALBUM_TRACKS: usize = 500;
  pub(crate) const LOOKUP_DEADLINE: Duration = Duration::from_secs(50);
  pub(crate) struct LookupRequest { pub album_id: u64, pub library_revision: u64, pub release_id: Option<String>,
      pub fingerprint: bool, pub acoustid_key: Option<String> }
  pub(crate) enum TagRequest { Lookup(LookupRequest) }
  pub(crate) enum TagOutcome { Proposal(TagProposal) }
  #[derive(Debug)] pub(crate) struct Failure { pub code: ErrorCode, pub message: String, pub queue_revision: u64 }
  pub(crate) struct TaggingWorkers { handle: Handle, thread: Option<thread::JoinHandle<()>> }
  impl TaggingWorkers { pub(crate) fn start(database: PathBuf, library: LibraryHandle) -> Self; // live providers
      fn with_providers(database: PathBuf, providers: engine::Providers, library: Option<LibraryHandle>) -> Self;
      pub(crate) fn handle(&self) -> Handle; pub(crate) fn stop(&self); pub(crate) fn shutdown(self) }
  #[derive(Clone)] pub(crate) struct Handle { sender: mpsc::SyncSender<Work>, stop: Arc<AtomicBool>,
      library: Option<LibraryHandle> }   // stored now, used for re-index in Task 10
  impl Handle { pub(crate) async fn request(&self, request: TagRequest) -> Result<TagOutcome, Failure> }
  ```
  - One thread named `tagging-0`, behind `sync_channel(4)`.
  - Work is dropped when the reply is closed or the work is already cancelled.
  - A lookup's `Cancellation` gets `abandoned` set by a `CancelOnDrop` when its future is dropped (as `src/lyrics/mod.rs:63-68,182-211`).
  - A full queue gives `Failure { code: InvalidCommand, message: "tag lookups are busy, try again shortly" }`.
- Produces, in `proposal.rs`:
  ```rust
  pub(super) const PROPOSAL_TTL: Duration = Duration::from_secs(15 * 60);
  pub(super) const MAX_PROPOSALS: usize = 8;
  pub(super) struct ProposedFile { pub track_id: u64, pub path: PathBuf, pub stamp: FileStamp,
      pub values: BTreeMap<TagField, String> }                      // writable, changed fields only
  pub(super) struct Proposal { pub release_id: String, pub release_title: String, pub files: Vec<ProposedFile> }
  pub(super) struct Proposals { /* id → (created, Proposal) */ }
  impl Proposals { pub fn insert(&mut self, now: Instant, proposal: Proposal) -> String;  // uuid v4
      pub fn take(&mut self, now: Instant, id: &str) -> Option<Proposal> }
  pub(super) struct ReleaseCache { /* id → (fetched, Release), ≤ 32, 15 min */ }
  impl ReleaseCache { pub fn get(&mut self, now: Instant, id: &str) -> Option<Release>; pub fn put(&mut self, now: Instant, release: Release) }
  ```
- Produces, in `engine.rs`:
  ```rust
  pub(super) struct Providers { pub musicbrainz: Result<MusicBrainz, String>, pub acoustid: Result<AcoustId, String> }
  pub(super) struct Engine { /* read-only state DB, journal path + Option<Journal>, providers, proposals, releases */ }
  impl Engine {
      pub(super) fn open(database: &Path, providers: Providers) -> anyhow::Result<Self>  // journal: database.with_extension("tagging.sqlite")
      pub(super) fn lookup(&mut self, request: &LookupRequest, cancel: &Cancellation) -> Result<TagProposal, Failure>
  }
  ```
- Lookup algorithm (spec "Lookup", with decisions 6–9):
  1. **Validate:**
     - `release_id` must pass `is_mbid`, else `InvalidCommand "release_id must be a MusicBrainz ID"`.
     - `acoustid_key` must be 1–64 ASCII letters or digits, else `InvalidCommand "acoustid_key must be 1 to 64 letters or digits"`.
     - `fingerprint` without a key gives `InvalidCommand "fingerprint lookup needs an AcoustID key"`.
  2. **Read the album** in one read transaction.
     - The revision must match `library_state.revision`, else `StaleLibraryRevision "library revision is N"`.
     - Rows: `SELECT t.id,a.path,t.metadata FROM library_tracks t JOIN library_assets a ON a.id=t.asset_id JOIN library_roots r ON r.id=a.root_id WHERE t.album_id=?1 AND a.indexed=1 AND a.last_seen_scan>=r.completed_scan ORDER BY t.disc_number,t.track_number,t.id`.
     - No rows gives `InvalidCommand "unknown album"`. More than 500 gives `InvalidCommand "album has more than 500 tracks"`.
     - `AlbumFacts` come from `library_albums.title` and `album_artist` and the summed durations.
  3. **Candidates** (decision 6):
     - explicit: `[release_id]`;
     - fingerprint: run `fingerprint` per file, `lookup` per print, `rank_releases`. Votes go into `Evidence`. An empty ranking gives `Lookup "AcoustID has no match for these files"`.
     - stored ID: `[most_common_release(tracks)]` when it is `Some`;
     - search: the second query applies when the first is empty (decision 7). Hits go into `Evidence.search_score`.
     - No candidate gives `Lookup "MusicBrainz has no matching release"`.
  4. **Fetch and choose:** fetch every candidate through `ReleaseCache`, `score` each, and choose the best, or the explicit one.
  5. **Per file, in DB order:**
     - `writable_format` is `None` → `writable: false`, reason "Tyranny can't write tags to this format".
     - `read_tags` errors → reason `"Tyranny couldn't read this file's tags: {error}"`.
     - No `TrackRef` → `matched: false`, reason "No matching track on this release".
     - A changed field with a `read_only` copy → `writable: false`, reason "Has an old ID3 tag Tyranny can't edit".
     - Changed fields in `unwritable_fields_in(&tags)` go to `unwritable_fields` and out of `changes`. This covers the per-format deny list and, for ID3v2.3 files, `ID3V23_UNWRITABLE`.
     - `removes_other_copies` is `!tags.copies_of(changed fields).is_empty()`.
     - The `stamp` is taken when the file is read.
  6. **Store** the proposal (writable files with changes only), then reply.

  `cancel.check_lookup(deadline)` (from Task 4) runs before each network call and each file. Every `LookupError` maps to `ErrorCode::Lookup` with its Display. Each `Failure` fills `queue_revision` from `SELECT revision FROM control WHERE singleton=1` (0 on error), as `src/lyrics/mod.rs:281-287` does.
- `LibraryFixture` (cfg(test), sync):
  ```rust
  pub(super) struct LibraryFixture { pub root: PathBuf, pub database: PathBuf, pub album_id: u64, pub revision: u64, pub files: Vec<PathBuf> }
  impl LibraryFixture { pub fn new(files: &[(&str /*fixture or "tone10.wav"*/, &[(TagField, &str)])]) -> Self }
  ```
  It copies each file into `<root>/Album/`. `"tone10.wav"` means `write_tone_wav(.., 10)`. It then tags each copy with `tags::edit_tags`, plus album "Old Album" and album artist "Old Artist". A value given for an entry replaces these defaults. An entry ending `.mka` is instead created by the `ffmpeg` CLI with the same tags as `-metadata` (as `src/lyrics/tests.rs:52-91` does), because lofty cannot write Matroska. It creates the DB with `crate::store::Store::open`, scans it with `Library` on an internal tokio runtime, and reads the album ID and revision. Finally it shuts the library down.

- [ ] **Step 1: Write the failing tests (in `src/tagging/tests.rs`)**

MusicBrainz and AcoustID are `for_test(fixture.origin(), Duration::ZERO)` on routes serving the JSON fixtures.
- `lookup_by_stored_release_id_fetches_only_that_release`: 3 `tone.flac` copies tagged with release A's ID and track numbers 1–3.
  - `seen()` is exactly one target, the release A URL.
  - `candidates.len() == 1`, and every track is matched and writable.
  - The changes include `Album: Some("Old Album") → "Fixture Album"`.
- `lookup_by_search_scores_and_chooses_best`: three `tone.flac` copies with no stored IDs, each tagged with album "Fixture Album". The requests are the search, then A, then B. The chosen release is A. Candidate A scores 1600 (1000 + 300 + 200 + 100). B comes second with 290 (0 + 0 + 200 + 90), and its label fields match the fixture.
- `empty_search_retries_without_track_count_then_reports_no_match`: two search targets, the second without `tracks%3A`. The result is `Failure { code: Lookup, message: "MusicBrainz has no matching release" }`.
- `explicit_release_id_is_the_only_candidate` and `invalid_release_id_is_refused_before_http`.
- `lookup_by_fingerprint_ranks_acoustid_votes`: three `"tone10.wav"` files with key `KEY123`. There are three AcoustID requests, each containing `client=KEY123`.
  - Every file votes for B and A, so the vote ranking is B then A (Task 5 tests `rank_releases` order).
  - Candidates are sorted by score: A first (1192 = 1000 for 3 tracks + 92 for "Old Album" vs "Fixture Album" + 100 for votes), then B (192).
  - The chosen release is A.
- `fingerprint_requires_a_valid_key`: both messages above.
- `stale_revision_and_unknown_album_fail_before_network`: the codes are `StaleLibraryRevision` and `InvalidCommand "unknown album"`, and `seen()` is empty.
- `current_values_come_from_the_file_not_the_database`: a file with no title tag. The DB title is the file stem, but `changes` has `Title` with `old: None`.
- `unwritable_and_unmatched_files_say_why`:
  - Setup: three `tone.flac` copies tagged with release A's ID and track numbers 1–3, plus a `"tone.mka"` entry and a `"tone10.wav"` entry with no track number (10 s, so it matches no 250 ms release track).
  - The `.mka` row has reason "Tyranny can't write tags to this format".
  - The unmatched row has `matched: false` and reason "No matching track on this release".
- `musicbrainz_busy_stops_the_lookup`: a 503 gives `Lookup "MusicBrainz is busy, try again shortly"`, with exactly one request.
- `proposals_expire_after_15_minutes_and_evict_the_oldest`: `take(now + PROPOSAL_TTL + 1 s, id)` is `None`. After 9 inserts the first ID is gone.
- `abandoned_lookup_stops_the_worker`:
  - Setup: through `TaggingWorkers::with_providers`, on the search path (files without stored IDs). The first search reply has `delay: 2 s`. Drop the `request` future after 100 ms.
  - A second lookup (explicit `release_id` B) still completes.
  - `seen()` holds the first search and then only release B's fetch. No `/ws/2/release/1111…` (A) target appears, so the abandoned lookup made no follow-up fetch.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib tagging::tests`
Expected: compile errors.

- [ ] **Step 3: Implement `proposal.rs`, `engine.rs` (lookup only), the worker and `Handle` in `mod.rs`, and the protocol types**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --lib tagging`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src/tagging src/protocol.rs
git commit -m "Look up an album on MusicBrainz into a stored proposal"
```

---

### Task 8: Apply, revert and the edit list

**Files:**
- Modify: `src/tagging/engine.rs`, `src/tagging/mod.rs`, `src/tagging/tests.rs`, `src/protocol.rs` (`TagFileOutcome`, `TagFileResult`, `TagApplyResult`)

**Interfaces:**
- Consumes: `Proposals::take`, `safe_write::{stamp, write_safely, WriteError}`, `tags::read_tags`, `Journal`.
- Produces, in `src/protocol.rs`:
  ```rust
  #[serde(rename_all = "snake_case")] pub enum TagFileOutcome { Written, Skipped, Failed }
  pub struct TagFileResult { pub track_id: u64, pub file_name: String, pub outcome: TagFileOutcome, pub message: Option<String> }
  pub struct TagApplyResult { pub edit_id: Option<u64>, pub files: Vec<TagFileResult> }
  ```
- Extends `mod.rs`:
  ```rust
  pub(crate) struct ApplyRequest { pub proposal_id: String, pub track_ids: Vec<u64>, pub omit_fields: Vec<TagField> }
  pub(crate) enum TagRequest { Lookup(LookupRequest), Apply(ApplyRequest), Edits, Revert { edit_id: u64 } }
  pub(crate) enum TagOutcome { Proposal(TagProposal), Applied { result: TagApplyResult, written: Vec<PathBuf> },
      Edits(Vec<TagEditSummary>) }
  ```
  Apply and Revert work ignores `abandoned`. It checks only the worker `stop` flag, and only between files.
  Change Task 7's worker rule so that only `Lookup` work is dropped when its reply is closed or it is already cancelled. `Handle::request` sets `abandoned` through `CancelOnDrop` when its future is dropped. If the rule also covered Apply, an Apply still in the queue would be discarded. Apply, Revert and Edits work always runs, and its reply may go to a closed channel.
- Extends `engine.rs`:
  ```rust
  pub(super) fn apply(&mut self, request: &ApplyRequest, stop: &AtomicBool) -> Result<(TagApplyResult, Vec<PathBuf>), Failure>
  pub(super) fn revert(&mut self, edit_id: u64, stop: &AtomicBool) -> Result<(TagApplyResult, Vec<PathBuf>), Failure>
  pub(super) fn edits(&mut self) -> Result<Vec<TagEditSummary>, Failure>   // journal.recent(50)
  ```
  - **Apply:**
    - An empty `track_ids` gives `InvalidCommand "choose at least one file"`.
    - `take` returns `None` → `InvalidCommand "lookup expired, look the album up again"`.
    - A track ID that is not in `proposal.files` → `InvalidCommand "track {id} is not part of this lookup"`, with nothing written. `proposal.files` holds only writable files with changes. Decision 10: the proposal is still consumed.
    - Per requested file in proposal order:
      - no proposed values left after `omit_fields` → skipped "nothing to write";
      - `stamp ≠ proposal stamp`, or the file is gone → skipped "changed since the lookup".
    - Then `read_tags` fresh and build each `JournalFile`: `before` = primary values, `after` = the proposal values, `removed` = `copies_of(after keys)`.
    - If any file remains:
      - Open the journal. If it is unavailable: `Persistence "tag journal unavailable: …"`, and nothing is written.
      - `begin`, then `write_safely` each file and `set_status`.
      - `Changed` → skipped "changed since the lookup", and its journal status is `Failed`, so it is not counted and not revertible. `Failed(m)` → failed `m`, with status `Failed`.
    - `edit_id` is `None` when nothing reached `begin`. The returned paths are the written ones.
  - **Revert:**
    - Unknown → `InvalidCommand "unknown tag edit"`. Already reverted → `InvalidCommand "this tag edit was already reverted"`.
    - For files with status `Written` or `Pending`: if every `after` field still equals `read_tags(..).primary`, `write_safely` with `TagEdit { set: before, restore: removed }` and set the status `Reverted`. Otherwise skip with "changed since the edit".
    - Finally `mark_reverted`. The result's `edit_id` is `Some(edit_id)`.

- [ ] **Step 1: Write the failing tests**
- `apply_writes_proposal_values_and_journals_before_and_after`:
  - lofty reads back each proposal `new` value.
  - `Journal::load(edit_id)` holds the before and after values, with statuses `Written`.
  - The returned paths equal the 3 files.
  - A second `apply` with the same ID gives "lookup expired, look the album up again".
- `omitted_fields_are_left_unchanged_and_the_rest_written`:
  - Setup: 3 `tone.flac` copies tagged with release A's ID, track numbers 1–3 and title "Old Title", so the proposal changes `Title` and `Album`.
  - Apply with `omit_fields: [Title]`.
  - Each file's title still reads "Old Title", and `Album` reads "Fixture Album".
  - `Journal::load(edit_id)` has no `Title` key in any file's `before` or `after`.

  The wire-level refusal of client values is Task 10's test.
- `unknown_track_rejects_the_whole_apply`: no file changes on disk.
- `apply_skips_a_file_changed_since_the_lookup`: append a byte to one file. That file is skipped with "changed since the lookup"; the others are written.- `deleted_file_between_lookup_and_apply_is_skipped` (Review Focus 3).
- `unwritable_directory_fails_every_file_and_leaves_originals` (Review Focus 2): directory at 0555; every outcome is `Failed`, the originals are identical, and the journal statuses are `Failed`.
- `revert_restores_before_values_and_removes_added_fields`:
  - Setup: MP3 copies with ID3v1 copies of the title and no MB IDs. Apply, then revert.
  - The IDs are absent again and the title is back.
  - The ID3v1 copies are restored.
- `double_revert_is_refused`.
- `revert_skips_files_changed_since_the_edit`: change one file's title with `edit_tags` after the apply.
- `edits_lists_latest_first_with_counts`.
- `apply_is_not_cancelled_when_the_requester_goes_away`: through `Handle`, drop the future right after sending. Then poll `read_tags` until the files hold the new values (deadline 10 s).

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib tagging::tests`
Expected: compile errors or failures in the new tests.

- [ ] **Step 3: Implement `apply`, `revert`, `edits`, the new request kinds and the protocol types**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --lib tagging`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src/tagging src/protocol.rs
git commit -m "Apply and revert tag proposals"
```

---

### Task 9: Path-scoped Reindex

**Files:**
- Modify: `src/library_types.rs`, `src/library/mod.rs`, `src/library/db.rs`, `src/scanner.rs`, `src/protocol.rs` (the `From<LibraryReply>` arm), `src/library/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  // library_types.rs
  LibraryCommand::Reindex { paths: Vec<PathBuf> }
  LibraryReply::ReindexQueued { paths: usize }
  // library/mod.rs
  pub(crate) const MAX_PENDING_REINDEX: usize = 4096;
  enum ScanScope { Root, Paths }                    // new field `scope` on `Active`
  // scanner.rs
  impl Scanner { pub fn start_paths(paths: Vec<PathBuf>) -> Result<Self> }
  // library/db.rs
  pub(super) fn begin_paths(transaction: &Transaction<'_>) -> Result<u64>   // bumps scan_serial only; saves no status
  pub(super) fn finish_paths(connection: &mut Connection, serial: u64, status: &ScanStatus) -> Result<()>
  ```
  - **Scan status stays as it was.** A Reindex must leave `library_roots.last_scan` and `library_state.scan` exactly as they were. It may change only the library revision (bumped by installs and grouping, as in every scan) and track rows.
    - In `ScanScope::Paths` the scan's `ScanStatus` lives only in `Active.status`.
    - Today's code writes the status in `db::begin_scan`, `db::discover`, `db::next_batch`, `db::probed`, `db::finish`, `advance`'s `Enumerated` arm, `scan_tick`'s failure arm, `launch`'s failure arm and `run`'s shutdown arm. Each of these call sites skips `save_status`/`persist` for `Paths`. The `db` functions take a `persist_status: bool` (true for `Root`) to do this.
    - A failed Reindex logs `eprintln!("tyranny: re-index failed: {error:#}")` instead of saving a `Failed` status. The pending paths are then dropped.
    - A crash or shutdown mid-Reindex leaves no trace: the pending paths are in memory only, and `discard_old_work` removes their worklist rows. Those files keep their old metadata until the next rescan.
    - The client sees no scan progress for a Reindex.
  - `From<LibraryReply> for Response` maps `ReindexQueued` to `unreachable!("Reindex is internal to tagging and never reaches the wire")` (decision 14).
  - **`Coordinator::command(Reindex)`:** every path must be absolute and UTF-8, else `Invalid "reindex paths must be absolute UTF-8 paths"`. Paths are deduplicated into `pending_reindex: Vec<PathBuf>`. More than `MAX_PENDING_REINDEX` gives `Busy "too many files are waiting to be re-indexed"`. The reply is `ReindexQueued`. It never starts a scan directly.
  - **`Coordinator::run`:** when `active.is_none()` and `pending_reindex` is non-empty, it calls `reindex_step()`, before `upgrade_step`.
  - **`reindex_step`:**
    - Look up each pending path's `root_id` in `library_assets`; drop paths with no row.
    - Take the root of the first known path, and every pending path in that root.
    - `db::begin_paths`, `grouping::begin(root)`, `Scanner::start_paths`, `Active { scope: Paths, status: ScanStatus { root_id: Some(root), phase: Discovering, .. }, .. }`.
  - **`Scanner::start_paths`:**
    - It emits `Candidates`, 128 per batch, for paths that are regular files with a candidate extension. Each candidate gets a fingerprint as in `enumerate`.
    - Every other path is counted as an enumeration failure with its message.
    - `Enumerated { complete: false, .. }` follows. `complete` must be false, so grouping keeps `completed_scan` visibility.
    - Probing is unchanged.
  - **`advance()`:** at completion, `ScanScope::Root` calls `db::finish` as today. `ScanScope::Paths` calls `db::finish_paths`, which checks the worklist is empty and `total == processed`. It saves no status and never moves `completed_scan`.

- [ ] **Step 1: Write the failing tests (in `src/library/tests.rs`)**

They use the existing `flac`, `add`, `finished`, `tracks` and `albums` helpers. A Reindex does not change the scan phase, so `finished` cannot wait for it. Wait instead by polling `tracks` until the expected metadata appears (deadline 10 s).
- `reindex_updates_only_named_files_and_keeps_track_ids`:
  - Setup: two FLACs; rewrite one with `flac(path, &[("title","New")])`; send `Reindex { paths: [it] }`.
  - The reply is `ReindexQueued { paths: 1 }`.
  - Once the title is "New": the revision is greater, the track ID is unchanged, the other track is unchanged and visible, and `library_roots.completed_scan` is unchanged.
  - `library_roots.last_scan` and `library_state.scan` hold exactly the JSON they held before the Reindex.
- `reindex_during_a_scan_waits_for_the_scan`: `Rescan` then `Reindex` at once. Both complete, and the metadata is updated. The final `library_state.scan` is the Rescan's `Complete` status.
- `reindex_moves_an_album_to_release_identity`: add a `MUSICBRAINZ_ALBUMID` tag and reindex. The album ID changes; the track ID does not.
- `reindex_ignores_unknown_paths_and_rejects_relative_paths`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib library::tests::reindex`
Expected: compile errors.

- [ ] **Step 3: Implement as specified above**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --lib library`
Expected: PASS, including every existing library test.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src/library_types.rs src/library src/scanner.rs src/protocol.rs
git commit -m "Re-index named files without a root scan"
```

---

### Task 10: Protocol 13, routing and server tests

**Files:**
- Modify: `src/protocol.rs` (commands, responses, `VERSION = 13`, tests)
- Modify: `src/owner.rs` (owner and controller fields, routing, the `unreachable!` list at `:1622-1637`, shutdown)
- Modify: `src/tagging/mod.rs` (`TaggingWorkers::start(database, library)`; `Handle` reindexes after apply and revert)
- Modify: `src/server.rs:463` (discovery match)
- Modify: `tests/server_transport.rs`, `macos/Sources/TyrannyCore/WireModels.swift:6`, `scripts/playlist_gate.py`, `scripts/server_gate.py`, `scripts/smart_playlist_gate.py`, `SPIKE.md`

**Interfaces:**
- Produces, in `src/protocol.rs`:
  ```rust
  // Command
  MusicbrainzLookup { album_id: u64, library_revision: u64, release_id: Option<String>,
      #[serde(default)] fingerprint: bool, acoustid_key: Option<String> },
  ApplyTags { proposal_id: String, track_ids: Vec<u64>, #[serde(default)] omit_fields: Vec<TagField> },
  TagEdits,
  RevertTagEdit { edit_id: u64 },
  // Response
  TagProposal { proposal: TagProposal },
  TagApply { #[serde(flatten)] result: TagApplyResult },
  TagEdits { edits: Vec<TagEditSummary> },
  ```
  Wire shapes:
  - `{"kind":"tag_proposal","proposal":{…}}`;
  - `{"kind":"tag_apply","edit_id":3,"files":[…]}`;
  - `{"kind":"tag_edits","edits":[…]}`.
- Changes in `tagging/mod.rs`:
  - Remove the `#![allow(dead_code)]` attribute.
  - The re-index must not depend on the requester's future. A dropped future, a client disconnect or a server shutdown after the write must still re-index.
    - For Apply and Revert, when `Handle` has `Some(library)`, `Handle::request` uses `tokio::spawn` to start a detached task. That task awaits the worker's reply.
    - On `TagOutcome::Applied` with non-empty `written`, the task awaits `library.request(LibraryCommand::Reindex { paths: written })`.
    - The task then sends the outcome to the requester through a `oneshot`. If the requester is gone, that send fails, and this is harmless.
    - A Reindex failure does not change the reply. The task logs it with `eprintln!("tyranny: re-index after tag write failed: …")`.
  - Add `pub(crate) fn reply(outcome: Result<TagOutcome, Failure>) -> Reply`, which maps to `Response::{TagProposal, TagApply, TagEdits}` or `Reply::Error`.
- Changes in `owner.rs`:
  - `Owner { tagging: TaggingWorkers, .. }` and `Controller { tagging: TaggingHandle, .. }`, started after lyrics with `TaggingWorkers::start(database, library.clone())`. `shutdown` calls `stop` and then `shutdown`, as for lyrics.
  - `Controller::request` routes the four commands to `self.tagging.request(..)` and returns `Message::Reply { id, reply: tagging::reply(..) }`.
- `server.rs`: discovery becomes `Command::Artwork { .. } | Command::Lyrics { .. } | Command::MusicbrainzLookup { .. }`. Apply and revert are not discovery.

- [ ] **Step 1: Write the failing tests**
- In `src/protocol.rs`, `tag_commands_use_snake_case_wire_names_and_refuse_client_values`:
  - All four commands decode.
  - `musicbrainz_lookup` without `release_id`, `fingerprint` and `acoustid_key` decodes to `None`, `false` and `None`.
  - Adding `"path"` to a lookup is an error. Adding `"values"`, `"tags"` or `"path"` to `apply_tags` is an error.
  - A missing `album_id`, `library_revision`, `proposal_id`, `track_ids` or `edit_id` is an error.
  - The three responses serialize to the exact shapes above.
  - `serde_json::to_value(ErrorCode::Lookup) == "lookup"`.
- Change `assert_eq!(VERSION, 12)` to `13`.
- In `src/tagging/tests.rs`, `apply_through_handle_reindexes_written_paths` (the spec's Re-index test):
  - Setup: a live `Library` on the fixture DB and `TaggingWorkers::with_providers(.., Some(library.handle()))`.
  - After lookup and apply, poll `LibraryCommand::Tracks` until the album title is "Fixture Album".
  - The track IDs are unchanged and the revision is greater.
- In `src/tagging/tests.rs`, `dropped_apply_future_still_reindexes`: the same setup, but drop the apply `request` future right after sending. Polling `LibraryCommand::Tracks` still reaches album title "Fixture Album" (deadline 10 s).
- In `tests/server_transport.rs`, `tag_commands_answer_without_network`:
  - `tag_edits` gives `tag_edits` with `[]`.
  - A lookup with `library_revision: 0` gives `stale_library_revision`.
  - A lookup with the current revision and `album_id: 999` gives `invalid_command` "unknown album".
  - `apply_tags` with `proposal_id: "x"` and `track_ids: [1]` gives `invalid_command` "lookup expired, look the album up again".
  - `revert_tag_edit 99` gives `invalid_command` "unknown tag edit".

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --locked --lib -- protocol tagging && cargo test --locked --test server_transport tag_commands`
Expected: compile errors.

- [ ] **Step 3: Implement the protocol, owner, server and tagging changes. Bump every version reference in the Global Constraints list**

In `src/protocol.rs:1`, change "Version 12 local control protocol" to "Version 13 local control protocol". In `SPIKE.md:9`, change "Protocol v12 requires" to "Protocol v13 requires", and append the sentence: "Protocol v13 adds on-demand MusicBrainz tag lookup, reviewed tag writing into files, and revert." At `:89`, write "(currently v13)".

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test --locked --all-targets`
Expected: PASS, including `tests/swift_constants.rs`.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add src tests macos/Sources/TyrannyCore/WireModels.swift scripts SPIKE.md
git commit -m "Serve tag lookup commands in protocol 13"
```

---

### Task 11: TyrannyCore tag models and wire kinds

**Files:**
- Create: `macos/Sources/TyrannyCore/Tagging.swift`, `macos/Tests/TyrannyCoreTests/TaggingTests.swift`
- Modify: `macos/Sources/Tyranny/WireClient.swift` (`resultKind` at `:347` and `ResultHeader` at `:483`)

**Interfaces:**
- Produces, in `Tagging.swift` (all `public`, `Codable, Sendable, Equatable`; property names in snake_case as on the wire, matching `WireModels.swift` style):
  ```swift
  public enum TagField: String, Codable, Sendable, CaseIterable {
      case title, artist, album, albumArtist = "album_artist", trackNumber = "track_number", trackTotal = "track_total",
           discNumber = "disc_number", discTotal = "disc_total", date, originalDate = "original_date",
           musicbrainzRecordingID = "musicbrainz_recording_id", musicbrainzTrackID = "musicbrainz_track_id",
           musicbrainzReleaseID = "musicbrainz_release_id", musicbrainzReleaseGroupID = "musicbrainz_release_group_id",
           musicbrainzArtistID = "musicbrainz_artist_id", musicbrainzAlbumArtistID = "musicbrainz_album_artist_id"
      public var label: String   // "Title", "Artist", "Album", "Album Artist", "Track Number", "Track Total", "Disc Number",
                                 // "Disc Total", "Date", "Original Date", "MusicBrainz Recording ID", "MusicBrainz Track ID",
                                 // "MusicBrainz Release ID", "MusicBrainz Release Group ID", "MusicBrainz Artist ID",
                                 // "MusicBrainz Album Artist ID"
  }
  public struct TagCandidate: Identifiable { release_id, title, artist: String; date, country: String?; track_count, score: UInt32
      public var id: String { release_id }; public var label: String }
  public struct FieldChange { field: TagField; old: String?; new: String; public var line: String }
  public struct TrackProposal: Identifiable { track_id: UInt64; file_name: String; track_number: UInt32?; matched, writable: Bool;
      reason: String?; removes_other_copies: Bool; unwritable_fields: [TagField]; changes: [FieldChange]
      public var id: UInt64 { track_id }; public var notes: [String] }
  public struct TagProposal { proposal_id, release_id: String; candidates: [TagCandidate]; tracks: [TrackProposal] }
  public struct TagProposalResponse { public let proposal: TagProposal }
  public enum TagFileOutcome: String, Codable, Sendable { case written, skipped, failed }
  public struct TagFileResult { track_id: UInt64; file_name: String; outcome: TagFileOutcome; message: String?; public var line: String }
  public struct TagApplyResult { edit_id: UInt64?; files: [TagFileResult] }
  public struct TagEditSummary: Identifiable { edit_id: UInt64; created_at: Int64; release_title: String; file_count: UInt32; reverted: Bool
      public var id: UInt64 { edit_id }; public func rowText(timeZone: TimeZone) -> String }
  public struct TagEditsResponse { public let edits: [TagEditSummary] }
  public struct TagSelection: Equatable, Sendable {
      public init(_ proposal: TagProposal)
      public private(set) var selected: Set<UInt64>
      public func isEnabled(_ trackID: UInt64) -> Bool        // matched && writable && !changes.isEmpty
      public mutating func toggle(_ trackID: UInt64)          // no-op when disabled
      public var writeButtonTitle: String
      public func applyRequest() -> CommandRequest?           // nil when nothing is selected
  }
  public enum TagRequests {
      public static func lookup(albumID: UInt64, libraryRevision: UInt64, releaseID: String?, acoustIDKey: String?) -> CommandRequest
      public static let edits: CommandRequest
      public static func revert(editID: UInt64) -> CommandRequest
  }
  public enum TagLookupMessage { public static func text(for failure: ServerFailure) -> String }
  ```
  - `TagCandidate.label`: "Title — Artist · year · country · N tracks". Year is the first 4 characters of `date`. Missing parts are dropped together with their " · ". Use "1 track" for one.
  - `FieldChange.line`: "Label: old → new". A `nil` old is shown as "(none)".
  - `TrackProposal.notes`, in this order:
    - `!matched` → "No matching track on this release";
    - else `!writable` → `reason`;
    - else `changes.isEmpty` → "Already matches";
    - then each unwritable field → "can't write <Label> to this format";
    - then, if `removes_other_copies`, "Also removes old copies of these fields from other tags".
  - `TagFileResult.line`: "<file>: written", "<file>: skipped — <message>", or "<file>: failed — <message>".
  - `writeButtonTitle`: "Write Tags to N Files", or "Write Tags to 1 File".
  - `applyRequest()`: `CommandRequest("apply_tags", ["proposal_id": .string, "track_ids": .uints(in proposal order)])`. It sends no `omit_fields`.
  - `lookup`: `musicbrainz_lookup` with `album_id`, `library_revision`, `release_id` (only when non-nil), and `fingerprint: true` plus `acoustid_key` only when a key is given.
  - `rowText`: "yyyy-MM-dd HH:mm · <release_title> · N files" ("1 file"), plus " · reverted". It uses a `DateFormatter` with `en_US_POSIX` and the given time zone.
  - `TagLookupMessage.text`:
    - `"lookup"` → the message;
    - `"invalid_command"` whose message contains "lookup expired" → "This lookup expired. Look the album up again.";
    - `"stale_library_revision"` → "The library changed. Look the album up again.";
    - `"request_timeout"` → "MusicBrainz took too long to answer.";
    - anything else → the message.
- `WireClient.swift`:
  - `resultKind`: `"musicbrainz_lookup"` → `"tag_proposal"`; `"apply_tags", "revert_tag_edit"` → `"tag_apply"`; `"tag_edits"` → `"tag_edits"`.
  - `ResultHeader`: add `"tag_proposal", "tag_apply", "tag_edits"` to the snapshot-less list.

- [ ] **Step 1: Write the failing tests (swift-testing, `TaggingTests.swift`)**
- `decodesTheServerShapes`: decode the exact JSON of the Task 10 Rust response tests for all three kinds.
- `candidateLabels`: "Fixture Album — Fixture Artist · 2001 · GB · 3 tracks", "Fixture Album — Fixture Artist · 4 tracks" (no date or country), and a 1-track case ending "· 1 track".
- `changeLinesAndNotes`: "Album: Old Album → Fixture Album", "MusicBrainz Release ID: (none) → 11111111-1111-4111-8111-111111111111", and every `notes` case above.
- `selectionDefaultsAndToggling`: the default is exactly the enabled tracks. Toggling a disabled track does nothing. `writeButtonTitle` is "Write Tags to 3 Files", then "Write Tags to 1 File". An empty selection gives a nil `applyRequest()`.
- `applyRequestUsesProposalOrderAndNoValues`: the fields are exactly `proposal_id` and `track_ids`.
- `applyRequestForFiveHundredTracksFitsOneFrame` (Review Focus 5): 500 × `UInt64.max` through `RequestFrame.encode("apply_tags", fields:, id: UInt64.max)`, with `count <= RequestFrame.limit`.
- `lookupRequestFields`: key nil gives no `fingerprint` or `acoustid_key`; key "KEY123" gives `fingerprint: true` and `acoustid_key: "KEY123"`.
- `editRowText`: UTC, `created_at 1792575000` → "2026-10-21 09:30 · Fixture Album · 3 files", and the reverted suffix.
- `failureMessages`: each mapping above.

- [ ] **Step 2: Run them and see them fail**

Run: `(cd macos && swift test --filter TaggingTests)`
Expected: compile errors.

- [ ] **Step 3: Implement `Tagging.swift` and the `WireClient` kinds**

- [ ] **Step 4: Run them and see them pass**

Run: `(cd macos && swift test --filter TaggingTests)`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add macos/Sources/TyrannyCore/Tagging.swift macos/Tests/TyrannyCoreTests/TaggingTests.swift macos/Sources/Tyranny/WireClient.swift
git commit -m "Decode tag lookup replies in TyrannyCore"
```

---

### Task 12: Tagging model and the review sheet

**Files:**
- Create: `macos/Sources/Tyranny/Models/TaggingModel.swift`, `macos/Sources/Tyranny/Views/TagLookupSheet.swift`
- Modify: `macos/Sources/Tyranny/PlayerModel.swift` (`let tagging = TaggingModel()`, `tagging.attach(self)` beside `:110-115`, `var serverInstance: String? { connected ? wire.instanceID : nil }`)
- Modify: `macos/Sources/Tyranny/Shell/MainWindowView.swift` (a `.sheet(item:)` next to the Up Next prompt at `:83`)

**Interfaces:**
- Consumes: everything in Task 11; `PlayerModel.{endpoint, tokenPath, serverInstance, snapshot, connected}`.
- Produces:
  ```swift
  @MainActor @Observable final class TaggingModel {
      static let acoustIDKeyDefault = "acoustidAPIKey"
      struct Target: Identifiable, Equatable { let albumID: UInt64; let title: String; var id: UInt64 { albumID } }
      enum Phase: Equatable { case lookingUp, reviewing, applying, applied(TagApplyResult), failed(String) }
      var target: Target?                          // the sheet's item; setting nil closes and disconnects lookups
      private(set) var phase: Phase
      private(set) var proposal: TagProposal?
      private(set) var candidates: [TagCandidate]  // the picker's list: the first lookup's candidates for this target
      var selection: TagSelection?
      private(set) var edits: [TagEditSummary]
      private(set) var editsError: String?
      var hasAcoustIDKey: Bool
      func attach(_ player: PlayerModel)
      func begin(albumID: UInt64, title: String)
      func lookUp(releaseID: String?, fingerprint: Bool)
      func apply()
      func undo()                                  // revert the applied edit
      func refreshEdits() async
      func revert(_ editID: UInt64) async
  }
  ```
  - Two `WireClient(requestDeadlineSeconds: 60)`: `lookupWire` for `musicbrainz_lookup` only, and `editWire` for apply, revert and `tag_edits` (decision 12).
  - Each connects on demand to the player's endpoint and token. It refuses to continue when `wire.instanceID != player.serverInstance`, as `LyricsStore.swift:66-70` does.
  - A new lookup cancels the old one by `lookupWire.disconnect()` and a ticket check, as `LyricsStore.refresh` does.
  - A `stale_library_revision` failure retries once with the current `snapshot.library.revision`.
  - Errors become `.failed(TagLookupMessage.text(for:))`.
  - The key is read from `UserDefaults.standard.string(forKey: acoustIDKeyDefault)`. `lookUp` passes it to `TagRequests.lookup(acoustIDKey:)` only when `fingerprint == true`, and passes `nil` otherwise. `TagRequests.lookup` sends `fingerprint: true` whenever a key is given, so a key passed on every lookup would fingerprint every lookup.
  - **Candidates.** An explicit `release_id` makes the server return only that candidate. So the first lookup for a target (`releaseID == nil`) sets `candidates`. A fingerprint lookup without a `releaseID` also replaces them. A re-run with a picked `releaseID` keeps `candidates` and replaces only `proposal`. The picker always lists `candidates` and selects `proposal.release_id`.
  - After apply or undo, call `refreshEdits()`.
- Sheet (`TagLookupSheet`, width 640):
  - Title "Look Up on MusicBrainz" and the album title.
  - `.lookingUp`: `ProgressView` plus "Asking MusicBrainz…".
  - `.reviewing`: a `Picker("Release")` of `candidates[].label` (the model's kept list, not `proposal.candidates`). A new pick calls `lookUp(releaseID:, fingerprint: false)`.
  - Rows: a toggle (disabled when `!isEnabled`), the track number, the file name, the `changes[].line` lines and the `notes` (secondary style).
  - Buttons: "Identify by Audio Fingerprint" (only when `hasAcoustIDKey`), "Cancel", and `selection.writeButtonTitle` (default action, disabled when nothing is selected).
  - `.applied`: the `files[].line` lines, plus "Undo" and "Done".
  - `.failed`: the message, plus "Try Again" and "Cancel".
  - Accessibility identifiers: `tag-lookup-sheet`, `tag-lookup-progress`, `tag-lookup-release-picker`, `tag-lookup-row-<trackID>`, `tag-lookup-fingerprint`, `tag-lookup-cancel`, `tag-lookup-write`, `tag-lookup-undo`, `tag-lookup-error`.

- [ ] **Step 1: Implement `TaggingModel`, `TagLookupSheet`, and the `PlayerModel` and `MainWindowView` wiring**

The logic under test lives in TyrannyCore (Task 11). The app target has no test target, and the spec marks the native sheet smoke as not testable here.

- [ ] **Step 2: Build with warnings as errors**

Run: `(cd macos && swift build -Xswiftc -warnings-as-errors)`
Expected: success.

- [ ] **Step 3: Run all gates, then commit**

`client_gate.py --model-only` must stay green: the sheet is never presented without a target.

```bash
git add macos/Sources/Tyranny
git commit -m "Add the tag lookup review sheet"
```

---

### Task 13: Album menu entry points and `:musicbrainz`

**Files:**
- Modify: `macos/Sources/TyrannyCore/CommandLanguage.swift` (`CommandID.musicbrainz`, registry)
- Modify: `macos/Tests/TyrannyCoreTests/CommandLanguageTests.swift`
- Modify: `macos/Sources/Tyranny/CommandDispatcher.swift`
- Modify: `macos/Sources/Tyranny/Albums/AlbumGridItem.swift`, `macos/Sources/Tyranny/Albums/AlbumGridView.swift:393-397`
- Modify: `macos/Sources/Tyranny/Lists/TrackTableInput.swift`, `macos/Sources/Tyranny/Lists/VirtualTrackTable.swift`

**Interfaces:**
- Consumes: `TaggingModel.begin(albumID:title:)`.
- Produces:
  - `CommandID.musicbrainz = "musicbrainz"`, with registry entry `.init(.musicbrainz, "Look up the selected album on MusicBrainz.")`.
  - `AlbumGridItem.onLookUp: (() -> Void)?`. `AlbumGridCellView.menu(for:)` returns an `NSMenu` holding "Look Up on MusicBrainz…" (identifier `album-menu-musicbrainz`) when it shows an album, else `nil`.
  - `ActivatingTableView.gutterMenu: ((Int) -> NSMenu?)?`. `menu(for:)` uses it when `column(at:)` is the `AlbumBlockCell.columnID` column, and otherwise falls back to `contextMenu`.
- Behaviour:
  - **Grid:** `AlbumGridView.configure` sets `cell.onLookUp = { player.tagging.begin(albumID: album.id, title: album.title) }`. The item is disabled when `!player.connected`.
  - **Grouped lists:** `VirtualTrackTable` sets `gutterMenu` for rows where `layout.albumKey(atDisplayIndex:)` is non-nil, with the title taken from that album's facts (first line, else "Album").
  - **Command:** `.musicbrainz` calls `requireConnection()`.
    - In a list it uses `list.firstSelectedTrack` (`album_id`, `album`).
    - In Albums it uses the expansion's first selected track, else `selectedAlbum()`.
    - Otherwise it throws "Select an album first.".
    - It then calls `player.tagging.begin`.

- [ ] **Step 1: Write the failing test**

Add `("musicbrainz", .musicbrainz, [])` to `parsesEverySemanticActionWithItsArguments` in `CommandLanguageTests.swift`.

- [ ] **Step 2: Run it and see it fail**

Run: `(cd macos && swift test --filter CommandLanguageTests)`
Expected: compile error (no `.musicbrainz`).

- [ ] **Step 3: Implement the command, the dispatcher case and both menus**

- [ ] **Step 4: Run it and see it pass**

Run: `(cd macos && swift test --filter CommandLanguageTests)`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add macos
git commit -m "Open tag lookup from album menus and :musicbrainz"
```

---

### Task 14: Settings: AcoustID key and recent tag edits

**Files:**
- Modify: `macos/Sources/TyrannyCore/Tagging.swift` (`AcoustIDKey`), `macos/Tests/TyrannyCoreTests/TaggingTests.swift`
- Modify: `macos/Sources/Tyranny/Shell/SettingsView.swift` (`MusicFoldersPane`; grow the frame from height 430 to 640, wrapping the content in a `ScrollView` if it overflows)
- Modify: `macos/Sources/Tyranny/Models/TaggingModel.swift` (read the key through `AcoustIDKey`)

**Interfaces:**
- Produces:
  ```swift
  public enum AcoustIDKey {
      /// Trimmed key, or nil when empty or not 1–64 ASCII letters/digits (the server's rule).
      public static func normalized(_ text: String) -> String?
  }
  ```
- Settings → Library, below Music Folders:
  - An "AcoustID API key" `TextField` bound to `@AppStorage(TaggingModel.acoustIDKeyDefault)`, with identifier `settings-acoustid-key`.
  - Below it, the text "Get a key at acoustid.org/new-application" with `.textSelection(.enabled)`.
  - When the field is non-empty and `normalized` is nil, the orange caption "Use the letters and digits of your AcoustID application key.".
  - "Recent Tag Edits": up to 50 rows of `rowText(timeZone: .current)`, each with a "Revert" button (identifier `tag-edit-revert-<id>`), disabled when reverted or disconnected.
  - `.task { await model.tagging.refreshEdits() }` loads the rows. `editsError` shows when set.
  - With no rows, show "No tag edits yet".

- [ ] **Step 1: Write the failing test**

`acoustIDKeyNormalization`: `"  ab12CD  "` gives `"ab12CD"`. `""`, `"has space"`, `"ü"` and 65 × `"a"` give `nil`. 64 × `"a"` is accepted.

- [ ] **Step 2: Run it and see it fail**

Run: `(cd macos && swift test --filter TaggingTests)`
Expected: compile error.

- [ ] **Step 3: Implement `AcoustIDKey` and the Settings section, and use `normalized` in `TaggingModel.hasAcoustIDKey` and in the lookup**

- [ ] **Step 4: Run it and see it pass**

Run: `(cd macos && swift test --filter TaggingTests)`
Expected: PASS.

- [ ] **Step 5: Run all gates, then commit**

```bash
git add macos
git commit -m "Add the AcoustID key and recent tag edits to Settings"
```

---

## Spec Coverage

| Spec item | Task |
|---|---|
| Module, worker thread `tagging-0`, read-only DB, `<db>.tagging.sqlite`, 200 kept | 3, 7 |
| Providers: TLS, no redirects, timeouts, cap, UA, 1/s and 3/s gates, 503/Retry-After, `for_test` | 4, 5 |
| Lookup steps 1–6, fields table, file-read current values, 15-min proposal | 6, 7 |
| Apply steps 1–5, alias and other-tag removal, journal before first write, re-index, per-file results | 1, 2, 8, 10 |
| Writable formats and per-field deny list decided by fixtures | 1 |
| Revert, double revert, `tag_edits` latest 50 | 3, 8 |
| `Reindex` reusing the finish path, queued during a scan, track IDs kept | 9, 10 |
| Protocol 13, four commands, `ErrorCode::Lookup`, discovery cancellation | 7, 10 |
| Client entry points, review sheet, Settings key and edits, TyrannyCore pure parts | 11–14 |
| Verification list (unit, round-trip, safety, re-index, Swift, gates) | 1, 2, 4–11, every task's gates |
| Not testable here: live musicbrainz.org lookup, native sheet smoke | the final report says so; one live lookup by hand only if the network allows |
