# Format and Gapless Regression Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pin the 2026-10-20 spike results with two Rust integration suites: every encodable scanner format indexes and decodes, and playback stays gapless for files that carry gapless data.

**Architecture:** One committed Python script generates small fixtures into `tests/fixtures/` with local encoders and records them in a manifest. `tests/ffmpeg_formats.rs` indexes and decodes the format fixtures through the real `Library` and `Decoder`. `tests/gapless.rs` checks per-track lengths, lossless bit-exactness and player concatenation through `Decoder` and an unclocked capture `Player`.

**Tech Stack:** Rust 2024 integration tests (`cargo test`, tokio for async library calls), ffmpeg-next 9, Python 3 stdlib script shelling out to Homebrew `ffmpeg`, `lame`, `afconvert`.

**Spec:** `docs/superpowers/specs/2026-10-20-format-and-gapless-tests-design.md`

## Global Constraints

- Tests never encode at test time; they pass when linked against the release FFmpeg (built with `--disable-encoders`).
- No wall-clock assertions and no real audio device: `OutputConfig::Capture { path, clocked: false }`.
- Fixtures total ≤ 3 MB, checked in under `tests/fixtures/`.
- Engine rate `tyranny_likes_smoke::SAMPLE_RATE` = 48000. Int16 → f32 is `value as f32 / 32768.0` (as `tests/playback.rs` asserts).
- Gapless track lengths: 48 kHz `[24011, 26003, 25013, 24989]`; 44.1 kHz `[22063, 23887, 22979, 22961]`.
- "No local encoder" extensions: `amr, awb, ape, dff, dsf, mpc, shn, spx, tak`. (Amended
  2026-10-20, TSK-aware-yarn: all but tak now have fixtures. The manifest's `no_local_encoder`
  list became `formats.exceptions`, entries `{"extension", "reason"}`; see the spec.)
- No change to trimming, the resampler, or `scripts/build_ffmpeg.py` and other release packaging scripts.
- Never stage or modify the user's uncommitted work: `.gitignore`, `SPIKE.md`, `SPIKE_RESULTS.md`, `macos/Info.plist`, `macos/Sources/TyrannyLikesSmoke/ManagedServer.swift`, `scripts/build_macos.sh`, `scripts/macos_dependencies.py`, `scripts/test_macos_dependencies.py`, `scripts/build_ffmpeg.py`, `scripts/build_release_server.sh`, `scripts/build_dmg.sh`, `scripts/build_icon.sh`, `icons/`, `justfile`, `macos/Resources/`. Stage by explicit path only.
- Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

## Review Focus

1. A decode that errors part-way (e.g. "Invalid data found" after a few frames) must fail the format test, not pass on a partial count. Test: `every_format_fixture_decodes_to_eof` asserts `read` never returns `Err` and the count reaches ≥ 90 % (Task 2).
2. A fixture the scanner skips (extension not on the list) must fail, not silently shrink the indexed set. Test: `every_format_fixture_indexes_without_failures` asserts indexed track count == fixture count (Task 2).
3. A player that drops or duplicates samples only at the switch to a *different-codec* next track would pass same-set tests; the spec does not demand mixed runs, so this stays out of scope — noted, no test.
4. A regenerated fixture set with a different encoder version must not pass silently against stale expectations: tests read lengths from `manifest.json`, not hard-coded copies, except the spec's fixed boundary lengths, which the test cross-checks against the manifest. Test: `manifest_matches_the_spec_lengths` (Task 3).
5. Running `cargo test` from a different working directory must still find fixtures: build paths from `env!("CARGO_MANIFEST_DIR")` (Tasks 2, 3).

---

### Task 1: Fixture script, fixtures and manifest

**Files:**
- Create: `scripts/make_test_fixtures.py`
- Create: `tests/fixtures/manifest.json`, `tests/fixtures/formats/tone.<ext>` (27 files), `tests/fixtures/gapless/<set>/t1..t4.<ext>`

**Interfaces:**
- Produces `tests/fixtures/manifest.json` with this shape (Tasks 2 and 3 parse it with `serde_json::Value`):

```json
{
  "encoders": {"ffmpeg": "<ffmpeg -version first line>", "lame": "<lame --version first line>", "afconvert": "/usr/bin/afconvert"},
  "signal": {"description": "...", "amplitudes": [...], "phases": [...]},
  "formats": {"no_local_encoder": ["amr","awb","ape","dff","dsf","mpc","shn","spx","tak"],
              "files": [{"extension": "wav", "file": "formats/tone.wav", "codec": "pcm_s16le", "rate": 48000, "seconds": 0.25}]},
  "gapless": {"sets": [{"name": "wav48", "rate": 48000, "lossless": true, "files": ["gapless/wav48/t1.wav", "..."],
                        "lengths": [24011, 26003, 25013, 24989], "encoder": "ffmpeg pcm_s16le"}]}
}
```

- Set names exactly as the spec table: `wav48 flac48 alac48 mp3cbr48 mp3vbr48 mp3lame48 aac48 aacapple48 opus48 mp3notag48 wav44 flac44 mp3lame44 aacapple44`. `lengths` are source lengths in frames at the set's own rate.

- [ ] **Step 1: Write the script.** Python 3 stdlib only. Generate the chirp-sum signal (spec "Signal"; reuse `target/spikes/ruddy-ape/spike.py` `gen` and `target/spikes/aware-yarn/make_fixtures.sh` as references — read them, do not import them) as int16 WAV sources in a temp dir, split at the spec lengths, encode each set and each format fixture with the encoder named in the spec table, write the manifest. Fail with a clear message if `ffmpeg`, `lame` or `afconvert` is missing, or if any of the 27 format extensions cannot be produced. Usage: `python3 scripts/make_test_fixtures.py` regenerates `tests/fixtures/` in place (removing stale files under it first).

- [ ] **Step 2: Run it and check the output.**

Run: `python3 scripts/make_test_fixtures.py && du -sh tests/fixtures && ls tests/fixtures/formats | wc -l`
Expected: exits 0; total ≤ 3.0 MB; 27 format files.

- [ ] **Step 3: Check determinism.** Run the script a second time and compare `shasum -a 256` of every file before and after. Record in the report which encoders (if any) are non-deterministic; that is acceptable only if the decoded lengths are unchanged (the manifest is identical).

- [ ] **Step 4: Spot-check the gapless data with ffprobe.** For `mp3cbr48/t1.mp3` the trace shows the LAME tag skip (`-v trace … | grep -i "injecting skip"`), for `aacapple48/t1.m4a` it shows `iTunSMPB`, and `ffmpeg -i <track> -f f32le -ac 2 -ar 48000 -` decodes each 48k tagged track to exactly its manifest length × 8 bytes. Record the commands and results in the report.

- [ ] **Step 5: Commit**

```bash
git add scripts/make_test_fixtures.py tests/fixtures
git commit -m "Generate format and gapless test fixtures"
```

---

### Task 2: Public extension list and `tests/ffmpeg_formats.rs`

**Files:**
- Modify: `src/scanner.rs:372-384` (`is_candidate` uses the public list)
- Modify: `src/library_types.rs` (add the public constant)
- Create: `tests/ffmpeg_formats.rs`

**Interfaces:**
- Consumes: Task 1's `tests/fixtures/manifest.json` (`formats.files[].file`, `formats.files[].extension`, `formats.no_local_encoder`).
- Produces: `pub const AUDIO_EXTENSIONS: &[&str]` in `tyranny_likes_smoke::library_types`, same 36 entries and order as today's private `EXTENSIONS`.

- [ ] **Step 1: Write the failing tests** in `tests/ffmpeg_formats.rs` (doc comment at the top: what it guards, and the release-prefix command from the spec). Fixture root: `Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")`.
  - `scanner_extensions_are_all_covered`: `AUDIO_EXTENSIONS` as a set == fixture extensions ∪ `no_local_encoder`, and the two are disjoint. On failure, print the missing and extra extensions.
  - `every_format_fixture_decodes_to_eof`: for each fixture, `Decoder::open(path, GainMode::Off)`, `read` into a `BLOCK_FRAMES`-sized (or 4096) buffer until it returns 0; any `Err` fails with the file name and error; `total >= (0.9 * 0.25 * 48000.0) as usize`. Collect all failures and assert once, so one run lists every broken format.
  - `every_format_fixture_indexes_without_failures` (`#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`): copy the format fixtures into a scratch dir (pattern: `Scratch` in `tests/playback.rs`), `Library::start(scratch db path)`, add the scratch dir as a root through `library.handle().request(…)` with the add-root command the server uses (`src/library/mod.rs` `add_root`; find the command type), poll `library.state()` every 20 ms (timeout 30 s) until `scan.phase == ScanPhase::Complete`; assert `scan.failures == 0`, `scan.skipped == 0`, and `state().tracks == fixture count`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test ffmpeg_formats`
Expected: FAIL to compile (`AUDIO_EXTENSIONS` not found).

- [ ] **Step 3: Add `AUDIO_EXTENSIONS`** to `src/library_types.rs` and make `scanner::is_candidate` use it; update the scanner header comment if it lists the extensions (keep the list in one place; the comment can say "see `AUDIO_EXTENSIONS`").

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --test ffmpeg_formats` then `cargo test`
Expected: PASS; full suite still passes.

- [ ] **Step 5: Mutation check (report only, do not commit).** Temporarily remove `"wav"` from `AUDIO_EXTENSIONS` → `scanner_extensions_are_all_covered` and the index test fail. Temporarily make `Decoder::read` return an error after the first block for `.ogg` paths (or rename one fixture to `.txt` in the scratch copy) → the matching test fails. Revert; record the outputs.

- [ ] **Step 6: Commit**

```bash
git add src/library_types.rs src/scanner.rs tests/ffmpeg_formats.rs
git commit -m "Test that every encodable scanner format indexes and decodes"
```

---

### Task 3: `tests/gapless.rs`, audio-probe comment, release-prefix run

**Files:**
- Create: `tests/gapless.rs`
- Modify: `src/bin/audio-probe.rs:1-2` (comment only)

**Interfaces:**
- Consumes: Task 1's manifest (`gapless.sets[]`: `name`, `rate`, `lossless`, `files`, `lengths`).

- [ ] **Step 1: Write the tests** (doc comment at the top: what it guards, why the three ffmpeg-encoder cases are absent, the release-prefix command). Helpers: `decode_all(path) -> Vec<Frame>` (Decoder, `GainMode::Off`, to EOF, panics with the file name on error) and `capture_run(paths) -> Vec<Frame>` (Player over the paths, unclocked capture into a scratch file, pumped to completion like `finish` in `tests/playback.rs`, then the capture read back).
  - `manifest_matches_the_spec_lengths`: every `*48` set has lengths `[24011, 26003, 25013, 24989]`, every `*44` set `[22063, 23887, 22979, 22961]`; the 14 set names are exactly the spec's.
  - `tagged_48k_tracks_decode_to_their_source_length`: every 48k set except `mp3notag48`: `decode_all(track).len() == length`, per track. Collect all mismatches (set, track, expected, actual) and assert once.
  - `resampled_44k_tracks_decode_within_one_frame`: every 44.1k set: `|len − L × 48000 / 44100| ≤ 1` (compute in f64).
  - `untagged_mp3_keeps_its_encoder_delay`: `mp3notag48` tracks decode to `≥ L + 1105` frames. Comment: pins the no-codec-constant policy (`src/decoder.rs:6-7`), not a wish.
  - `lossless_decodes_are_bit_exact`: `wav48` decode equals the WAV file's int16 samples / 32768.0 (parse the data chunk; the spike's script writes plain 16-bit PCM); `flac48` and `alac48` decodes equal the `wav48` decode, per track.
  - `player_run_equals_the_tracks_end_to_end`: every 48k set: `capture_run(files) == concat(decode_all(file) for file)`, exactly. Every 44.1k set: total length within ±4 frames, and equal sample for sample outside ±64 frames of each boundary (boundary positions = cumulative per-track decoded lengths). Comment points at the deferred swr-carry follow-up.

- [ ] **Step 2: Run to verify they pass** (these pin existing behaviour; RED is the mutation check in Step 3)

Run: `cargo test --test gapless`
Expected: PASS.

- [ ] **Step 3: Mutation check (report only, do not commit).** Temporarily drop the final decoded frame of each track in `Decoder` (or skip the first 32 frames) → the length and concatenation tests fail. Revert; record the output.

- [ ] **Step 4: Update the audio-probe comment.** Replace "no gapless claim is made without measuring PCM" with a pointer to `tests/gapless.rs`, keeping the first sentence's meaning.

- [ ] **Step 5: Run both suites against the release prefix.** Prefix: the directory under `target/native/` named `ffmpeg-9.0.2-arm64-*`. Run the spec's command (`PKG_CONFIG_LIBDIR=<prefix>/lib/pkgconfig CARGO_TARGET_DIR=target/release-ffmpeg cargo test --test ffmpeg_formats --test gapless`). If the test binaries need `DYLD_LIBRARY_PATH=<prefix>/lib` to run, add it to the command in both doc comments. Confirm with `otool -L` on one test binary that it links the prefix dylibs. Expected: both suites PASS. Do not modify anything under `target/native/`.

- [ ] **Step 6: Full gates**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean; all pass.

- [ ] **Step 7: Commit**

```bash
git add tests/gapless.rs src/bin/audio-probe.rs tests/ffmpeg_formats.rs
git commit -m "Test that playback stays gapless for files with gapless data"
```
