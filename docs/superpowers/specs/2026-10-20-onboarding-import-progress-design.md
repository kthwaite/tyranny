# Onboarding: Obvious Import Progress

Date: 2026-10-20. Scope: macOS client (`macos/`) only. No server or protocol change. Task:
TSK-dark-sorbet-jfb1x.

The user skipped the design questions ("skip questions and start"). Every decision below is
Claude's call unless it quotes REQUIREMENTS.md.

## Intent

REQUIREMENTS.md, Interface: "Simple onboarding - click the prominent button, choose your music
directory, wait for the obvious progress indicator to fill up, bam, done".

Today the first two steps work. The third and fourth do not:

- The "Add Your Music" empty state shows only while `library.roots == 0`. When the folder is
  added, that state vanishes at once and the main area shows an empty, then slowly filling, Songs
  list.
- Progress shows only at the right of the status bar: a label plus a tiny spinner while
  discovering, and an 80 pt bar while indexing.
- When the scan ends the label disappears. There is no "done" moment.

## Goals

- After the user chooses their first music folder, the main area shows one large progress view
  until the import ends.
- The bar fills: indeterminate while discovering, determinate (`processed / total`) while indexing.
- The end is obvious: a "ready" screen with the library totals and one prominent button.
- A failed or interrupted import says so and offers a retry.

## Non-goals

- No change to the status bar's scan display. It still shows every scan, including later rescans.
- No progress screen for rescans or for adding a second folder. Those keep the status bar only.
- No Dock tile progress, notifications, or sounds.
- No server change. `ScanStatus` already carries `phase`, `discovered`, `processed`, `total`,
  `failures`, `skipped` and `last_error`.

## Anti-goals

- The user must never be trapped on the progress screen. They can leave it at any time, and
  choosing a different sidebar destination leaves it.
- The progress screen must not flash to the Songs list part-way through. Tracks become visible
  one batch at a time during the first scan (`library.tracks` grows while indexing), so the screen
  must not key off `tracks == 0`.
- The bar must never move backwards within a phase, and never show more than 100 %.

## Behaviour

### When the import screen shows

An **import session** is client state held by `PlayerModel` for this app run. It is not persisted.

| Event | Result |
|---|---|
| `add_root` succeeds while `library.roots == 0` (the first folder) | Start a session for the new root's id (`RootAdded.root.id`). |
| `add_root` succeeds while a session exists (after an empty first folder) | Restart the session for the new root. |
| First authoritative snapshot of a launch: `roots > 0`, `tracks == 0`, and the scan is active (`discovering` or `indexing`) | Start a session for `scan.root_id`. This covers a relaunch during the first import. |
| User presses **Browse Library** (while importing) or **Show Songs** (when done), or the destination changes | End the session. |
| Connection lost | Keep the session. The disconnected screen takes priority while disconnected (see below). |
| `add_root` fails | No session. The existing error message path is unchanged. |

### What the screen shows

The main area shows, in priority order: disconnected state, then the import screen while a session
exists, then the "Add Your Music" empty state when `roots == 0`, then the destination.

The import screen has one of six stages, computed from the session's root id and the current
`ScanStatus`:

| Stage | Condition | Title | Bar | Detail line | Buttons |
|---|---|---|---|---|---|
| Preparing | `scan.root_id` is not the session root, or phase `idle` | Importing Your Music | indeterminate | the folder path | Browse Library |
| Discovering | phase `discovering` for the session root | Importing Your Music | indeterminate | "Finding music… 1,204 files found" | Browse Library |
| Indexing | phase `indexing` for the session root | Importing Your Music | determinate, `min(processed, total) / total` | "Reading 412 of 1,129 files" | Browse Library |
| Done | phase `complete` for the session root, `tracks > 0` | Your Music Is Ready | none; a large checkmark symbol instead | library totals, e.g. "1,129 songs · 3.4 days · 41.7 GB" | **Show Songs** (prominent, default) |
| Empty | phase `complete` for the session root, `tracks == 0` | No Music Found | none | "Tyranny found no audio files in this folder." | **Choose Another Folder…** (`model.chooseFolder`) |
| Stopped | phase `failed` or `interrupted` for the session root | Import Stopped | none | `last_error`, or "The import did not finish." | **Try Again** (rescans the root), Browse Library |

- Indexing with `total` nil or 0 shows the indeterminate bar and "Reading 412 files".
- Under the detail line, when `failures + skipped > 0` on Indexing or Done: "12 files couldn't be
  read" (failures and skipped are summed; singular "1 file couldn't be read").
- Below the title, the folder path, abbreviated with `~` (`DisplayFormat.abbreviatedPath`).
- The bar is about 360 pt wide, regular size. The title uses `.title2` semibold, as the other empty
  states do (`ContentUnavailableView` style). Exact layout is left to the builder.
- Done uses the library totals format the status bar already uses (`DisplayFormat.totals` with no
  `of:`). On Empty the session continues, so the next folder chosen restarts it.
- **Show Songs** ends the session and sets `navigation.destination = .songs`.
- **Try Again** sends `rescan` for the session root. The session continues and returns to
  Preparing until the scan reports for that root.

### Stale status at start and retry

A session records the `ScanStatus` on show when it starts or retries (its *baseline*). While the
current status equals the baseline exactly, a terminal phase (`complete`, `failed`, `interrupted`)
reads as Preparing, so the old result of a retried scan is not shown as the new one. Any change
to the status ends this, so a scan that finishes between two snapshots still reaches Done. The
launch rule starts a session with no baseline. If a retried scan fails with an identical status,
the screen stays on Preparing; Browse Library still leaves it.

### Monotonic bar

Within one session, the shown determinate fraction never decreases. If a later snapshot has a
smaller fraction (a reordered or restarted scan), the screen keeps the larger one until the
fraction passes it or the stage changes. Clamp to `0...1`.

## Structure

- **TyrannyCore** gets a pure `ImportProgress` type (name is the builder's call): given the session
  root id, the `ScanStatus`, the library track count and the previous shown fraction, it returns the
  stage, title, detail text, failure line, and fraction (nil for indeterminate). All copy above lives
  here so it is unit-tested.
- **PlayerModel** holds the optional session (root id plus last shown fraction), starts and ends it
  per the table, and exposes the computed `ImportProgress`.
- **App** gets an `ImportProgressView` in `Shell/`, inserted into `MainWindowView`'s main-area
  branch. `NavigationModel` destination changes end the session.
- Accessibility identifiers: `import-progress` (container), `import-progress-bar`,
  `import-show-songs`, `import-browse`, `import-retry`.

## Verification

- TyrannyCore unit tests for every row of the stage table, the failure line (0, 1, many), nil and
  zero `total`, clamping, the monotonic rule, a status for another root, Empty, and the baseline
  rule.
- Unit tests for the session start rules (first folder only; launch rule; not on second folder).
- `NativeShellSmoke` onboarding case: after choosing the folder, `import-progress` exists; after the
  scan completes, `import-show-songs` exists; pressing it shows `main-songs`. Update
  `NativeSmoke`'s onboarding import to dismiss the screen before it continues.
- Renders with `scripts/render_views.py`: discovering, indexing, done, stopped, empty. The render harness
  may inject a fixed status so images are deterministic.
- Standard gates: `cargo fmt --check`, clippy `-D warnings`, `cargo test`, Swift debug and release
  builds with `-warnings-as-errors`, `swift test`, `client_gate.py --model-only`. Native gates remain
  blocked on Accessibility/Screen Recording permissions.
