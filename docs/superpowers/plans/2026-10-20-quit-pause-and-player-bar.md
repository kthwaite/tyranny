# Quit Pauses Playback and the Player Bar — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two changes. First, ⌘Q pauses the server and the next launch resumes. Second, the toolbar transport moves into a full-width player bar whose album art can expand into the sidebar.

**Architecture:**
- The server adds an optional `artist_id` to `NowPlaying`, so the bar can link to the artist.
- The quit and resume decision is pure TyrannyCore logic: `QuitSuspension`, `QuitSuspensionStore` and `Deadline`. The app delegate and `PlayerModel` wire it up.
- The player bar is a new SwiftUI view in the app target. The toolbar keeps only the filter field. A persisted `NavigationModel` flag places the art, and TyrannyCore's `NowPlayingArt.placement` decides where it shows.

**Tech Stack:**
- Rust (tokio, serde) for the server.
- Swift 6.3, SwiftUI with AppKit, macOS 14, and Swift Testing for the client.

**Spec:** `docs/superpowers/specs/2026-10-20-quit-pause-and-player-bar-design.md`. The mockup lives in `.superpowers/brainstorm/*/content/player-bar.html`; it is git-ignored, and the spec's layout section is authoritative.

## Global Constraints

- Branch: `feature/quit-pause-player-bar`. Commit messages carry no attribution lines.
- macOS 14 deployment target.
- Rust gates, run on every task that touches `src/` or `tests/`:
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`
- Swift gates, run on every task that touches `macos/`:
  - `swift build --package-path macos --scratch-path target/swift-client -Xswiftc -warnings-as-errors`
  - the same build with `-c release`
  - `swift test --package-path macos --scratch-path target/swift-client`
- The model gate, `python3 scripts/client_gate.py --model-only`, runs on every client task.
- The native `--app` gates cannot run here, because the terminal lacks Accessibility and Screen Recording permission. Update them anyway when they assert changed behaviour.
- Protocol `VERSION` stays 9. `artist_id` is an additive `Option`, and Swift's synthesized `Decodable` treats a missing optional key as nil.
- Every interactive control keeps or gets a kebab-case `accessibilityIdentifier`. Moved controls keep their existing identifiers:
  - `transport-previous`, `transport-play-pause`, `transport-next`, `transport-shuffle`, `transport-repeat`
  - `up-next-drawer-toggle`
  - `now-playing-display`, `now-playing-title`, `now-playing-subtitle`, `now-playing-scrubber`, `now-playing-elapsed`, `now-playing-remaining`, `now-playing-format`, `now-playing-error-summary`, `now-playing-error`, `now-playing-metadata-warning`, `now-playing-art`
  - `library-filter`
- `UserDefaults` keys:
  - `quitSuspension`: JSON for `QuitSuspension`.
  - `nowPlayingArtExpanded`: Bool, default `false`.
- The quit deadline is 1 second.

## Review Focus

1. **Quit while the server is buffering.** `transport == "buffering"` counts as playing: pause it, record the note, and resume on relaunch. The test is `noteTreatsBufferingAsPlaying`, in Task 2.
2. **Quit while the server is hung.** The pause reply never arrives. Quit must still complete within about 1 s, and no note is written. The test is `deadlineReturnsNilWhenOperationOutlivesIt` in Task 2, which uses an operation that ignores cancellation.
3. **Relaunch against a different server.** This covers another `--socket`, or the managed socket after using `--socket`. It must not resume. The test is `doesNotResumeOnAnotherSocket`, in Task 2.
4. **Another client changes the track between quit and relaunch.** The relaunch must leave the new track alone. The tests are `doesNotResumeWhenTheCurrentEntryChanged` and `doesNotResumeWhenAlreadyPlaying`, in Task 2.
5. **The window at its 940 pt minimum width.** The bar must not overlap or clip its controls. The left and right sections stay fixed and the scrubber shrinks. The check is render `17-player-bar-min-width`, in Task 4.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src/protocol.rs` | modify | `NowPlaying.artist_id: Option<u64>` |
| `src/store.rs` | modify | `EntryTrack.artist_id`; `entry_track` selects `t.artist_id` |
| `src/owner.rs` | modify | `describe` fills `artist_id` |
| `src/playback.rs` | modify | `source_metadata` sets `artist_id: None` |
| `tests/support/library_lists.rs` | modify | assert `artist_id` in `now_playing_reports_format_and_applied_gain` |
| `macos/Sources/TyrannyCore/WireModels.swift` | modify | Swift `NowPlaying.artist_id` |
| `macos/Sources/TyrannyCore/QuitSuspension.swift` | create | `QuitSuspension`, `QuitSuspensionStore`, `Deadline` |
| `macos/Sources/TyrannyCore/NowPlayingArt.swift` | create | `NowPlayingArt.placement` |
| `macos/Tests/TyrannyCoreTests/QuitSuspensionTests.swift` | create | Task 2 tests |
| `macos/Tests/TyrannyCoreTests/NowPlayingArtTests.swift` | create | Task 4 tests |
| `macos/Tests/TyrannyCoreTests/WireModelTests.swift` | modify | `artist_id` decode tests |
| `macos/Sources/Tyranny/TyrannyApp.swift` | modify | `applicationShouldTerminate` |
| `macos/Sources/Tyranny/PlayerModel.swift` | modify | `suspendForQuit()`, resume on launch |
| `macos/Sources/Tyranny/ManagedServer.swift` | modify | header comment only |
| `macos/Sources/Tyranny/NativeSmoke.swift` | modify | relaunch waits for resumed playback |
| `scripts/lifecycle_gate.py` | modify | quit pauses, relaunch resumes |
| `SPIKE.md` | modify | record the reversed quit decision |
| `macos/Sources/Tyranny/Shell/PlayerBar.swift` | create | the bar (left, centre, right sections) |
| `macos/Sources/Tyranny/Shell/TransportDisplay.swift` | modify or delete | `NativeSeekSlider` and `VolumeSlider` survive; the toolbar display goes |
| `macos/Sources/Tyranny/Shell/MainWindowView.swift` | modify | toolbar slimmed; bar above `StatusBar`; split-view visibility binding |
| `macos/Sources/Tyranny/Shell/SourceList.swift` | modify | large art inset at the sidebar foot |
| `macos/Sources/Tyranny/Models/NavigationModel.swift` | modify | `nowPlayingArtExpanded` |
| `macos/Sources/Tyranny/NativeRenderViews.swift` | modify | renders 15–17 |

---

### Task 1: `NowPlaying.artist_id` end to end

**Files:**
- Modify: `src/protocol.rs:407-421`, `src/store.rs:135-139`, `src/store.rs:357-380`, `src/owner.rs:695-710`, `src/playback.rs:1400-1415`
- Modify: `macos/Sources/TyrannyCore/WireModels.swift:19-45`
- Test: `tests/support/library_lists.rs:371-437`, `macos/Tests/TyrannyCoreTests/WireModelTests.swift`

**Interfaces:**
- Produces:
  - Rust: `NowPlaying.artist_id: Option<u64>`, placed after `album_id`. It is the track artist (`library_tracks.artist_id`), not the album artist.
  - Swift: `NowPlaying.artist_id: UInt64?`. `init` gains `artist_id:` after `album_id:`.

- [ ] **Step 1: Write the failing Rust assertions.** In `now_playing_reports_format_and_applied_gain`, after `assert_eq!(now.album_id, Some(track.album_id));`, add:
  ```rust
  assert_eq!(now.artist_id, Some(track.artist_id));
  ```
  Change the loose-entry assertion to:
  ```rust
  assert_eq!(
      (now.track_id, now.album_id, now.artist_id, now.format.as_str()),
      (None, None, None, "")
  );
  ```
- [ ] **Step 2: Run** `cargo test --test server_transport now_playing_reports_format_and_applied_gain`. Expected: compile error, because there is no field `artist_id`.
- [ ] **Step 3: Implement.**
  - Add the field to `NowPlaying` and `EntryTrack`.
  - `entry_track` selects `t.artist_id` and maps it.
  - `describe` sets `artist_id: Some(track.artist_id)`.
  - `source_metadata` and every other `NowPlaying { .. }` literal set `artist_id: None`. Find them with `rg "NowPlaying \{" src tests`.
- [ ] **Step 4: Run** the Rust gates. Expected: PASS.
- [ ] **Step 5: Write the failing Swift tests** in `WireModelTests`:
  - `nowPlayingDecodesArtistID`: decode a `NowPlaying` JSON with `"artist_id": 7` and expect `artist_id == 7`.
  - `nowPlayingWithoutArtistIDDecodesNil`: decode the same JSON without the key and expect `artist_id == nil`.
- [ ] **Step 6: Run** `swift test --package-path macos --scratch-path target/swift-client --filter WireModelTests`. Expected: FAIL.
- [ ] **Step 7: Implement** the Swift field and `init` parameter. Update every `NowPlaying(` call site; find them with `rg "NowPlaying\(" macos`.
- [ ] **Step 8: Run** the Swift gates and the model gate. Expected: PASS.
- [ ] **Step 9: Commit** with the message `Report the track artist id with now playing`.

### Task 2: Quit suspension logic in TyrannyCore

**Files:**
- Create: `macos/Sources/TyrannyCore/QuitSuspension.swift`
- Test: `macos/Tests/TyrannyCoreTests/QuitSuspensionTests.swift`

**Interfaces:**
- Produces:
  ```swift
  public struct QuitSuspension: Codable, Equatable, Sendable {
      public let socketPath: String
      public let entryID: UInt64
      public init(socketPath: String, entryID: UInt64)
      /// Non-nil only when `transport` is "playing" or "buffering" and `currentID` is set.
      public static func note(socketPath: String, transport: String?, currentID: UInt64?) -> QuitSuspension?
      /// True only for the same socket path, `transport == "paused"`, and `currentID == entryID`.
      public func shouldResume(socketPath: String, transport: String?, currentID: UInt64?) -> Bool
  }
  public struct QuitSuspensionStore: Sendable {
      public static let key = "quitSuspension"
      public init(defaults: UserDefaults)
      public func save(_ note: QuitSuspension)
      /// Returns the stored note and removes it. Undecodable data is removed and returns nil.
      public func take() -> QuitSuspension?
  }
  public enum Deadline {
      /// The operation's result, or nil once `limit` passes. It returns at the deadline even
      /// if the operation ignores cancellation; the operation is cancelled but not awaited.
      public static func race<T: Sendable>(_ limit: Duration,
                                           _ operation: @escaping @Sendable () async throws -> T) async -> T?
  }
  ```

- [ ] **Step 1: Write the failing tests.** Use `UserDefaults(suiteName: "QuitSuspensionTests.\(UUID())")!` for store tests. The tests are:
  - `noteWhenPlaying`: `note(socketPath: "/private/tmp/a/control.sock", transport: "playing", currentID: 4) == QuitSuspension(socketPath: "/private/tmp/a/control.sock", entryID: 4)`.
  - `noteTreatsBufferingAsPlaying`: same, with `"buffering"`.
  - `noNoteWhenPausedStoppedOrUnknown`: nil for `"paused"`, `"stopped"`, `"ended"`, `"error"`, `nil`; and for `"playing"` with `currentID: nil`.
  - `resumesWhenPausedOnTheSameEntry`: a note `("/private/tmp/a/control.sock", 4)` and `shouldResume("/private/tmp/a/control.sock", "paused", 4)` gives true.
  - `doesNotResumeOnAnotherSocket`: `shouldResume("/private/tmp/b/control.sock", "paused", 4)` gives false.
  - `doesNotResumeWhenTheCurrentEntryChanged`: `shouldResume("/private/tmp/a/control.sock", "paused", 5)` gives false.
  - `doesNotResumeWhenAlreadyPlaying`: false for `"playing"`, `"stopped"`, and `nil` transport.
  - `storeTakeReturnsTheNoteOnce`: save, then `take()` gives the note, and a second `take()` gives nil.
  - `storeTakeDropsUndecodableData`: set the key to `Data("x".utf8)`; `take()` gives nil, and the key is gone.
  - `deadlineReturnsTheOperationResult`: `await Deadline.race(.seconds(1)) { 42 } == 42`.
  - `deadlineReturnsNilWhenOperationOutlivesIt`: the operation loops `while true { try? await Task.sleep(for: .seconds(10)) }`, swallowing cancellation. `race(.milliseconds(100))` returns nil, and the elapsed time measured with `ContinuousClock` is under 1 s.
  - `deadlineReturnsNilWhenOperationThrows`.
- [ ] **Step 2: Run** `swift test --package-path macos --scratch-path target/swift-client --filter QuitSuspensionTests`. Expected: FAIL, because the types are not defined.
- [ ] **Step 3: Implement** `QuitSuspension.swift`.
  - `Deadline.race` must not use a task group, because a group awaits its children.
  - Start the operation and a sleeper as two unstructured `Task`s.
  - Resume a single `CheckedContinuation<T?, Never>` from whichever finishes first. Guard it with a lock-protected `resumed` flag.
  - Then cancel both tasks.
- [ ] **Step 4: Run** the Swift gates. Expected: PASS.
- [ ] **Step 5: Commit** with the message `Add quit suspension notes and a deadline race`.

### Task 3: Quit pauses, relaunch resumes

**Files:**
- Modify: `macos/Sources/Tyranny/TyrannyApp.swift:5-46`, `macos/Sources/Tyranny/PlayerModel.swift` (`accept`, `shutdown` region), `macos/Sources/Tyranny/ManagedServer.swift:4`
- Modify: `macos/Sources/Tyranny/NativeSmoke.swift:38-42`, `scripts/lifecycle_gate.py:241-252`, `SPIKE.md` (line 9 summary)

**Interfaces:**
- Consumes: `QuitSuspension`, `QuitSuspensionStore` and `Deadline` from Task 2.
- Produces:
  - `PlayerModel.suspendForQuit() async`. It pauses if playing, waits at most 1 s, and stores the note only after a successful reply.
  - `PlayerModel` resumes on the first snapshot of the app's first connection.

- [ ] **Step 1: `PlayerModel.suspendForQuit()`.**
  - Guard: `connected`, a snapshot, and a non-nil `QuitSuspension.note(socketPath:transport:currentID:)` from the snapshot.
  - Call `Deadline.race(.seconds(1)) { try await self.send("pause") as Applied }`.
  - Save the note to `QuitSuspensionStore(defaults: .standard)` only when the result is non-nil.
  - Bypass `mutate`/`canMutate`, so that `busy` cannot block quitting.
- [ ] **Step 2: Resume on launch.**
  - Add `@ObservationIgnored private var launchSuspensionChecked = false` to `PlayerModel`.
  - In `accept(_:newInstance:)`, on the first call with `newInstance == true` while `!launchSuspensionChecked`, set the flag first.
  - Then, if `QuitSuspensionStore.take()` returns a note and `shouldResume(socketPath: socketPath, transport: state.transport, currentID: state.current_id)` is true, call `transport.play()`.
  - Later reconnects never check again.
- [ ] **Step 3: The delegate.** `TyrannyAppDelegate.applicationShouldTerminate(_:) -> NSApplication.TerminateReply`:
  - If a suspension is already in flight, return `.terminateLater`. AppKit is already waiting for that reply, so a second ⌘Q adds nothing.
  - Otherwise, if `model?.connected == true`, return `.terminateLater`, then `Task { await model.suspendForQuit(); NSApp.reply(toApplicationShouldTerminate: true) }`.
  - Else return `.terminateNow`.
  - Replace the comment in `applicationWillTerminate` ("The daemon owns audio and outlives this controller.") with "The daemon outlives this controller; quit paused it first."
  - In `ManagedServer.swift:4`, replace "The app never owns playback." with "The daemon owns playback; the app only pauses it on quit."
- [ ] **Step 4: Update the gates that assert the old behaviour.**
  - `NativeSmoke` `--ui-smoke-relaunch`: replace the immediate `check(transport == "playing")` with `try await wait("resumed playback") { model.snapshot?.transport == "playing" }`. Rename the case to `native-relaunch-resumes-suspended-playback`.
  - `lifecycle_gate.py`, after `launch("present-playing")`:
    - The server must report `transport == "paused"`, with `position_frames` at or above the pre-launch value and the same `current_id`. Record `present-daemon-reused-and-client-quit-pauses`.
    - Then `launch("relaunch-resumes")`. Its report snapshot or a fresh `state()` must reach `"playing"` with an advancing position. Record `client-relaunch-resumes-suspended-playback`.
    - Add a case that pauses first: `control.request("pause")`, then launch, quit, relaunch. The server must still be `"paused"`. Record `client-quit-while-paused-does-not-resume`.
  - `SPIKE.md` line 9: replace "explicit Quit leaves daemon audio running but removes its system controls" with "explicit Quit pauses daemon audio (resumed on the next launch) and removes its system controls; see `docs/superpowers/specs/2026-10-20-quit-pause-and-player-bar-design.md`".
- [ ] **Step 5: Run** the Swift gates, the model gate, and `python3 scripts/lifecycle_gate.py`. The lifecycle gate drives LaunchServices with no Accessibility permission. If it cannot run here, record the exact failure in the task report and continue. Expected: PASS.
- [ ] **Step 6: Commit** with the message `Pause playback on quit and resume it on the next launch`.

### Task 4: Full-width player bar

**Files:**
- Create: `macos/Sources/Tyranny/Shell/PlayerBar.swift`, `macos/Sources/TyrannyCore/NowPlayingArt.swift`, `macos/Tests/TyrannyCoreTests/NowPlayingArtTests.swift`
- Modify: `macos/Sources/Tyranny/Shell/MainWindowView.swift:51-142`, `macos/Sources/Tyranny/Shell/TransportDisplay.swift`, `macos/Sources/Tyranny/Shell/SourceList.swift`, `macos/Sources/Tyranny/Models/NavigationModel.swift:30-45`, `macos/Sources/Tyranny/NativeRenderViews.swift:36-90`

**Interfaces:**
- Consumes: `NowPlaying.artist_id` and `album_id` (Task 1); `NavigationModel.reveal` and `revealPlaying()`; `ArtworkView` and `ArtworkStore.nowPlayingRequest`; `NativeSeekSlider` and `VolumeSlider`.
- Produces:
  ```swift
  // TyrannyCore
  public enum NowPlayingArt {
      public enum Placement: Equatable, Sendable { case bar, sidebar }
      /// Expanded art shows in the sidebar only while the sidebar is visible.
      public static func placement(expanded: Bool, sidebarVisible: Bool) -> Placement
  }
  // App
  extension NavigationModel { var nowPlayingArtExpanded: Bool }  // persisted, key "nowPlayingArtExpanded"
  struct PlayerBar: View { let model: PlayerModel; @Bindable var navigation: NavigationModel; let sidebarVisible: Bool }
  ```
- New identifiers:
  - `player-bar`
  - `now-playing-art-expand`: the thumbnail button.
  - `now-playing-art-large`: the sidebar art.
  - `now-playing-art-collapse`: the large art button.
  - `now-playing-artist-link`, `now-playing-album-link`.

- [ ] **Step 1: Write the failing tests.** In `NowPlayingArtTests`:
  - `collapsedShowsInTheBar`: `placement(expanded: false, sidebarVisible: true) == .bar`.
  - `expandedShowsInTheSidebar`: `placement(expanded: true, sidebarVisible: true) == .sidebar`.
  - `expandedFallsBackToTheBarWhileTheSidebarIsHidden`: `placement(expanded: true, sidebarVisible: false) == .bar`.
- [ ] **Step 2: Run** `swift test --package-path macos --scratch-path target/swift-client --filter NowPlayingArtTests`. Expected: FAIL.
- [ ] **Step 3: Implement `NowPlayingArt`.** Add `nowPlayingArtExpanded` to `NavigationModel`. It is read in `init` and saved in `didSet`, like `upNextDrawerVisible`.
- [ ] **Step 4: Build `PlayerBar`** to the spec's layout section:
  - The bar is about 74 pt tall.
  - **Left section** (fixed at about 250 pt):
    - The 52 pt thumbnail shows only when the placement is `.bar` and a track is playing. Hovering it shows a ⤢ badge, and clicking it sets `nowPlayingArtExpanded = true`.
    - Then the title (semibold, followed by the metadata ⚠), the artist, and the album, each on one line and truncating.
    - The title is a plain button that calls `navigation.revealPlaying()`.
    - The artist and album are plain buttons that set `navigation.reveal` to `.artist(id)` or `.album(id)`. When the id is nil, they show as plain text.
    - A playback error replaces the whole left section with today's `errorSummary` button and popover; move that code from `TransportDisplay`.
  - **Centre section** (flexible):
    - shuffle, previous, a circular play/pause of about 30 pt, next, repeat. Keep today's enabled and disabled rules, help text, and accessibility values.
    - Below them: elapsed time, `NativeSeekSlider` at regular size with a max width of about 520 pt, and remaining time. Keep the seek-preview reset `onChange` handlers.
  - **Right section** (fixed at about 250 pt): the format line and gain line, the Up Next drawer toggle, a speaker icon, and `VolumeSlider`.
  - **Nothing playing or disconnected:** the layout is unchanged, with a blank art tile, empty text, and disabled controls.
- [ ] **Step 5: Rewire the window.**
  - In `MainWindowView.body`, wrap the `NavigationSplitView` and the bar in the existing `VStack`, in this order: split view, `Divider`, `PlayerBar`, `Divider`, `StatusBar`.
  - Bind `NavigationSplitView(columnVisibility:)` to a `@State` value. `sidebarVisible` is `columnVisibility != .detailOnly`.
  - Remove the `.navigation` and `.primaryAction` transport items and the `.principal` `TransportDisplay` from `toolbar`. Keep the filter field as a `ToolbarItem(placement: .primaryAction)`. The split view supplies its own sidebar toggle.
  - Delete the toolbar display struct from `TransportDisplay.swift`, keeping `NativeSeekSlider`, `VolumeSlider` and their helpers. The builder chooses whether to rename the file.
- [ ] **Step 6: Add the sidebar art.**
  - When `NowPlayingArt.placement(...) == .sidebar`, `SourceList` adds `.safeAreaInset(edge: .bottom)`.
  - It holds a square `ArtworkView(store: model.artwork, request: model.artwork.nowPlayingRequest, identifier: "now-playing-art-large", dimension: <sidebar width - 16>)`, with 8 pt padding.
  - Hovering it shows ⤡, and clicking it sets `nowPlayingArtExpanded = false`.
  - `SourceList` needs `sidebarVisible` only to stay consistent; while it is rendered, the sidebar is visible.
- [ ] **Step 7: Add the renders.** In `NativeRenderViews`, after `11-playlist-regular`:
  - Shoot `15-player-bar-collapsed` with expanded off and `16-player-bar-art-expanded` with expanded on, restoring the user's value afterwards.
  - Set the window to 940×560 for `17-player-bar-min-width`, then restore 1180×740.
  - Run `python3 scripts/render_views.py`, after `scripts/build_macos.sh` if the bundle is stale, and look at the PNGs.
  - Expected: no overlap or clipping. The bar sits above the status bar, and the toolbar shows only the filter.
- [ ] **Step 8: Run** the Swift gates and the model gate. Run `rg "TransportDisplay\(" macos` and expect no hits. Expected: PASS.
- [ ] **Step 9: Commit** with the message `Move transport into a full-width player bar with expandable art`.

---

## Self-review notes

- **Spec coverage.** Every spec section maps to a task:
  - Quit and relaunch behaviour table: Tasks 2 and 3.
  - Reversal of the recorded decision: Task 3, Step 3 (the comments) and Step 4 (`SPIKE.md`).
  - `artist_id`: Task 1.
  - Toolbar and bar layout, art expansion, the hidden-sidebar fallback, interactions and states: Task 4.
  - Verification: the tests in each task, the gates in Task 3, and the renders in Task 4.
- **Not covered by automated tests.** Hover badges, and real AppKit termination timing. The native gates would cover these, but they are blocked on permissions.
