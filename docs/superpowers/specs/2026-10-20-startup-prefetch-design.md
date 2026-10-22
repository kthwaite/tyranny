# Faster Client Startup: Launch Prefetch, Cheaper Cells, Startup Budgets

Historical transport note: launch prefetch now uses the same private Unix-socket
validation and bounded JSON framing as the main client, not WebSockets or bearer
tokens. The original measurements and design rationale below remain historical.

Date: 2026-10-20. Scope: macOS client (`macos/`) and `scripts/scale_gate.py`. No server or
protocol change. Task: TSK-quiet-jade-xd88r.

The user asked for "the three measured startup fixes (early connect + Songs prefetch, cheaper
cells, skip the redundant reloadVisible) as one plan … and add the scale_gate budgets", and
skipped the design questions. Every decision below is Claude's call unless it quotes that request
or the spike.

## Intent

REQUIREMENTS.md: "Instant startup, even with thousands of tracks". The 2026-10-20 profile
(`target/spikes/client-startup/FINDINGS.md`, local) measured launch → rows on screen at 50k tracks:
≈840 ms (release client) with a release server. All steps run in series:

- the main queue is not drained until ≈470 ms, so the connection starts only after the first frame;
- the Songs `list_tracks` (≈102 ms on the server) is sent only at ≈556 ms;
- creating the first rows costs ≈105 ms, half of it a redundant second reload.

The spike's versions of the three fixes saved 130 ms (release) and 146 ms (debug) together. This
design makes them production-quality and pins startup with a budget.

## Goals

1. **Launch prefetch.** At app launch, before the UI is built, send the first Songs window
   request to the server on a short-lived background connection. The first Songs fetch uses its
   reply when, and only when, it is the same request. Otherwise everything works exactly as
   today.
2. **Cheaper cells.** Track table cells stop creating a symbol image each; one shared image is set
   only on the playing row's status cell.
3. **No redundant reload.** When `noteNumberOfRowsChanged()` has just created every visible row
   from zero rows, `Coordinator.update` does not call `reloadVisible()` again.
4. **Budgets in `scripts/scale_gate.py`** (spike proposal, release server, 50k): server ready
   ≤ 300 ms, hello ≤ 50 ms, first Songs page ≤ 250 ms, total to first rows ≤ 500 ms; client
   `startup.process_to_model_ready_ms` ≤ 1500 ms, checked whether or not Accessibility runs.

## Non-goals

- No server query work (the O(library) first page stays; with the prefetch it runs in parallel).
- No change to the first window's content or chrome (the spike's "lighter first window" ideas
  need UI decisions).
- No frame-based cell layout (unmeasured; a later task).
- No change to `WireClient`'s main connection, its validation, or the connect loop.

## Anti-goals

- The prefetch must never show wrong rows. A reply is used only for an identical request; any
  doubt means a normal fetch.
- The prefetch must never delay startup: no waiting on it beyond the moment the real fetch would
  otherwise have been answered, and no retries.
- The prefetch connection must hold the same security line as `WireClient`: validated loopback
  endpoint, token read only after validation, bearer header, redirects rejected, ephemeral session
  with no caches or cookies.

## Launch prefetch

### Shape (Claude's call: a second connection)

A `LaunchPrefetch` object owned by `PlayerModel`, started from `PlayerModel.init` once `endpoint`
and `tokenPath` are resolved (both are set in `init`, including the managed-server case).

- It runs off the main actor. URLSession callbacks and continuations on its own queue; no
  `@MainActor` hop until a fetch claims the result. (The spike showed the main queue is blocked
  until ≈470 ms, so anything on the main actor would not start early.)
- It opens its own `URLSessionWebSocketTask` to the same endpoint, waits for the server's
  protocol v9 `hello`, sends one `list_tracks` request (below), waits for that request's reply,
  keeps the raw reply bytes with the hello's `instance_id` and the latest `history_revision` it saw
  (the hello snapshot's, or a newer `state` message's before the reply), and closes the socket. It
  ignores every other server message.
- It gives up silently on any error, on a non-ok reply, or after 2 s in total. Giving up means the
  slot is empty.
- It reuses `WireClient`'s endpoint validation, token reading and redirect rejection (make the
  needed helpers callable off the main actor rather than duplicating them).
- It never retries and never starts a managed server. On a first launch where the server is not
  running yet, the connect fails fast and the app behaves as today.

### The request

The exact request the Songs list's first fetch would send at launch:

- `command: "list_tracks"`, `list:` the Songs `ListDescription` as `ListModel` would build it at
  launch (library source, the persisted Songs grouping, and whatever sort, browser and query the
  first fetch uses), `revision: null`, `offset: 0`, `limit: 200` (the `ListWindowCache` window
  size).
- Build the description through the same code `ListModel` uses, not a copy. If that code needs
  inputs only available later (for example a `NavigationModel`), extract a pure function that
  both call with the same persisted inputs.

### The claim (one-shot)

`ListModel.fetch` asks the player for the prefetched reply before sending. It gets it only when
all hold:

- it is the first claim since launch (the slot empties on the first claim, used or not);
- the fetch's encoded fields (`list`, `revision`, `offset`, `limit`) equal the prefetch's, compared
  as `JSONValue` (structural equality, not string bytes);
- the prefetch finished with an ok reply;
- the reply is fresh for the main connection (below).

If the prefetch has finished at the claim, the fetch checks it and uses it or sends normally. If
the prefetch is still in flight, the fetch sends its normal request at once and races it with the
prefetch: the first ok reply wins (a prefetched reply only if fresh), and the loser's reply is
dropped. Neither is cancelled: the send because cancelling a request closes the shared
connection, the claim because the prefetch's own 2 s deadline bounds it. A failed send waits for
the prefetch; a stale or failed prefetch waits for the send. In the normal case the prefetch is a
clear win. In two rare cases it can make the first window later than a plain fetch, by at most the
2 s deadline: the server's library worker is single-threaded, so in the race the normal send queues
behind the prefetch query (or the reverse), and a stale prefetch's query delays the send; and a
failed send waits up to the prefetch deadline before it rethrows. Decoding the claimed bytes goes through the same
`ListTracksResponse` decode as a normal reply.

Freshness: `ListModel` takes its library and history revisions from the main connection's
snapshot, and the window cache never compares a page's revision with them. A page from before a
library move (a scan finishing at launch) or a history move (a track ending changes play counts)
would never be refreshed. So a prefetched reply is used only when the prefetch's hello
`instance_id` equals the main connection's, the page's `revision` equals the main snapshot's
`library.revision`, and the prefetch's last-seen `history_revision` equals the main snapshot's
`history_revision`. Any mismatch means a normal fetch.

The pure parts (slot state, one-shot claim, request equality, the freshness rule) live in
TyrannyLikesSmokeCore with unit tests.

## Cheaper cells

In `TrackCellView` (`macos/Sources/TyrannyLikesSmoke/Lists/VirtualTrackTable.swift` ≈575):

- One shared `NSImage` for the playing indicator (a `static let`), with its symbol configuration
  applied once. `init` no longer calls `NSImage(systemSymbolName:)`.
- The image view gets the image only when the cell shows the playing state; non-playing cells
  have no image. Accessibility label "Playing" stays on the shown image.
- Behaviour and appearance are unchanged.

## No redundant reload

In `Coordinator.update` (≈205–211): when the table had zero rows before
`noteNumberOfRowsChanged()` and has rows after it, the new row views were just configured from the
current data, so skip `reloadVisible()` for that pass. Every other path keeps its reload.

## Budgets in `scripts/scale_gate.py`

- Server launches (`start()`, ≈205): also time the WebSocket `hello` and the first Songs
  `list_tracks` (library source, offset 0, limit 200) per launch, and record `ready_ms`, `hello_ms`,
  `first_page_ms`, `total_ms`. Assert ready ≤ 300, hello ≤ 50, first page ≤ 250, total ≤ 500 —
  only with `--server-profile release`; with a debug server, record without asserting and say so
  in the report's limitations (the debug first page is ≈315 ms).
- Client (`native_run()`, ≈245): assert `startup.process_to_model_ready_ms` ≤ 1500 for warm runs.
  This field is written before the Accessibility steps, so the assertion must not depend on them.
  Keep the existing `warm_native_first_usable_ms <= 1000` target where Accessibility runs.
- Report the new targets in the report's `targets` block in the existing style.

## Verification

- TyrannyLikesSmokeCore unit tests for the claim: first claim only; equal fields claim; any field difference
  falls back; an in-flight prefetch is awaited up to its deadline; an empty or failed slot falls
  back; structural (not byte) equality.
- Swift debug and release builds with `-warnings-as-errors`; `swift test`;
  `client_gate.py --model-only` (it drives `PlayerModel`, so a broken claim path shows there).
- Measurement: rerun the spike's uninstrumented comparison — debug client with `--scale-output`
  against a release server and a 50k fixture (`target/spikes/client-startup/seed.py`,
  `target/spikes/quiet-jade/native.py`), n ≥ 8 warm, `dev` vs this branch in the same pass. Expect
  roughly −100 ms or better on `process_to_model_ready_ms`; report the numbers whatever they are.
- `scale_gate.py --queries-only --server-profile release` runs the server budgets without
  Accessibility (if that mode starts servers; otherwise state what ran). The full native gate needs
  Accessibility and Screen Recording and is the user's to run.
- Standard gates: `cargo fmt --check`, clippy `-D warnings`, `cargo test` (no Rust change expected).
