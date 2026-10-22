# Onboarding Import Progress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After the user chooses their first music folder, the main area shows a large import progress screen that fills, then a clear "ready" screen.

**Architecture:** A pure TyrannyLikesSmokeCore `ImportSession` turns the session root, the live `ScanStatus` and library totals into an `ImportProgress` value (stage, copy, fraction). `PlayerModel` owns the optional session, starts and ends it, and stores the latest `ImportProgress`. A new `ImportProgressView` renders it in `MainWindowView`'s main-area branch.

**Tech Stack:** Swift 6, SwiftUI/AppKit, Swift Testing (TyrannyLikesSmokeCore), the native smoke and render harnesses (DEBUG).

**Spec:** `docs/superpowers/specs/2026-10-20-onboarding-import-progress-design.md`

## Global Constraints

- Client only. No change under `src/`, `tests/`, or to the protocol.
- Swift builds pass with `-warnings-as-errors` in debug and release.
- Copy is exactly as the spec's stage table: titles "Importing Your Music", "Your Music Is Ready", "No Music Found", "Import Stopped"; buttons "Browse Library", "Show Songs", "Choose Another Folder…", "Try Again".
- Accessibility identifiers: `import-progress`, `import-progress-bar`, `import-show-songs`, `import-browse`, `import-retry`, plus `import-choose-another` for Choose Another Folder….
- Phase strings are `"idle"`, `"discovering"`, `"indexing"`, `"complete"`, `"failed"`, `"interrupted"` (`ScanStatus.phase` is a `String`).
- Never stage or modify the user's uncommitted work: `.gitignore`, `SPIKE.md`, `SPIKE_RESULTS.md`, `macos/Info.plist`, `scripts/build_macos.sh`, `icons/`, `justfile`, `macos/Resources/`, `scripts/build_dmg.sh`, `scripts/build_icon.sh`, `.DS_Store`. Stage files by explicit path only.

## Review Focus

1. A small folder that finishes between two snapshots (the client never sees `discovering`/`indexing` for it) must still reach Done, not hang on Preparing. Test: `finishedBetweenSnapshotsReachesDone` (Task 1).
2. `processed > total` or `total == 0` must not show > 100 % or divide by zero. Tests: `indexingClampsProcessedAboveTotal`, `indexingWithZeroTotalIsIndeterminate` (Task 1).
3. A scan for a different root (e.g. a rescan started from Settings for another folder) must not drive the screen. Test: `statusForAnotherRootIsPreparing` (Task 1).
4. Adding a second folder from Settings with a populated library must not show the screen. Test: `addingASecondFolderStartsNoSession` (Task 1).
5. Picking a sidebar destination while importing must leave the screen; the user is never trapped. Covered by the `NativeShellSmoke` step in Task 2.

---

### Task 1: `ImportSession` and `ImportProgress` in TyrannyLikesSmokeCore

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/ImportProgress.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/ImportProgressTests.swift`

**Interfaces:**
- Consumes: `ScanStatus`, `LibraryState`, `ListTotals`, `DisplayFormat.count(_:)`, `DisplayFormat.totals(_:of:)` (all existing in TyrannyLikesSmokeCore).
- Produces:

```swift
public struct ImportProgress: Equatable, Sendable {
    public enum Stage: Equatable, Sendable { case preparing, discovering, indexing, done, empty, stopped }
    public let stage: Stage
    public let title: String
    public let detail: String?       // nil on Preparing (the view shows the folder path instead)
    public let failureLine: String?  // "12 files couldn't be read"; Indexing and Done only
    public let fraction: Double?     // nil = indeterminate; set only on Indexing with total > 0
}

public struct ImportSession: Equatable, Sendable {
    public let rootID: UInt64
    public init(rootID: UInt64, baseline: ScanStatus?)
    /// Starts a fresh baseline after Try Again; resets the shown fraction.
    public mutating func retry(baseline: ScanStatus)
    /// The screen for this status. Updates the remembered fraction (monotonic rule).
    public mutating func progress(library: LibraryState) -> ImportProgress
    /// Session to start after `add_root` succeeds, or nil. `rootsBefore` is `library.roots`
    /// before the request; `current` is the existing session, if any.
    public static func afterAddRoot(rootID: UInt64, rootsBefore: Int, current: ImportSession?,
                                    baseline: ScanStatus?) -> ImportSession?
    /// Session for the first authoritative snapshot of a launch, or nil.
    public static func atLaunch(library: LibraryState) -> ImportSession?
}
```

Rules the body must follow (from the spec):
- Status belongs to the session only when `scan.root_id == rootID`; otherwise Preparing.
- Baseline: while `library.scan == baseline` (whole-struct equality), terminal phases read as Preparing. Once any status differs from the baseline, clear the baseline for good.
- Monotonic: on Indexing, shown fraction = `max(remembered, clamp(min(processed,total)/total, 0...1))`; reset the remembered fraction when the stage changes.
- `failureLine`: `failures + skipped`; 0 → nil; 1 → "1 file couldn't be read"; n → "\(count(n)) files couldn't be read".
- `afterAddRoot`: returns a session when `rootsBefore == 0` or `current != nil`; else nil.
- `atLaunch`: returns a session (no baseline) when `roots > 0`, `tracks == 0`, phase is `discovering` or `indexing`, and `scan.root_id` is non-nil.

- [ ] **Step 1: Write the failing tests** in `ImportProgressTests.swift` (`@Suite struct ImportProgressTests`). Use a helper `scan(_ phase: String, root: UInt64? = 7, discovered: Int = 0, processed: Int = 0, total: Int? = nil, failures: Int = 0, skipped: Int = 0, error: String? = nil) -> ScanStatus` and `library(_ scan: ScanStatus, roots: Int = 1, tracks: Int = 0, durationMs: UInt64 = 0, bytes: UInt64 = 0) -> LibraryState`. Tests and their key expectations:

  - `idleIsPreparing`: phase idle → `.preparing`, title "Importing Your Music", detail nil, fraction nil.
  - `statusForAnotherRootIsPreparing`: root 9 indexing 5/10 → `.preparing`.
  - `discoveringShowsFoundCount`: discovered 1204 → `.discovering`, detail "Finding music… 1,204 files found", fraction nil.
  - `indexingShowsFraction`: 412 of 1129 → `.indexing`, detail "Reading 412 of 1,129 files", fraction `412.0/1129.0`.
  - `indexingWithNilTotalIsIndeterminate`: processed 412, total nil → detail "Reading 412 files", fraction nil.
  - `indexingWithZeroTotalIsIndeterminate`: total 0 → fraction nil, detail "Reading 412 files".
  - `indexingClampsProcessedAboveTotal`: 15 of 10 → fraction 1.0.
  - `fractionNeverMovesBackwards`: progress at 6/10, then 4/10 → second fraction 0.6; then 8/10 → 0.8.
  - `fractionResetsWhenStageChanges`: 6/10 indexing, then discovering, then indexing 2/10 → 0.2.
  - `failureLineCountsFailuresAndSkipped`: (0,0) → nil; (1,0) → "1 file couldn't be read"; (10,2) → "12 files couldn't be read".
  - `completeWithTracksIsDone`: tracks 1129, durationMs 293_760_000, bytes 41_700_000_000 → `.done`, title "Your Music Is Ready", detail `DisplayFormat.totals(ListTotals(tracks: 1129, duration_ms: 293_760_000, size_bytes: 41_700_000_000))`, fraction nil.
  - `completeWithNoTracksIsEmpty`: → `.empty`, title "No Music Found", detail "tyranny likes smoke found no audio files in this folder.".
  - `failedIsStopped`: failed with error "disk vanished" → `.stopped`, title "Import Stopped", detail "disk vanished".
  - `interruptedWithoutErrorIsStopped`: → `.stopped`, detail "The import did not finish.".
  - `terminalBaselineReadsAsPreparing`: session baseline = failed status S; progress(S) → `.preparing`.
  - `finishedBetweenSnapshotsReachesDone`: baseline = idle status (root nil); progress(complete for root 7, tracks 3) → `.done`.
  - `changedStatusClearsBaseline`: baseline S (failed); progress(discovering) then progress(S again) → second is `.stopped`.
  - `retryRestoresBaseline`: after retry(baseline: S), progress(S) → `.preparing`.
  - `firstFolderStartsSession`: `afterAddRoot(rootID: 7, rootsBefore: 0, current: nil, baseline: nil)?.rootID == 7`.
  - `addingASecondFolderStartsNoSession`: `rootsBefore: 1, current: nil` → nil.
  - `addAfterEmptyRestartsSession`: `rootsBefore: 1, current: ImportSession(rootID: 7, …)`, rootID 8 → session for 8.
  - `launchDuringFirstImportStartsSession`: roots 1, tracks 0, indexing root 7 → session root 7.
  - `launchWithTracksStartsNoSession`: tracks 5 → nil; idle → nil; roots 0 → nil.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd macos && swift test --filter ImportProgressTests`
Expected: FAIL to compile (`ImportSession` not found).

- [ ] **Step 3: Implement `ImportProgress.swift`** with the interfaces and rules above. Doc comments in the style of `NowPlayingArt.swift` / `QuitSuspension.swift`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd macos && swift test --filter ImportProgressTests` then `cd macos && swift test`
Expected: all pass (previous total 202 tests plus the new ones).

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/TyrannyLikesSmokeCore/ImportProgress.swift macos/Tests/TyrannyLikesSmokeCoreTests/ImportProgressTests.swift
git commit -m "Model the onboarding import screen in TyrannyLikesSmokeCore"
```

---

### Task 2: Session wiring, `ImportProgressView`, and the smokes

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmoke/PlayerModel.swift` (`accept(_:newInstance:)` ~273, `addRoot` ~378, `rescan` ~397)
- Create: `macos/Sources/TyrannyLikesSmoke/Shell/ImportProgressView.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/Shell/MainWindowView.swift` (main-area branch ~130-170)
- Modify: `macos/Sources/TyrannyLikesSmoke/NativeShellSmoke.swift` (~49-63), `macos/Sources/TyrannyLikesSmoke/NativeSmoke.swift` (~72-80)

**Interfaces:**
- Consumes: Task 1's `ImportSession`, `ImportProgress`.
- Produces (on `PlayerModel`, all `@MainActor`):
  - `private(set) var importProgress: ImportProgress?` (stored and observed; nil when no session)
  - `var importRootPath: String?` — the session root's path from `roots`, abbreviated with `DisplayFormat.abbreviatedPath(_, home: NSHomeDirectory())`
  - `func endImport()`
  - `func retryImport()`
  - DEBUG only: `var importOverride: ImportProgress?` — when non-nil the view shows it instead of `importProgress` (Task 3 uses it).

- [ ] **Step 1: Wire the session in `PlayerModel`.**
  - Add `@ObservationIgnored private var importSession: ImportSession?`.
  - In `addRoot`, read `snapshot?.library.roots` and `snapshot?.library.scan` before the request; after `RootAdded` succeeds, set `importSession = ImportSession.afterAddRoot(rootID: added.root.id, rootsBefore:, current: importSession, baseline:)`, then recompute.
  - In `accept`, inside the existing `newInstance, !launchSuspensionChecked` block's sibling: on the first authoritative snapshot of a launch (use a new `launchImportChecked` flag the same way), set `importSession = ImportSession.atLaunch(library: state.library)` when `importSession == nil`. Then on every accepted snapshot, if a session exists, `importProgress = importSession!.progress(library: state.library)` (write back the mutated session).
  - `endImport()`: clears session and `importProgress`.
  - `retryImport()`: `importSession?.retry(baseline: current scan)`, recompute, then `rescan(rootID)`.
  - On `disconnect()`/snapshot reset, keep the session (spec: keep while disconnected).

- [ ] **Step 2: Build `ImportProgressView(progress: ImportProgress, folder: String?, …actions)`** in `Shell/ImportProgressView.swift`.
  - Centred column, like `ContentUnavailableView`: symbol (`music.note.house` while importing, `checkmark.circle.fill` on Done, `questionmark.folder` on Empty, `exclamationmark.triangle` on Stopped), title `.title2.weight(.semibold)`, folder path (secondary), `ProgressView` ~360 pt wide on Preparing/Discovering/Indexing (`ProgressView(value:)` when `fraction` is set, indeterminate linear otherwise), detail, failure line (secondary), buttons per the spec table.
  - Show Songs is `.borderedProminent` with `.keyboardShortcut(.defaultAction)`; it calls `endImport()` and sets `navigation.destination = .songs`. Browse Library calls `endImport()`. Try Again calls `retryImport()`. Choose Another Folder… calls `model.chooseFolder()` and is disabled when `!model.canMutate || model.scanActive`.
  - Identifiers per Global Constraints; the container `.accessibilityElement(children: .contain)` with `import-progress`.

- [ ] **Step 3: Insert it into `MainWindowView`.** New branch directly after `if !model.connected { … }`: `else if let progress = (DEBUG importOverride ??) model.importProgress { ImportProgressView(…) }`. Add `.onChange(of: navigation.destination) { model.endImport() }` on the main window content.

- [ ] **Step 4: Update `NativeShellSmoke`.** After pressing Add Music Folder: `try await drive("exists", "import-progress")`; wait for scan complete with 105 tracks (as today); `try await drive("exists", "import-show-songs")`; `try capture("shell-import-done")`; `try await press("import-show-songs")`; wait `model.importProgress == nil`; `try await drive("exists", "main-songs")`. Then a second check of the trap rule is unnecessary here, because the destination loop that follows already selects destinations with no session.
  Add case string `"native-onboarding-import-screen-fills-then-shows-songs"`.

- [ ] **Step 5: Update `NativeSmoke`.** After `native folder import` completes, `try await press("import-show-songs")` and wait `model.importProgress == nil` before `songs list`.

- [ ] **Step 6: Build and test**

Run:
```bash
cd macos && swift build -Xswiftc -warnings-as-errors && swift build -c release -Xswiftc -warnings-as-errors && swift test
python3 scripts/client_gate.py --model-only
```
Expected: builds clean, all tests pass, gate passes. (Native `--app` gates need Accessibility/Screen Recording; not runnable here.)

- [ ] **Step 7: Commit**

```bash
git add macos/Sources/TyrannyLikesSmoke/PlayerModel.swift macos/Sources/TyrannyLikesSmoke/Shell/ImportProgressView.swift macos/Sources/TyrannyLikesSmoke/Shell/MainWindowView.swift macos/Sources/TyrannyLikesSmoke/NativeShellSmoke.swift macos/Sources/TyrannyLikesSmoke/NativeSmoke.swift
git commit -m "Show the import progress screen after the first music folder"
```

---

### Task 3: Renders of every stage

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmoke/NativeRenderViews.swift` (after the `20-player-bar-error` shot, before `renderSettings`)
- Output: `spike-results/onboarding-ui/` (run `scripts/render_views.py --out spike-results/onboarding-ui`)

**Interfaces:**
- Consumes: Task 2's DEBUG `PlayerModel.importOverride`, Task 1's `ImportSession` (build each `ImportProgress` through `ImportSession.progress(library:)` with a fixed `LibraryState`, not by hand, so the renders show real copy).

- [ ] **Step 1: Add five shots**, each setting `model.importOverride`, then `await shot(...)`, then clearing it (also clear it in the harness's restore path however the run ends): `21-import-discovering` (discovered 1,204), `22-import-indexing` (412 of 1,129, failures 3), `23-import-done` (1,129 tracks, 293,760,000 ms, 41.7 GB), `24-import-stopped` (failed, "The music folder is no longer available."), `25-import-empty`.

- [ ] **Step 2: Build and render**

Run: `cd macos && swift build -Xswiftc -warnings-as-errors` then `python3 scripts/render_views.py --out spike-results/onboarding-ui`
Expected: the run log lists 21–25 with no `error:` line; the five PNGs exist. Read each PNG and check: bar visible on 21/22 and 22's about 36 % full; checkmark and "Show Songs" on 23; "Try Again" on 24; "Choose Another Folder…" on 25; nothing clipped at 1180 × 740.

- [ ] **Step 3: Commit**

```bash
git add macos/Sources/TyrannyLikesSmoke/NativeRenderViews.swift spike-results/onboarding-ui
git commit -m "Render the onboarding import screen stages"
```
