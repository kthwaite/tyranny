# Keybindings and Search Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Any key can run any command, and `/`, `n` and `N` jump between matches in a track list.

**Architecture:** `KeyChord`, `Keymap`, `KeymapStore` and `KeymapEditor` in `TyrannyLikesSmokeCore` hold every decision. A local key monitor runs commands through `NavigationModel.submitCommand`. Search adds the `find_in_list` wire command, a `.search` palette mode and `ListModel.search`.

**Tech Stack:** Swift 6.2, SwiftUI/AppKit, Swift Testing; Rust, rusqlite.

**Spec:** `docs/superpowers/specs/2026-11-05-keybindings-and-search-design.md`

## Global Constraints

- Work in worktree `/Users/kit/Source/_p.music/tyranny-keys-search`. Tasks 1–4 are on `feature/loamy-timber-keybindings`. Tasks 5–7 are on `feature/sodden-log-search`, made from `dev` after Part 1 merges.
- Run only your task's targeted tests: `cd macos && swift test --filter <Suite>`, or `cargo test --locked <name>`. The controller runs the full gates.
- Match the surrounding code: comment density, naming and idiom. Tests use Swift Testing (`@Test`, `#expect`), as the existing `macos/Tests/TyrannyLikesSmokeCoreTests` do.
- `Core` means `macos/Sources/TyrannyLikesSmokeCore`. `App` means `macos/Sources/TyrannyLikesSmoke`. `Tests` means `macos/Tests/TyrannyLikesSmokeCoreTests`.
- Do not change `VERSION` in `src/protocol.rs` or `WireProtocol.version`. The bump happens in the merge into `dev`.
- Plain or ⌥ chords must never become menu shortcuts: menu keys beat text fields.
- End commit messages with no attribution lines.

## Review Focus

- Key pressed while a filter, rename field or palette input has focus: plain keys type; `n` must not jump. Pinned in Task 2's `KeyDispatch` tests.
- Non-US layouts and Caps Lock: Caps Lock with `n` stays `n`; a Shift-produced `:` or `/` is read as that character. Pinned in Task 1.
- User removes a default chord, then a later version adds a new default: the removal survives and the new default appears. Pinned in Task 1.
- Search in a grouped list or with a sort: the position returned must be the same position `list_tracks` uses. Pinned in Task 5 with a grouped list.
- Playlist with one track twice: `row_id` is the entry, and `n` moves to the second occurrence. Pinned in Task 5.

---

## Part 1: Keybindings

### Task 1: KeyChord, Keymap and KeymapStore

**Files:**
- Create: `Core/KeyChord.swift`, `Core/Keymap.swift`
- Test: `Tests/KeyChordTests.swift`, `Tests/KeymapTests.swift`

**Interfaces:**
- Produces:
  ```swift
  public struct KeyChord: Hashable, Sendable {
      public struct Modifiers: OptionSet, Hashable, Sendable { control, option, shift, command }
      public let key: String          // "l", "N", "/", or a named key: "left", "f5", "return"
      public let modifiers: Modifiers // .shift only with a named key
      public init?(_ canonical: String)            // nil when unreadable
      public var canonical: String                 // ctrl+opt+shift+cmd+key
      public var display: String                   // "⌥⌘E", "⌘→", "n"
      public var isNamedKey: Bool
      public static func event(characters: String?, keyCode: UInt16, modifiers: Modifiers, capsLock: Bool) -> KeyChord?
  }
  public struct KeyBinding: Equatable, Sendable { public let chord: KeyChord; public let command: String; public let isCustom: Bool }
  public enum Keymap {
      public static let defaults: [(chord: String, command: String)]
      public static func reservedReason(_ chord: KeyChord) -> String?
  }
  @MainActor @Observable public final class KeymapStore {
      public init(defaults: UserDefaults = .standard)
      public static let defaultsKey = "keymapChanges"
      public private(set) var bindings: [KeyBinding]  // sorted by display group: defaults order, then custom by canonical
      public func command(for chord: KeyChord) -> String?
      public func chord(forCommand command: String) -> KeyChord?  // normalised compare
      public func bind(_ chord: KeyChord, to command: String)     // normalises with CommandHistory.normalised
      public func remove(_ chord: KeyChord)  // a default gets a tombstone; a custom chord is dropped
      public func resetAll()
  }
  ```

- [ ] **Step 1: Write failing tests** in `KeyChordTests`:
  - round trip: `KeyChord("cmd+opt+e")?.canonical == "opt+cmd+e"` (order normalised); `KeyChord("cmd++")?.key == "+"`; `KeyChord("shift+f5")?.canonical == "shift+f5"`; `KeyChord("shift+n") == nil` (Shift with a character key); `KeyChord("") == nil`; `KeyChord("hyper+x") == nil`.
  - display: `opt+cmd+e` → `⌥⌘E`; `cmd+right` → `⌘→`; `n` → `n`; `N` → `N`.
  - `event(characters: "n", keyCode: 45, modifiers: [], capsLock: true)?.canonical == "n"`; `event(characters: "N", keyCode: 45, modifiers: [], capsLock: true)?.canonical == "n"`; `event(characters: "N", keyCode: 45, modifiers: [.shift], capsLock: false)?.canonical == "N"`; `event(characters: "n", keyCode: 45, modifiers: [.shift, .command], capsLock: false)?.canonical == "cmd+N"`; `event(characters: ":", keyCode: 41, modifiers: [.shift], capsLock: false)?.canonical == ":"`; `event(characters: "\u{F702}", keyCode: 123, modifiers: [.command], capsLock: false)?.canonical == "cmd+left"`; `event(characters: "\u{F702}", keyCode: 123, modifiers: [.shift], capsLock: false)?.canonical == "shift+left"`; keypad Enter (76) and Return (36) are both `return`; empty characters with an unknown key code give nil.

  In `KeymapTests`:
  - every default chord parses and every default command passes `CommandLanguage.parse`, except `equalizer` and `lyrics toggle`, which parse after Task 3 (Task 3 removes the exception);
  - defaults include `cmd+1` → `go songs` and `cmd+9` → `go radio`;
  - `reservedReason`: non-nil for `cmd+q`, `cmd+w`, `cmd+h`, `cmd+m`, `cmd+,`, `cmd+a`, `cmd+c`, `cmd+v`, `cmd+x`, `cmd+z`, `cmd+Z`, `space`, `return`, `escape`, `tab`, `delete`, `forwarddelete`, `left`, `shift+down`, `:`; nil for `cmd+left`, `opt+space`, `n`, `/`, `cmd+l`;
  - store with an isolated `UserDefaults(suiteName:)`: `bind` a custom chord and read it back from a new store; `remove` a default leaves `command(for:)` nil in a new store; **a tombstoned default stays removed while a default not in the changes still appears** (simulate a new default by writing changes that name only one chord, then checking the other defaults); `bind` over a default marks it custom; `resetAll` restores defaults and clears the key; an unreadable chord in stored changes is ignored; `chord(forCommand: ":reveal-playing")` finds `cmd+l`; `bind(chord, to: ":seek +1m")` stores `seek +1m`.

- [ ] **Step 2: Run to verify failure.** `cd macos && swift test --filter 'KeyChordTests|KeymapTests'`. Expected: compile failure, missing types.
- [ ] **Step 3: Implement.** Named keys by key code: 123 left, 124 right, 125 down, 126 up, 49 space, 36 and 76 return, 53 escape, 48 tab, 51 delete, 117 forwarddelete, 115 home, 119 end, 116 pageup, 121 pagedown, and the F1–F20 key codes. Display symbols: ← → ↓ ↑ Space ↩ ⎋ ⇥ ⌫ ⌦ ↖ ↘ ⇞ ⇟ F5. Parse by splitting on `+` with the final key token taken from what remains, so `cmd++` works. Case applies only to single letter keys. Store changes as `[String: String]` under `defaultsKey`; an empty value is a tombstone.
- [ ] **Step 4: Run to verify pass.** Same command. Expected: PASS.
- [ ] **Step 5: Commit.** `git add` the four files; message `Add key chords and the persisted keymap`.

### Task 2: KeyDispatch and the main-window key probe

**Files:**
- Create: `Core/KeyDispatch.swift`, `App/Shell/KeymapKeyProbe.swift`
- Modify: `App/Models/NavigationModel.swift`, `App/Shell/MainWindowView.swift`, `App/CommandDispatcher.swift` only if needed for failures
- Test: `Tests/KeyDispatchTests.swift`

**Interfaces:**
- Consumes: Task 1's `KeyChord`, `KeymapStore`.
- Produces:
  ```swift
  public enum KeyDispatch {
      /// Whether `chord` may run a bound command.
      public static func shouldRun(_ chord: KeyChord, isMainWindow: Bool, hasSheet: Bool, overlayOpen: Bool, isEditingText: Bool) -> Bool
  }
  /// Runs key commands one at a time, in order. A failure goes to `report`.
  @MainActor public final class KeyCommandQueue {
      public init(run: @escaping @MainActor (String) async throws -> Void, report: @escaping @MainActor (String) -> Void)
      public func enqueue(_ command: String)
  }
  ```
  `NavigationModel` gains `let keymap: KeymapStore` (built from its `defaults`, like `palette`).

- [ ] **Step 1: Write failing tests** in `KeyDispatchTests`:
  - `shouldRun` false when not the main window, with a sheet, or with an overlay open, for `cmd+l` and `n`;
  - editing text: `n`, `/`, `opt+x`, `ctrl+a` false; `cmd+left`, `cmd+right`, `cmd+up`, `cmd+down`, `cmd+delete`, `cmd+forwarddelete` false; `cmd+f`, `cmd+l`, `cmd+opt+e` true;
  - not editing: `n`, `opt+x`, `cmd+left` true.
  - `KeyCommandQueue`: enqueue `a`, `b`, `c` with a `run` that suspends; order is a, b, c and `b` does not start until `a` ends; a throwing `run` reports its `localizedDescription` and the next command still runs.

- [ ] **Step 2: Run to verify failure.** `swift test --filter KeyDispatchTests`.
- [ ] **Step 3: Implement** both types. Implement `KeymapKeyProbe: NSViewRepresentable` in the same style as `CommandPaletteKeyProbe` (`App/Shell/CommandPaletteView.swift`): a hit-transparent `ProbeView` with a local `.keyDown` monitor while it has a window. It builds a chord with `KeyChord.event`, from `event.charactersIgnoringModifiers`, `event.keyCode`, and modifier flags with `.capsLock` passed separately. It returns nil (consumed) only when `KeyDispatch.shouldRun` is true and the keymap has a command. In `MainWindowView`, add `.background(KeymapKeyProbe(...))` next to the palette probe, with `overlayOpen` = `navigation.wrappedYear != nil || navigation.palette.isOpen`. Build the `KeyCommandQueue` in `onAppear`: `run` is `navigation.submitCommand`, `report` sets `model.message`.
- [ ] **Step 4: Run to verify pass.** `swift test --filter KeyDispatchTests` and `swift build`.
- [ ] **Step 5: Commit.** Message `Run bound commands from the main window's keys`.

### Task 3: New commands and menus that follow the keymap

**Files:**
- Modify: `Core/CommandLanguage.swift` (registry), `App/CommandDispatcher.swift`, `App/Models/NavigationModel.swift`, `App/TyrannyLikesSmokeApp.swift`, `App/Shell/PlayerBar.swift`, `App/Shell/MainWindowView.swift`, `App/Shell/SettingsView.swift` (add `case keys` to `SettingsTab`; pane comes in Task 4)
- Create: `App/Shell/KeyChord+SwiftUI.swift`
- Test: `Tests/CommandLanguageTests.swift`, `Tests/KeymapTests.swift`

**Interfaces:**
- Consumes: `navigation.keymap`.
- Produces: commands `equalizer` (no arguments) and `lyrics <on|off|toggle>`; `settings keys` as a parsed choice. `extension KeyChord { var menuShortcut: KeyboardShortcut? }` is nil unless the chord has ⌘. `NavigationModel` gains `@ObservationIgnored var openEqualizer: (@MainActor () -> Void)?` installed from `MainWindow`, in the same way as its other closures.

- [ ] **Step 1: Write failing tests:**
  - `CommandLanguage.parse("equalizer").id == .equalizer`; `parse("lyrics toggle")` and `parse("settings keys")` parse; `parse("lyrics sideways")` throws.
  - In `KeymapTests`, remove the Task 1 exception: every default command parses.
  - `KeyChord.menuShortcut` is in the app target, which has no test target. Instead add `KeyChord.hasMenuShortcut: Bool` in Core and test it: true for `cmd+l` and `ctrl+cmd+l`; false for `n`, `opt+x`, `ctrl+a`, `shift+f5`.

- [ ] **Step 2: Run to verify failure.** `swift test --filter 'CommandLanguageTests|KeymapTests|KeyChordTests'`.
- [ ] **Step 3: Implement.** `equalizer` throws `CommandLanguageError("Open the library window before opening the Equalizer.")` when `openEqualizer` is nil. `lyrics` sets `player.lyrics.setVisible` from `flag(arguments[0], current: player.lyrics.isVisible)`. Replace every hard-coded `.keyboardShortcut` in the menus of `TyrannyLikesSmokeApp.swift` and on the lyrics toggle in `PlayerBar.swift` with `.keyboardShortcut(navigation.keymap.chord(forCommand: "<command>")?.menuShortcut)`, using the Spec's table for the command text. For Go, build `"go \(destination.key)"`. Replace `(⌥⌘L)` and `(⌥⌘U)` in `.help` strings with the chord's `display`, and drop the parenthesis when unbound. Grep `App` for other hard-coded chords in help text. `installTextFieldKeyGuard` stays as it is. Map `.keys` in `SettingsView` to a placeholder `Text` for one task only, so the build stays exhaustive.
- [ ] **Step 4: Run to verify pass.** Same filter, then `swift build -Xswiftc -warnings-as-errors`.
- [ ] **Step 5: Commit.** Message `Add equalizer and lyrics commands and drive menu shortcuts from the keymap`.

### Task 4: Keys settings pane and editor

**Files:**
- Create: `Core/KeymapEditor.swift`, `App/Shell/KeysSettingsPane.swift`
- Modify: `App/Shell/SettingsView.swift`, `App/NativeCommandSmoke.swift`
- Test: `Tests/KeymapEditorTests.swift`

**Interfaces:**
- Consumes: `KeymapStore`, `Keymap.reservedReason`, `CommandLanguage.parse`.
- Produces:
  ```swift
  @MainActor @Observable public final class KeymapEditor {
      public enum Verdict: Equatable { case incomplete, invalidCommand(String), reserved(String), conflict(command: String), ready }
      public init(store: KeymapStore, editing: KeyBinding? = nil)
      public var chord: KeyChord?
      public var command: String
      public var verdict: Verdict { get }
      public var canSave: Bool { get }   // .ready or .conflict
      public var saveTitle: String { get } // "Save", or "Replace" on conflict
      public func save()   // Removes the edited row's old chord when the chord changed, then binds.
  }
  ```

- [ ] **Step 1: Write failing tests** in `KeymapEditorTests`, with an isolated store:
  - new editor: `.incomplete`, `canSave` false;
  - `chord = cmd+s`, `command = "seek +1q"`: `.invalidCommand` with the parser's message, `canSave` false;
  - `chord = cmd+q`: `.reserved`, `canSave` false, whatever the command;
  - `chord = cmd+l`, `command = "seek +1m"`: `.conflict(command: "reveal-playing")`, `saveTitle == "Replace"`; `save()` rebinds `cmd+l`;
  - editing `cmd+b`'s row and keeping its chord is `.ready`, not a conflict with itself;
  - editing `cmd+b`'s row and moving it to `cmd+j`: after `save()`, `cmd+b` is unbound and `cmd+j` runs `browser toggle`;
  - `command = ":seek +1m"` stores `seek +1m`.

- [ ] **Step 2: Run to verify failure.** `swift test --filter KeymapEditorTests`.
- [ ] **Step 3: Implement the model, then the pane.** `KeysSettingsPane(keymap:)`: a `List` of bindings (chord `display`, monospaced command, a "Custom" label when `isCustom`, a warning when the command no longer parses, Edit and Remove buttons); **Add Binding…** and **Reset to Defaults** with `.confirmationDialog`; an editor sheet with `KeyRecorder`, a command `TextField`, the verdict message using the `error` colour token, and Cancel and Save. `KeyRecorder` is an `NSViewRepresentable` button in the style of `KeymapKeyProbe`: while recording, a local `.keyDown` monitor consumes every key and sets the chord from `KeyChord.event`; Escape cancels recording. Add the `.keys` tab with accessibility identifier `settings-keys`. Add a case to `NativeCommandSmoke` near the palette case: `settings keys` selects the tab, and running `lyrics toggle` twice restores `lyrics.isVisible`.
- [ ] **Step 4: Run to verify pass.** `swift test --filter KeymapEditorTests`, `swift build -Xswiftc -warnings-as-errors`.
- [ ] **Step 5: Commit.** Message `Add the Keys settings pane`.

---

## Part 2: Search mode

### Task 5: `find_in_list` on the server

**Files:**
- Modify: `src/protocol.rs`, `src/library_types.rs`, `src/library/lists.rs`, `src/library/mod.rs`, `src/owner.rs`, `src/bin/tyranny-likes-smoke-ctl.rs` only if it enumerates commands
- Test: `src/library/tests.rs`, `src/protocol.rs` tests

**Interfaces:**
- Produces: `Command::FindInList { list, revision (required), term: String, from: Option<usize>, backward: bool }`, and `LibraryCommand::FindInList` with the same fields. `LibraryReply::ListMatch { revision: u64, found: Option<ListMatch>, wrapped: bool }`, where `pub struct ListMatch { pub position: usize, pub row_id: u64 }`. Wire reply: `kind: "list_match"`, `revision`, `match` (serde rename, nullable), `wrapped`.

- [ ] **Step 1: Write failing tests** in `src/library/tests.rs`, using its list fixtures, on a library sorted by title with `beatles` matching rows at known positions:
  - `find_forward_from_null_returns_first_match`;
  - `find_forward_is_strictly_after_from`;
  - `find_backward_is_strictly_before_from` and `find_backward_from_null_returns_last_match`;
  - `find_wraps_and_says_so`, in both directions; a lone match with `from` at itself returns itself with `wrapped: true`;
  - `find_no_match_returns_none`, with `wrapped: false`;
  - `find_respects_list_query`: a row outside `list.query` never matches, and the position is the position inside the narrowed list;
  - `find_position_matches_list_tracks_when_grouped`: with `Grouping::Album`, the row at the returned position in a `ListTracks` page has the returned `row_id`;
  - `find_duplicate_playlist_entries_are_distinct`: a playlist holding one track twice returns the two entries' `row_id`s in turn;
  - `find_rejects_empty_term` (`""` and `"   "` are `Invalid`), `find_rejects_bad_filter` (the same `invalid_filter` error as `list_tracks`), and a stale revision gives `StaleLibraryRevision`.

  In `src/protocol.rs` tests: the request round trip rejects a missing `revision`, and the reply serialises to `{"kind":"list_match","revision":3,"match":{"position":57,"row_id":812},"wrapped":false}`, and `"match":null`.
- [ ] **Step 2: Run to verify failure.** `cargo test --locked find_`. Expected: compile failure.
- [ ] **Step 3: Implement** `find_in_list(transaction, CompiledList, term, from, backward)` in `lists.rs`. `compile` emits a 1-based `position`. Parse the term with `Filter::parse(term, values.len() + 1)`, so its parameters follow the list's. Query shape:
  ```sql
  SELECT l.position-1,l.row_id FROM ({sql}) l CROSS JOIN library_tracks t ON t.id=l.track_id
  WHERE l.position{op}?{from} AND ({filter}) ORDER BY l.position {ASC|DESC} LIMIT 1
  ```
  Filter SQL refers to the track as `t`. Run it once in the direction, then once with no position bound if nothing was found and `from` was set (that second hit is `wrapped: true`). Add the arm to `execute`'s `(expected, filter)` match with `(*revision, None)`, and route it in `library/mod.rs` and `owner.rs` beside `ListTracks`, including the `unreachable!` list.
- [ ] **Step 4: Run to verify pass.** `cargo test --locked find_`, `cargo clippy --all-targets -- -D warnings`.
- [ ] **Step 5: Commit.** Message `Add find_in_list: the next match in a list's order`.

### Task 6: Search mode in the client

**Files:**
- Create: `Core/ListSearch.swift`
- Modify: `Core/CommandLanguage.swift`, `Core/CommandPaletteModel.swift`, `Core/Keymap.swift`, `Core/WireModels.swift`, `App/Models/ListModel.swift`, `App/Models/NavigationModel.swift`, `App/CommandDispatcher.swift`, `App/Shell/CommandPaletteView.swift`
- Test: `Tests/ListSearchTests.swift`, `Tests/CommandPaletteModelTests.swift`, `Tests/CommandLanguageTests.swift`, `Tests/KeymapTests.swift`, `Tests/WireModelTests.swift`

**Interfaces:**
- Consumes: Task 5's wire shape.
- Produces:
  ```swift
  public struct ListMatchResponse: Decodable, Sendable { revision: UInt64; match: ListMatchRow?; wrapped: Bool }
  public struct ListMatchRow: Decodable, Sendable { position: Int; row_id: UInt64 }
  public enum ListSearch {
      public static func fields(list: ListDescription, revision: UInt64?, term: String, from: Int?, backward: Bool) throws -> [String: JSONValue]
      /// Forward starts after the last selected row, backward before the first; nil with no selection.
      public static func from(selection: IndexSet, backward: Bool) -> Int?
      public static func wrappedMessage(backward: Bool) -> String
      public static func noMatch(_ term: String) -> String
  }
  ```
  `CommandID`: `search`, `searchNext = "search-next"`, `searchPrevious = "search-previous"`. `CommandLanguage.quote` becomes `public`. `CommandPaletteModel.Mode.search`. `Keymap.defaults` gains `/` → `search`, `n` → `search-next`, `N` → `search-previous`. `NavigationModel.lastSearch: String?`. `ListModel.search(term: String, backward: Bool) async throws`.

- [ ] **Step 1: Write failing tests:**
  - `ListSearchTests`: `from(selection: [4, 9], backward: false) == 9`; `from(selection: [4, 9], backward: true) == 4`; `from(selection: [], backward: …) == nil`; `fields` holds `term`, `backward`, a null `from` when nil and a null `revision` when nil; messages are "Search wrapped to the top." (forward), "Search wrapped to the bottom." (backward) and `No match for “x”.`.
  - `WireModelTests`: decodes `{"kind":"list_match","revision":3,"match":{"position":57,"row_id":812},"wrapped":true}` and a null `match`.
  - `CommandLanguageTests`: `search`, `search some term`, `search-next`, `search-previous` parse; `search` has an optional text argument.
  - `KeymapTests`: `/`, `n` and `N` are bound, none is reserved, and `KeyDispatch` refuses them while editing.
  - `CommandPaletteModelTests`:
    - `open(.search)`: text is `/`, dropdown is `.hint`;
    - Return with `/beatles` calls `submit` with `search beatles`, and closes on success;
    - a term needing quotes (`/a "b" c`) is submitted through `CommandLanguage.quote` and round-trips through `parse` to exactly that term;
    - a thrown error stays inline and the text is kept; `history.entries` is unchanged after a search succeeds;
    - Return with an empty term (`/`) and an injected `lastSearch` of `x` submits `search-next`; with none, it closes without submitting;
    - Tab does nothing; Esc closes;
    - in `.commands` mode, `:search` with no term switches to `.search` mode rather than submitting (as `:history` does); `:search x` submits as normal and is recorded.
- [ ] **Step 2: Run to verify failure.** `swift test --filter 'ListSearchTests|WireModelTests|CommandLanguageTests|KeymapTests|CommandPaletteModelTests'`.
- [ ] **Step 3: Implement.** The palette gets `@ObservationIgnored public var hasLastSearch: @MainActor () -> Bool = { false }`, installed by `NavigationModel`. In the view, show a leading hint from `.hint` as today; no view change beyond this is expected, because the input is shared. `CommandDispatcher`:
  - `.search` with no argument calls `navigation.openCommandPalette(.search)`; with one, `try await search(term, backward: false)`;
  - `.searchNext` and `.searchPrevious` throw "No previous search." when `navigation.lastSearch` is nil;
  - `search` throws "Search requires a track list." when `currentList` is nil, then `requireConnection`, then sets `navigation.lastSearch` to the term once the request is accepted (also for no-match, so `n` repeats it) and calls `list.search`.

  `ListModel.search`:
  - sends `find_in_list` with `description`, `cache.revision`, and `ListSearch.from(selection: selectedPositions, backward:)`;
  - on `stale_library_revision`, calls `sync()` and throws "The list changed. Search again.";
  - on `invalid_filter`, throws its message;
  - with no match, throws `CommandLanguageError(ListSearch.noMatch(term))`;
  - with a match, calls `rows.select(IndexSet(integer: position), rowID: { $0 == position ? row_id : nil }, basis: selectionBasis)`, sets `revealRequest` with the position, and sets `player.message` when `wrapped`.
- [ ] **Step 4: Run to verify pass.** Same filter, then `swift build -Xswiftc -warnings-as-errors`.
- [ ] **Step 5: Commit.** Message `Add vi-style search: /, n and N`.

### Task 7: Search against a real server

**Files:**
- Modify: `App/NativeCommandSmoke.swift`, and `scripts/client_gate.py` only if it needs more fixture rows

**Interfaces:**
- Consumes: Task 6's commands.

- [ ] **Step 1: Add a smoke case** `search-selects-without-filtering`, near the palette case. On Songs with the gate's fixture library:
  - note the list total; run `search <known fixture title>`; check that one row is selected, the total is unchanged and `navigation.filter` is empty;
  - run `search-next` and check it wraps or moves according to the fixture;
  - run `search zzzz-nothing` and check it throws the no-match message and leaves the selection unchanged;
  - run `go albums`, then `search-next`, and check "Search requires a track list."
- [ ] **Step 2: Run.** `python3 scripts/client_gate.py --commands-only` after building per the script's usage. Expected: passes with the new case listed. The controller applies the protocol bump to this branch's working state only at merge, so run this on the branch's existing version.
- [ ] **Step 3: Commit.** Message `Exercise search in the command smoke`.
