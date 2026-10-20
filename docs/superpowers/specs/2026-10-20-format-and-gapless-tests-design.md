# Format and Gapless Regression Tests

Date: 2026-10-20. Scope: Rust integration tests, their fixtures, a fixture script, and two tiny
source changes. Tasks: TSK-aware-yarn-d04hz, TSK-ruddy-ape-g6sjj.

The user skipped the design questions ("skip the questions and start"). Every decision below is
Claude's call unless it quotes REQUIREMENTS.md or a spike finding.

## Intent

Two spikes on 2026-10-20 measured things that no test protects:

- **Formats (TSK-aware-yarn):** every scanner extension that local encoders can produce scans and
  decodes, with both Homebrew ffmpeg and the pinned release FFmpeg prefix. A scan can succeed while
  decoding fails ("Decoder not found"), so a scan test alone is not enough.
- **Gapless (TSK-ruddy-ape):** the player is already gapless whenever the file carries gapless
  data. Its PCM is bit-identical to decoding each track alone and concatenating. libavcodec does
  the trimming (SKIP_MANUAL off, `src/decoder.rs:3-7`).

These tests pin both results, so that a future change to the decode path, to the player's track
switch, or to the FFmpeg build fails loudly instead of silently adding gaps or losing formats.

## Goals

- `tests/ffmpeg_formats.rs`: one short fixture per encodable scanner extension. Each one indexes
  through the real library path with zero failures, and decodes through `Decoder` to at least
  90 % of its expected frame count with no error.
- The format test also fails when someone adds an extension to the scanner without a fixture or
  an entry in an explicit "no local encoder" list.
- `tests/gapless.rs`: per-track decoded lengths equal the source lengths for every tagged format,
  lossless decodes are bit-exact, and the player's capture of a 4-track run is bit-identical to the
  per-track decodes laid end to end.
- One committed script regenerates every fixture deterministically and records encoder versions.
- A documented command runs both suites against the release FFmpeg prefix.

## Non-goals

- No change to trimming or to the resampler. The spike's optional "carry the swr context across
  tracks" fix is a separate task; the 44.1 kHz assertions allow its ±1 sample until then.
- No content sniffing (the spike judged it not worth it).
- No edits to `scripts/build_ffmpeg.py` or the other release packaging scripts. They are the
  user's uncommitted work in progress.
- No fixtures for extensions without a local encoder or muxer: amr, awb, ape, dff, dsf, mpc, shn,
  spx, tak. They are named in the test's "no local encoder" list instead.
- No automated release-prefix run in `cargo test`. The prefix is built by the user's uncommitted
  script, so the run is a documented manual command for now.

## Anti-goals

- Tests must not depend on encoders at test time. The release FFmpeg is built with
  `--disable-encoders`, and the tests must pass when linked against it.
- Tests must not be flaky: no wall-clock timing, no real audio device. Use the unclocked capture
  output.
- Fixtures must stay small: 3 MB total at most, checked in as ordinary files.

## Fixtures

All fixtures are checked in under `tests/fixtures/` and produced by
`scripts/make_test_fixtures.py` (Python 3, standard library only; it shells out to the encoders).
The script is deterministic for a given set of encoder versions, and writes
`tests/fixtures/manifest.json` with the encoder versions it used (`ffmpeg -version` first line,
`lame --version` first line, `afconvert` presence) and the source lengths.

### Signal

The spike's reference: stereo int16, the sum of three chirps (log 40→2000 Hz, log 3000→250 Hz,
lin 700→1300 Hz) with a per-channel phase offset. A pure sine is rejected because it makes
alignment ambiguous. The exact amplitudes and phases are the builder's call; they go in the
script and the manifest.

### `tests/fixtures/formats/`

One 0.25 s, 48 kHz (44.1 kHz where the codec requires it) stereo file per encodable extension,
named `tone.<ext>`, covering the 27 extensions the spike could produce: aac, ac3, aif, aiff, aifc,
alac, au, caf, dts, flac, m4a, m4b, mka, mov, mp2, mp3, mp4, oga, ogg, opus, ra, tta, wav, wave,
webm, wma, wv. Each uses the codec and container the spike used for that extension
(`target/spikes/aware-yarn/` holds its generator; the manifest records the choice).
If an extension in that list cannot be produced, the script fails, naming it; the builder then
moves it to the "no local encoder" list and records why in the manifest.

### `tests/fixtures/gapless/`

The signal split into 4 tracks at sample-exact boundaries that are not codec-frame multiples.
At 48 kHz the track lengths are 24011, 26003, 25013 and 24989 frames (about 2.08 s in total).
At 44.1 kHz they are 22063, 23887, 22979 and 22961 frames.

| Set | Rate | Files | Encoder |
|---|---|---|---|
| `wav48`, `flac48`, `alac48` | 48k | `.wav`, `.flac`, `.m4a` | ffmpeg |
| `mp3cbr48`, `mp3vbr48` | 48k | `.mp3` with the LAME tag | ffmpeg libmp3lame (192k CBR; `-q:a 2`) |
| `mp3lame48` | 48k | `.mp3` | `lame` CLI 192k CBR |
| `aac48` | 48k | `.m4a` (elst) | ffmpeg native aac |
| `aacapple48` | 48k | `.m4a` (iTunSMPB) | `afconvert -f m4af -d aac -b 192000` |
| `opus48` | 48k | `.opus` | ffmpeg libopus 160k |
| `mp3notag48` | 48k | `.mp3`, no Xing/LAME tag | ffmpeg libmp3lame `-write_xing 0` |
| `wav44`, `flac44` | 44.1k | `.wav`, `.flac` | ffmpeg |
| `mp3lame44` | 44.1k | `.mp3` | `lame` CLI |
| `aacapple44` | 44.1k | `.m4a` | `afconvert` |

ffmpeg's `aac_at` m4a, ffmpeg's native Vorbis and ffmpeg's libmp3lame at 44.1 kHz are left out:
the spike showed their gapless data is wrong in the file, so they would test the encoder, not us.

## Assertions

### Formats

For each fixture in `tests/fixtures/formats/`:

1. Index the directory through `tyranny::library::Library` (the same scanner + metadata probe the
   server uses). The scan completes with `failures == 0`, and every fixture is a visible track.
2. Open it with `Decoder::open(path, GainMode::Off)` and read until EOF. No error, and the frame
   count is at least 90 % of `0.25 s × 48000` (the engine rate; the decoder resamples).
3. The set of scanner extensions equals the fixture extensions plus the "no local encoder" list
   (amr, awb, ape, dff, dsf, mpc, shn, spx, tak). To make this checkable, move the scanner's
   private `EXTENSIONS` list (`src/scanner.rs:376`) to a public constant that the scanner keeps
   using (location and name are the builder's call, for example
   `tyranny::library_types::AUDIO_EXTENSIONS`).

Failures name the file and the ffmpeg error, so a release-prefix run points straight at the
missing decoder or demuxer.

### Gapless

Engine rate is 48 kHz (`tyranny::SAMPLE_RATE`). For each set:

- **Per-track length:** decode each track alone with `Decoder` (`GainMode::Off`) to EOF.
  - 48k sets except `mp3notag48`: length equals the source length exactly.
  - 44.1k sets: `|length − L × 48000 / 44100| ≤ 1`.
  - `mp3notag48`: characterisation. Each track decodes to at least `L + 1105` frames. The comment
    says this pins the policy of no codec-constant trimming (`src/decoder.rs:6-7`), not a wish.
- **Lossless bit-exactness:** `flac48` and `alac48` decodes equal the `wav48` decode, sample for
  sample. The `wav48` decode equals the int16 samples in the WAV files, scaled to f32 exactly as
  the decoder does (the builder reads the decoder's conversion and matches it).
- **Player concatenation:** for every set, a `Player` over the 4 tracks with
  `OutputConfig::Capture { clocked: false }` and `GainMode::Off`, pumped to completion, writes a
  capture that is bit-identical to the four per-track decodes laid end to end. This is the
  property that makes playback gapless: the player adds and drops nothing at a track switch.
  - Exception: 44.1k sets may differ within ±1 frame of each boundary, because each track restarts
    the resampler. Assert the total length within ±4 frames and bit-identity away from the
    boundaries (outside ±64 frames). A comment points at the deferred swr-carry task.

## Release-prefix run

A doc comment at the top of both test files gives the command, using the spike's method:

```
PKG_CONFIG_LIBDIR=<prefix>/lib/pkgconfig CARGO_TARGET_DIR=target/release-ffmpeg \
  cargo test --test ffmpeg_formats --test gapless
```

with `<prefix>` as `build_ffmpeg.py` prints it (for example
`target/native/ffmpeg-9.0.2-arm64-…`). The builder confirms the command works against the prefix
that already exists in `target/native/`, and adds any needed `DYLD_LIBRARY_PATH` to the comment.

## Small source changes

- Public scanner extension list (above).
- Update the stale comment at `src/bin/audio-probe.rs:1-2` ("no gapless claim is made without
  measuring PCM") to point at `tests/gapless.rs`.

## Left to the builder

- Signal amplitudes and phases; fixture bit rates beyond those fixed above.
- How the format test drives `Library` (handle commands, waiting for completion).
- Shared test helpers (a `tests/support` module or local functions), in the style of
  `tests/playback.rs`.

## Verification

- Both suites pass with `cargo test`, and against the release prefix with the documented command.
- Mutation check, done once by hand and reported (not committed): breaking one thing makes the
  matching assertion fail. For example, dropping the last frame of each decode fails the length
  and concatenation tests, and removing `wav` from the scanner list fails the format coverage
  check.
- Standard gates: `cargo fmt --check`, clippy `-D warnings`, `cargo test`. The Swift client is
  untouched.
