# Faster Client Startup Implementation Plan

Historical implementation plan: control transport subsequently moved to private,
length-prefixed Unix-domain sockets. The WebSocket/token steps below record the
original design, not current connection instructions; see [SPIKE.md](../../../SPIKE.md#small-initial-interface).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut ~130 ms from client launch → rows on screen at 50k tracks with a launch prefetch of the first Songs window, cheaper track cells and no redundant first reload, and pin startup with scale_gate budgets.

**Architecture:** A TyrannyLikesSmokeCore actor `LaunchPrefetchSlot` holds the one prefetched reply and decides the one-shot claim. An app-side `LaunchPrefetch` opens a short-lived background WebSocket from `PlayerModel.init`, sends the Songs `list_tracks`, and fulfils the slot; `ListModel.fetch` claims it. Table changes are local to `VirtualTrackTable.swift`; budgets go in `scripts/scale_gate.py`.

**Tech Stack:** Swift 6 (strict concurrency), SwiftUI/AppKit, URLSessionWebSocketTask, Swift Testing; Python 3 gate script.

**Spec:** `docs/superpowers/specs/2026-10-20-startup-prefetch-design.md`

## Global Constraints

- Branch `feature/startup-prefetch` from `dev`; merge back into `dev` (see `CLAUDE.md`: `prod` takes merges only from `dev`).
- No server or protocol change; no change to `WireClient`'s main connection or the connect loop.
- Prefetch request: `command "list_tracks"`, `list` = launch Songs `ListDescription`, `revision: null`, `offset: 0`, `limit: 200`.
- Prefetch deadline: 2 s total; no retries; never starts a managed server; silent on failure.
- Claim: first claim only; fields equal as `[String: JSONValue]` (structural); ok reply only; otherwise normal fetch.
- Prefetch security = `WireClient`'s: validated loopback endpoint, token read only after validation, bearer header, redirects rejected, ephemeral session without caches/cookies/proxies.
- Budgets (release server, 50k): ready ≤ 300 ms, hello ≤ 50 ms, first Songs page ≤ 250 ms, total ≤ 500 ms; client warm `startup.process_to_model_ready_ms` ≤ 1500 ms independent of Accessibility. Debug server: record, do not assert.
- Swift builds clean with `-warnings-as-errors` (debug and release); `swift test`; `python3 scripts/client_gate.py --model-only --app "macos/.build/debug/tyranny likes smoke"`.
- Native `--app` gates need Accessibility/Screen Recording: not runnable here.
- Commit messages: plain imperative sentences, no attribution lines. Stage by explicit path. Never touch files outside the task's list.

## Review Focus

1. A user whose persisted Songs grouping/sort/browser differs from the defaults must get correct rows: the claim falls back on any field difference. Test: `differentFieldsFallBackAndEmptyTheSlot` (Task 1); Task 2 builds the request through the shared description function.
2. The server not running at launch (first managed launch) must cost nothing: the prefetch fails fast and the first fetch does not wait the full 2 s. Test: `failedPrefetchReturnsNilAtOnce` (Task 1) — a `fulfil(nil)` before or during a claim returns nil immediately.
3. A claim racing a late reply must not hang past the deadline. Test: `claimGivesUpAtTheDeadline` (Task 1).
4. Only the very first Songs fetch may use the reply — a later filter/sort change must never get launch rows. Test: `onlyTheFirstClaimCanSucceed` (Task 1).
5. The playing indicator must still appear and update on the playing row after the cell change. Covered by the existing native smokes (compile-checked here) and Task 3's manual render check.

---

### Task 1: `LaunchPrefetchSlot` in TyrannyLikesSmokeCore

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/LaunchPrefetch.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/LaunchPrefetchTests.swift`

**Interfaces:**
- Produces:

```swift
public actor LaunchPrefetchSlot {
    /// `request` is the prefetch's request fields (everything but `command` and `id`).
    public init(request: [String: JSONValue])
    /// The prefetch finished: the raw ok reply body, or nil for any failure. Later calls are ignored.
    public func fulfil(_ reply: Data?)
    /// One-shot. Returns the reply when this is the first claim, `request` equals the prefetch's
    /// request, and the prefetch succeeds by `deadline`; otherwise nil. The slot is empty afterwards.
    public func claim(_ request: [String: JSONValue], until deadline: ContinuousClock.Instant) async -> Data?
}
```

- [ ] **Step 1: Write the failing tests** (`@Suite struct LaunchPrefetchTests`), using a request `r = ["list": .string("songs"), "revision": .null, "offset": .int(0), "limit": .int(200)]` and `soon = ContinuousClock.now + .seconds(1)`:
  - `fulfilledEqualRequestIsClaimed`: fulfil(data) then claim(r) == data.
  - `differentFieldsFallBackAndEmptyTheSlot`: fulfil(data); claim(r with limit 100) == nil; claim(r) == nil.
  - `onlyTheFirstClaimCanSucceed`: fulfil(data); claim(r) == data; claim(r) == nil.
  - `inFlightPrefetchIsAwaited`: start claim(r, soon) in a Task; then fulfil(data) after ~20 ms; claim returns data.
  - `failedPrefetchReturnsNilAtOnce`: fulfil(nil); claim(r, now + 5 s) returns nil in well under 1 s (assert elapsed < 0.5 s); and the in-flight variant: claim started, then fulfil(nil) → nil promptly.
  - `claimGivesUpAtTheDeadline`: never fulfil; claim(r, now + 50 ms) returns nil, elapsed between 40 ms and 1 s.
  - `fulfilAfterTheClaimIsIgnored`: claim times out → nil; then fulfil(data) does nothing harmful; a further claim → nil.
  - `equalityIsStructural`: two dictionaries built in different insertion orders with equal values compare equal and claim succeeds.

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter LaunchPrefetchTests`
Expected: FAIL to compile (`LaunchPrefetchSlot` not found).

- [ ] **Step 3: Implement `LaunchPrefetchSlot`.** State: `.pending`, `.ready(Data?)`, `.claimed`. Waiters: a checked continuation resumed by `fulfil` or by a deadline task (`Task.sleep(until:)`), whichever first; resume exactly once. No polling.

- [ ] **Step 4: Run to verify pass**

Run: `cd macos && swift test --filter LaunchPrefetchTests` then `cd macos && swift test`
Expected: PASS; no warnings (`swift build -Xswiftc -warnings-as-errors`).

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/TyrannyLikesSmokeCore/LaunchPrefetch.swift macos/Tests/TyrannyLikesSmokeCoreTests/LaunchPrefetchTests.swift
git commit -m "Hold one launch prefetch reply for the first matching fetch"
```

---

### Task 2: Launch prefetch connection and the first-fetch claim

**Files:**
- Create: `macos/Sources/TyrannyLikesSmoke/LaunchPrefetch.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/WireClient.swift` (make `validateEndpoint`, `readToken`, `RedirectRejector` and the URLSession configuration usable off the main actor — `nonisolated static`, no behaviour change)
- Modify: `macos/Sources/TyrannyLikesSmoke/PlayerModel.swift` (`init` ≈78: start the prefetch after `endpoint`/`tokenPath` are set; add `func claimLaunchPrefetch(_ fields: [String: JSONValue]) async -> Data?`)
- Modify: `macos/Sources/TyrannyLikesSmoke/Models/ListModel.swift` (`fetch` ≈446: claim before `player.send`; shared launch description)
- Possibly modify: `macos/Sources/TyrannyLikesSmoke/Models/NavigationModel.swift` (only to expose the persisted inputs the shared description needs)

**Interfaces:**
- Consumes: Task 1's `LaunchPrefetchSlot`.
- Produces: `final class LaunchPrefetch: Sendable` (or a `nonisolated` struct + task) with `static func start(endpoint: String, tokenPath: String, request: [String: JSONValue]) -> LaunchPrefetchSlot`; `PlayerModel.claimLaunchPrefetch(_:) async -> Data?` (returns nil after the first call); a pure `static func launchSongsDescription(...) -> ListDescription` (name is the implementer's call) that `ListModel` also uses for its first Songs description.

- [ ] **Step 1: Shared description.** Find how `ListModel` builds its Songs description at launch (`init` ≈122 and `currentDescription()` ≈207: source, browser, query, sort, grouping). Extract one pure function both the prefetch and `ListModel` call with the same persisted inputs (UserDefaults keys via `NavigationModel`'s existing key helpers, e.g. `groupingKey`). If any input cannot be known at `PlayerModel.init`, leave it at the value `ListModel` uses for its first fetch and rely on the claim's equality check. Put the function in TyrannyLikesSmokeCore if it is pure; add a unit test that it produces the same description as `ListModel` would for: default defaults; Songs grouped by album.

- [ ] **Step 2: Prefetch connection** in `macos/Sources/TyrannyLikesSmoke/LaunchPrefetch.swift`, off the main actor: validate endpoint, read token, open an ephemeral URLSession with `RedirectRejector`, `webSocketTask` with the bearer header and `maximumMessageSize` = `WireClient.responseLimit`, resume, receive until the `hello` message, send `{"id": 1, "command": "list_tracks", …request}`, receive until the reply with `id` 1, `fulfil` its body if ok else `fulfil(nil)`, cancel the socket and invalidate the session. A 2 s overall deadline cancels everything and `fulfil(nil)`. Read `WireClient.receive`/`IncomingMessage` for the exact hello and reply shapes; reuse their decoding. Request fields use the same `JSONValue` constructors `ListModel.fetch` uses (`.int(offset)`, `.int(limit)`, `.null`) so structural equality holds.

- [ ] **Step 3: Wire it.** `PlayerModel.init`: after `endpoint`/`tokenPath` are final, `launchPrefetch = LaunchPrefetch.start(...)` with the shared description's request fields. Skip it for harness modes that never show Songs first (none known — note in the report if you find one). `claimLaunchPrefetch` calls `slot.claim(fields, until: start + 2 s)` once, then drops the slot. `ListModel.fetch`: build the fields dictionary first; `if let data = await player.claimLaunchPrefetch(fields)`, decode it as the normal reply (same `ListTracksResponse` path, including `ServerFailure` handling for error bodies); else `player.send` as today.

- [ ] **Step 4: Build and test**

Run: `cd macos && swift build -Xswiftc -warnings-as-errors && swift build -c release -Xswiftc -warnings-as-errors && swift test` then `python3 scripts/client_gate.py --model-only --app "macos/.build/debug/tyranny likes smoke"`
Expected: all clean/pass.

- [ ] **Step 5: Prove the claim is used (report only, not committed).** Add a temporary `print` to stderr on claim hit/miss, launch the debug app once with `--scale-output <tmp>` against a server seeded by `target/spikes/client-startup/seed.py` (see `target/spikes/quiet-jade/native.py` for the launch arguments; use harness arguments and an isolated state dir), confirm "hit", then remove the print. Also confirm a miss falls back by launching with Songs grouped differently in the harness defaults suite (`dev.tyranny.controller.harness`) only if the shared function cannot read it — otherwise skip and say why.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/TyrannyLikesSmoke/LaunchPrefetch.swift macos/Sources/TyrannyLikesSmoke/WireClient.swift macos/Sources/TyrannyLikesSmoke/PlayerModel.swift macos/Sources/TyrannyLikesSmoke/Models/ListModel.swift <any other touched file>
git commit -m "Prefetch the first Songs window while the window is built"
```

---

### Task 3: Cheaper track cells and no redundant first reload

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmoke/Lists/VirtualTrackTable.swift` (`Coordinator.update` ≈159–215; `TrackCellView` ≈575 onward)

**Interfaces:** none new.

- [ ] **Step 1: Shared playing image.** `TrackCellView`: a `private static let playingImage` built once with the existing symbol (`speaker.wave.2.fill`, accessibility description "Playing") and `SymbolConfiguration(pointSize: 10, weight: .regular)`; `init` no longer creates an image; the cell sets `icon.image = Self.playingImage` only when it shows the playing state and `nil` otherwise. Read how the cell shows/hides the icon today and keep the same visual result and accessibility.

- [ ] **Step 2: Skip the redundant reload.** In `Coordinator.update`, record `table.numberOfRows` before `noteNumberOfRowsChanged()`; when it was 0 and is now > 0 on that pass, do not call `reloadVisible()` (the new row views were configured from current data). All other paths unchanged. Comment why in one line.

- [ ] **Step 3: Build, test, render.**

Run: `cd macos && swift build -Xswiftc -warnings-as-errors && swift build -c release -Xswiftc -warnings-as-errors && swift test` then `cargo build && python3 scripts/render_views.py --app "macos/.build/debug/tyranny likes smoke" --out target/render-startup`
Expected: clean; renders complete with no `error:` lines. Look at the Songs render with a playing track (the player-bar renders 15–16 play a track): the speaker icon shows on the playing row only, unchanged from before.

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/TyrannyLikesSmoke/Lists/VirtualTrackTable.swift
git commit -m "Share the playing icon and skip the first redundant row reload"
```

---

### Task 4: Startup budgets in scale_gate and the before/after measurement

**Files:**
- Modify: `scripts/scale_gate.py` (`start()` ≈205, `native_run()` ≈245, targets ≈192, report/limitations ≈570–625)

**Interfaces:** none new.

- [ ] **Step 1: Server startup timing.** In `start()`, after the ready line, time the WebSocket `hello` and one Songs `list_tracks` (`{"source":{"kind":"library"}, …}` exactly as the client's first fetch: library source, default browser/query/sort, grouping none, revision null, offset 0, limit 200) on a fresh connection, and record per launch `ready_ms`, `hello_ms`, `first_page_ms`, `total_ms` beside the existing `authenticated_ready_ms`. With `--server-profile release`, assert the spec budgets (ready ≤ 300, hello ≤ 50, first page ≤ 250, total ≤ 500) with messages naming the launch and value; with a debug server, record only and append a limitation line.

- [ ] **Step 2: Client budget.** In `native_run()`, assert warm `startup.process_to_model_ready_ms ≤ 1500` as soon as the scale report is read, before and independent of Accessibility steps. Add `warm_native_model_ready_ms` (`<=`, 1500) and the server targets to the `targets` block.

- [ ] **Step 3: Run what runs here.** `python3 scripts/scale_gate.py --queries-only --server-profile release` (build `cargo build --release --bins` first). Expected: passes and the report carries the new server fields. If `--queries-only` does not start the measured server launches, say exactly what ran.

- [ ] **Step 4: Before/after measurement (report only).** Build `dev` in a throwaway worktree (`git worktree add --detach ../tyranny-startup-baseline dev`, then `cd macos && swift build`) and this branch's debug app. Seed a 50k fixture (`target/spikes/client-startup/seed.py`), start one release server, and alternate the two apps with `--scale-output` (see `target/spikes/quiet-jade/native.py`), n ≥ 8 warm each, same pass. Report median/max `process_to_model_ready_ms` for both and the delta. Remove the throwaway worktree. Use harness arguments and isolated state only.

- [ ] **Step 5: Commit**

```bash
git add scripts/scale_gate.py
git commit -m "Gate client and server startup times in scale_gate"
```
