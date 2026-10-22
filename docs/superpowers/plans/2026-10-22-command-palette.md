# Command Palette Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `:` command palette with dropdown completion, Tab-accept, inline errors and a `:history` picker, on top of the existing command language.

**Architecture:** All behaviour lives in `TyrannyLikesSmokeCore`, where `swift test` can reach it. `CommandLanguage` gains richer suggestions and a usage hint. `CommandHistory` is a value type persisted in `UserDefaults`. `CommandPaletteModel` is an `@Observable` state machine with injected closures. The app target only renders the model, forwards keys, and wires `NavigationModel`, `CommandDispatcher`, the menu and the `:` key.

**Tech Stack:** Swift 6.2 package, SwiftUI + AppKit, macOS 14, Swift Testing (`import Testing`, `@Suite`, `@Test`, `#expect`).

**Spec:** `docs/superpowers/specs/2026-10-22-command-palette-design.md`. Read it before each task. Goal numbers below refer to it.

## Global Constraints

- Branch `feature/basic-puppet-command-palette` from `dev`; merge back into `dev` with `--no-ff`. No protocol `VERSION` bump (no wire change).
- Every task ends green on: `(cd macos && swift build -Xswiftc -warnings-as-errors)` and `(cd macos && swift test)`.
- History: newest first, cap `100`, `UserDefaults` key `"commandHistory"`, entries trimmed and without the leading `:`; `history` itself never recorded; only successful commands recorded.
- `history` registry summary, exactly: `"Pick a previous command."`
- Matching is by prefix only (existing `CommandLanguage.completions` behaviour). Do not change parse or completion results for existing input.
- `:` must never be a menu key equivalent (see `installTextFieldKeyGuard` in `TyrannyLikesSmokeApp.swift`).
- Native UI gates (`client_gate.py --app` scenarios) cannot run here (no Accessibility / Screen Recording). Do not claim visual verification.
- Code style: match surrounding files. Doc comments in short plain sentences, as in `CommandLanguage.swift` and `NavigationModel.swift`.

## Review Focus

1. Reopening the palette while an earlier command is still in flight, and that command then fails or succeeds: the error must go to `report`, not to the new palette; success must still be recorded. (Task 3 test `staleRunReportsAndRecordsWithoutTouchingNewSession`.)
2. Typing `:` in the toolbar filter, a rename field, or the palette's own field must insert `:` and not open the palette. (Task 4 test `triggerIgnoresTextEditingSheetsAndOtherWindows`.)
3. Quoted completions, e.g. a playlist named `Road Trip`: Tab must produce `:playlist-play "Road Trip"` and Return must parse it. (Task 1 test `suggestionsCarryQuotedLinesAndBareValues`.)
4. `sort none` takes no direction: Tab on `none` must not add a trailing space. (Task 1 test `continuesFollowsRemainingArguments`.)
5. Duplicate and colon-variant history entries (`:go albums`, ` go albums `, `go albums`) must collapse to one entry `go albums`. (Task 2 test `recordNormalisesAndDeduplicates`.)

---

### Task 1: Suggestions, usage hint and the `history` command

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmokeCore/CommandLanguage.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/CommandDispatcher.swift` (exhaustive switch)
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/CommandLanguageTests.swift`

**Interfaces:**
- Produces:
  - `CommandID.history` (raw value `"history"`), in `registry` after `.help` with summary `"Pick a previous command."` and no arguments.
  - `public struct CommandSuggestion: Equatable, Sendable { public let line: String; public let value: String; public let summary: String?; public let continues: Bool }`. `line` is the full replacement line (what `completions` returns today). `value` is the bare candidate (command name or unquoted argument value). `summary` is the registry summary at the command-name stage, else `nil`. `continues` is true when the command takes another argument (optional or required) after this token.
  - `public static func suggestions(_ text: String, playlistNames: [String] = []) -> [CommandSuggestion]`.
  - `completions(_:playlistNames:)` becomes `suggestions(...).map(\.line)`. Its results must not change.
  - `public static func usageHint(_ text: String) -> CommandDefinition?`: the definition when the first token is a complete, known command name followed by whitespace (argument stage); `nil` at the command-name stage or for an unknown name.

- [ ] **Step 1: Write the failing tests** in `CommandLanguageTests`:

```swift
@Test func suggestionsCarryQuotedLinesAndBareValues() {
    let rows = CommandLanguage.suggestions(":playlist-play R", playlistNames: ["Road Trip"])
    #expect(rows == [CommandSuggestion(line: ":playlist-play \"Road Trip\"", value: "Road Trip", summary: nil, continues: false)])
    let names = CommandLanguage.suggestions(":se")
    #expect(names.first == CommandSuggestion(line: ":seek", value: "seek",
        summary: "Seek absolutely, or relatively with a sign; default unit is seconds.", continues: true))
    #expect(names.map(\.value).contains("settings"))
    #expect(CommandLanguage.completions(":se") == names.map(\.line))
}

@Test func continuesFollowsRemainingArguments() {
    #expect(CommandLanguage.suggestions("play").first?.continues == false)
    #expect(CommandLanguage.suggestions("colour ac").first?.continues == true)
    #expect(CommandLanguage.suggestions("colour accent ").first?.continues == false)
    #expect(CommandLanguage.suggestions("sort n").first { $0.value == "none" }?.continues == false)
    #expect(CommandLanguage.suggestions("help").first?.continues == true)   // optional argument
}

@Test func usageHintOnlyAtArgumentStage() {
    #expect(CommandLanguage.usageHint(":seek +1")?.id == .seek)
    #expect(CommandLanguage.usageHint(":seek ")?.id == .seek)
    #expect(CommandLanguage.usageHint(":seek") == nil)
    #expect(CommandLanguage.usageHint(":nope 1") == nil)
}

@Test func historyIsARegisteredCommand() throws {
    #expect(try CommandLanguage.parse(":history") == ParsedCommand(id: .history, arguments: []))
    #expect(CommandLanguage.registry.first { $0.id == .history }?.summary == "Pick a previous command.")
}
```

Also add `("history", .history, [])` to the `cases` list in `parsesEverySemanticActionWithItsArguments` (it asserts every `CommandID` is covered).

- [ ] **Step 2: Run** `cd macos && swift test --filter CommandLanguageTests`. Expected: FAIL (compile errors: `CommandSuggestion`, `suggestions`, `usageHint`, `.history` undefined).

- [ ] **Step 3: Implement** in `CommandLanguage.swift`. Restructure the body of `completions` into `suggestions`. For `continues`: at the command-name stage, `!definition.arguments.isEmpty`. At argument index `i`, `i + 1 < definition.arguments.count`, except `false` for `sort` with value `none`. In `CommandDispatcher.execute`, add `case .history: throw CommandLanguageError("Use :history in the command palette.")` for now; Task 4 replaces it.

- [ ] **Step 4: Run** `cd macos && swift test` and `cd macos && swift build -Xswiftc -warnings-as-errors`. Expected: all pass, no warnings.

- [ ] **Step 5: Commit** `git commit -am "Add command suggestions, usage hint and history command"`.

---

### Task 2: `CommandHistory`

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/CommandHistory.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/CommandHistoryTests.swift`

**Interfaces:**
- Consumes: `CommandLanguage.parse`, `CommandID.history` (Task 1).
- Produces:
  - `public struct CommandHistory: Equatable, Sendable`
  - `public static let limit = 100`, `public static let defaultsKey = "commandHistory"`
  - `public private(set) var entries: [String]` (newest first), `public init(entries: [String] = [])`
  - `public mutating func record(_ text: String)`
  - `public static func normalised(_ text: String) -> String`: trim whitespace, drop one leading `:`, trim again.
  - `public static func isHistoryCommand(_ text: String) -> Bool`: true when `text` parses to `.history`.
  - `public static func load(from defaults: UserDefaults) -> CommandHistory` and `public func save(to defaults: UserDefaults)`. Stored as a `[String]` array under `defaultsKey`. `load` applies the cap and drops empty strings.

- [ ] **Step 1: Write the failing tests.** Use a throwaway suite, `UserDefaults(suiteName: "CommandHistoryTests.\(UUID())")!`, and remove its domain at the end.

```swift
@Test func recordNormalisesAndDeduplicates() {
    var history = CommandHistory()
    history.record(":go albums"); history.record("seek +1m"); history.record(" go albums ")
    #expect(history.entries == ["go albums", "seek +1m"])
}
@Test func recordSkipsEmptyAndHistory() {
    var history = CommandHistory()
    history.record("  "); history.record(":"); history.record(":history"); history.record("history")
    #expect(history.entries.isEmpty)
}
@Test func recordCapsAtLimit() {
    var history = CommandHistory()
    for index in 0..<105 { history.record("volume \(index)") }
    #expect(history.entries.count == 100)
    #expect(history.entries.first == "volume 104")
    #expect(history.entries.last == "volume 5")
}
@Test func roundTripsThroughDefaults() { /* record two, save, load into a new value, expect equal; load from an empty suite gives [] */ }
```

- [ ] **Step 2: Run** `cd macos && swift test --filter CommandHistoryTests`. Expected: FAIL (`CommandHistory` undefined).
- [ ] **Step 3: Implement** `CommandHistory.swift` to the interface above.
- [ ] **Step 4: Run** `cd macos && swift test`. Expected: PASS.
- [ ] **Step 5: Commit** `git add -A macos && git commit -m "Add persisted command history"`.

---

### Task 3: `CommandPaletteModel`

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/CommandPaletteModel.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/CommandPaletteModelTests.swift`

**Interfaces:**
- Consumes: `CommandSuggestion`, `CommandLanguage.usageHint`, `CommandHistory` (Tasks 1–2).
- Produces (all `public`, `@MainActor @Observable final class CommandPaletteModel`):
  - `enum Mode: Equatable, Sendable { case commands, history }`
  - `struct Row: Equatable, Sendable { let line: String; let value: String; let summary: String?; let continues: Bool }`. History rows use `line = ":" + entry`, `value = entry`, `summary = nil`, `continues = false`.
  - `enum Dropdown: Equatable, Sendable { case rows([Row]), hint(usage: String, summary: String), empty }`
  - State, `private(set)` except `text`: `isOpen: Bool`, `mode: Mode`, `text: String`, `dropdown: Dropdown`, `highlighted: Int?`, `error: String?`, `isRunning: Bool`, `history: CommandHistory`.
  - Setting `text` clears `error` and recomputes `dropdown` and `highlighted`.
  - Injected closures, `@ObservationIgnored public var`: `suggest: @MainActor (String) -> [CommandSuggestion]` (default `{ CommandLanguage.suggestions($0) }`), `submit: @MainActor (String) async throws -> Void` (default throws `CommandLanguageError("Open the library window before submitting commands.")`), `report: @MainActor (String) -> Void` (default no-op).
  - `init(defaults: UserDefaults = .standard)` loads history.
  - `func open(_ mode: Mode = .commands)`: fresh session. `isOpen = true`, `isRunning = false`, `error = nil`. Text is `":"` in `.commands` and `""` in `.history`.
  - `func close()`: `isOpen = false`, ends the session.
  - `func moveHighlight(_ delta: Int)`: clamps to the row range; no wrap.
  - `func acceptHighlighted()` (Tab): ignored while `isRunning`. In `.commands`, `text = row.line + (row.continues ? " " : "")`. In `.history`, `mode = .commands; text = row.line`.
  - `func submitText() async` (Return): ignored while `isRunning`. In `.history`, same as `acceptHighlighted()`, or nothing when there are no rows. In `.commands`, `CommandHistory.isHistoryCommand(text)` → `open(.history)`. Otherwise run (below).

Dropdown rule (`.commands`): rows from `suggest(text)` if non-empty, else `.hint` from `usageHint(text)` if non-nil, else `.empty`. In `.history`: rows from `history.entries` with prefix `CommandHistory.normalised(text)`, else `.empty`. `highlighted = 0` when there are rows, else `nil`.

Run algorithm (the session check is the part the tests pin):

```swift
let session = currentSession          // bumped by open() and close()
isRunning = true
do {
    try await submit(text)
    history.record(text); history.save(to: defaults)
    if session == currentSession { close() }
} catch {
    let message = error.localizedDescription
    if session == currentSession { isRunning = false; self.error = message } else { report(message) }
}
```

`isRunning` for a stale session is never written back, so a reopened palette stays idle.

- [ ] **Step 1: Write the failing tests.** Use a fresh defaults suite per test. Build the model with stub closures: `suggest` returning fixed rows or the real `CommandLanguage.suggestions`; `submit` recording calls and optionally throwing; a `CheckedContinuation` gate for the in-flight tests. Tests:
  - `opensWithColonAndCommandRows`: `open()` → `text == ":"`, rows non-empty, `highlighted == 0`.
  - `highlightClampsAtEnds`: with 3 rows, `moveHighlight(-1)` stays 0; `moveHighlight(5)` → 2.
  - `tabAcceptsAndAddsSpaceWhenContinuing`: real suggestions, `text = ":rep"`, `acceptHighlighted()` → `text == ":repeat "` and rows are `["all", "cycle", "off", "one"]` by `value`.
  - `argumentStageWithoutCandidatesShowsHint`: `text = ":seek +1"` → `dropdown == .hint(usage: "seek <[+-]number[s|m|h|%]>", summary: "Seek absolutely, or relatively with a sign; default unit is seconds.")`.
  - `unknownNameShowsEmptyAndReturnShowsError`: `text = ":zz"` → `.empty`. Submitting with the real parser as `submit` (`{ _ = try CommandLanguage.parse($0) }`) → `isOpen`, `error == "Unknown command: zz."`, `text == ":zz"`.
  - `editingClearsError`: after the above, `text = ":z"` → `error == nil`.
  - `successClosesAndRecords`: submit succeeds for `":go albums"` → `!isOpen`, `history.entries == ["go albums"]`, persisted in defaults.
  - `failureIsNotRecorded`.
  - `returnRunsTypedTextNotHighlightedRow`: `text = ":se"`, submit → the stub received `":se"`.
  - `historyCommandOpensPickerWithoutSubmitting`: seed history `["seek +1m", "go albums"]`; `text = ":history"`; submit → `mode == .history`, `text == ""`, stub not called, row values in history order. `text = "go"` filters to `["go albums"]`.
  - `pickingHistoryFillsInputUnrun`: in history mode `submitText()` → `mode == .commands`, `text == ":seek +1m"`, stub not called.
  - `tabAndReturnIgnoredWhileRunning`: gate the stub; start `submitText()` in a `Task`; while it waits, `acceptHighlighted()` and `submitText()` change nothing and the stub has one call.
  - `staleRunReportsAndRecordsWithoutTouchingNewSession`: gate a failing stub; `close()` then `open()` mid-flight; resume with an error → `report` received the message, the new session has `error == nil` and `isRunning == false`. Repeat with success → entry recorded, the new session stays open.

- [ ] **Step 2: Run** `cd macos && swift test --filter CommandPaletteModelTests`. Expected: FAIL (type undefined).
- [ ] **Step 3: Implement** `CommandPaletteModel.swift` to the interface above.
- [ ] **Step 4: Run** `cd macos && swift test` and `cd macos && swift build -Xswiftc -warnings-as-errors`. Expected: PASS, no warnings.
- [ ] **Step 5: Commit** `git add -A macos && git commit -m "Add command palette model"`.

---

### Task 4: Palette UI, `:` key, menu item and dispatcher wiring

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/CommandPaletteTrigger.swift`
- Create: `macos/Sources/TyrannyLikesSmoke/Shell/CommandPaletteView.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/Models/NavigationModel.swift`, `macos/Sources/TyrannyLikesSmoke/Shell/MainWindowView.swift`, `macos/Sources/TyrannyLikesSmoke/CommandDispatcher.swift`, `macos/Sources/TyrannyLikesSmoke/TyrannyLikesSmokeApp.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/CommandPaletteTriggerTests.swift`

**Interfaces:**
- Consumes: `CommandPaletteModel` (Task 3), `CommandLanguage.suggestions` (Task 1).
- Produces:
  - `public enum CommandPaletteTrigger { public static func shouldOpen(characters: String?, hasCommandModifiers: Bool, isPaletteWindow: Bool, hasSheet: Bool, isEditingText: Bool) -> Bool }`. True only for `characters == ":"`, no ⌘/⌃/⌥, the palette's window, no sheet, and no text editing. Shift is allowed.
  - `NavigationModel`: `let palette: CommandPaletteModel`, built from the same `defaults` in `init`. `commandCompletion` is replaced by `commandSuggestions: (@MainActor (String) -> [CommandSuggestion])?`. `func suggestCommand(_ text: String) -> [CommandSuggestion]`, falling back to `CommandLanguage.suggestions`. `completeCommand` becomes `suggestCommand(text).map(\.line)`. `func openCommandPalette(_ mode: CommandPaletteModel.Mode = .commands) { palette.open(mode) }`. In `init`, set `palette.suggest` and `palette.submit` with `[weak self]` to `suggestCommand` and `submitCommand`.
  - `CommandDispatcher.suggestions(_:)`: like `completions`, but returns `CommandLanguage.suggestions(...)`. `case .history: navigation.openCommandPalette(.history)`.

- [ ] **Step 1: Write the failing test** `triggerIgnoresTextEditingSheetsAndOtherWindows`. Table-driven: `(":", false, true, false, false) → true`. Each of these → false: `";"`; `":"` with command modifiers; another window; a sheet; editing text; `nil` characters.
- [ ] **Step 2: Run** `cd macos && swift test --filter CommandPaletteTriggerTests`. Expected: FAIL.
- [ ] **Step 3: Implement the trigger**, then run the test. Expected: PASS.
- [ ] **Step 4: Wire `NavigationModel` and `CommandDispatcher`** as in Interfaces. In `MainWindowView.onAppear`, set `navigation.commandSuggestions = { dispatcher.suggestions($0) }` and `navigation.palette.report = { model.message = $0 }`. Clear both in `onDisappear`, alongside the existing lines.
- [ ] **Step 5: Build `CommandPaletteView`.**
  - It renders `navigation.palette` in a `MainWindowView` `.overlay` (after the Wrapped overlay), only while `palette.isOpen`.
  - A full-window, near-transparent backdrop. A click on it calls `close()`.
  - The panel is centred horizontally, with its vertical centre at one-third of the window height from the top, and about 560 pt wide.
  - Input line: monospaced, with a small `ProgressView` while `isRunning`.
  - Error line: in `ColourToken.error` through the existing theme environment, as other views read tokens.
  - Rows: `value` plus `summary` in secondary text; the highlighted row filled with the accent token. At most 8 rows show, and the list scrolls to keep the highlighted row visible.
  - Hint: the `usage` line and `summary`.
  - The input is an `NSViewRepresentable` wrapping `NSTextField`, focused on appear. Its delegate's `control(_:textView:doCommandBy:)` maps:
    - `insertTab:` → `acceptHighlighted()`
    - `insertNewline:` → `Task { await palette.submitText() }`
    - `moveUp:` / `moveDown:` → `moveHighlight(∓1)`
    - `cancelOperation:` → `close()`

    All of these return `true`. `controlTextDidChange` writes `palette.text`. `updateNSView` pushes `palette.text` back only when it differs, keeping the caret at the end.
- [ ] **Step 6: Detect `:`.** Add a background `NSViewRepresentable` probe in `MainWindowView`, modelled on `SidebarVisibilityProbe`. It installs `NSEvent.addLocalMonitorForEvents(matching: .keyDown)` while it has a window and removes it when it leaves. It calls `CommandPaletteTrigger.shouldOpen` with:
  - `event.charactersIgnoringModifiers`;
  - ⌘/⌃/⌥ in `event.modifierFlags`;
  - `event.window === probe.window`;
  - `probe.window?.attachedSheet != nil`;
  - whether `firstResponder` is an `NSText`.

  On true, and when `navigation.wrappedYear == nil`, it calls `navigation.openCommandPalette()` and swallows the event.
- [ ] **Step 7: Menu item.** In `TyrannyLikesSmokeApp.commands`, inside `CommandGroup(replacing: .textEditing)` after "Filter", add `Button("Command…") { openWindow(id: "main"); navigation.openCommandPalette() }` with **no** `keyboardShortcut`.
- [ ] **Step 8: Run** `cd macos && swift test`, `cd macos && swift build -Xswiftc -warnings-as-errors` and `cd macos && swift build -c release -Xswiftc -warnings-as-errors`. Expected: all pass, no warnings.
- [ ] **Step 9: Commit** `git add -A macos && git commit -m "Add command palette UI, colon key and Command menu item"`.

---

### Task 5: Real-server smoke through the palette, and gates

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmoke/NativeCommandSmoke.swift`

**Interfaces:**
- Consumes: `NavigationModel.palette`, `openCommandPalette` (Task 4).

- [ ] **Step 1: Extend `NativeCommandSmoke.exercise`.**
  - Set `navigation.commandSubmission = { try await commands.execute($0) }`.
  - Open the palette, set `text = ":go albums"`, and `await navigation.palette.submitText()`. Check that `navigation.destination == .albums`, `!palette.isOpen`, and `palette.history.entries.first == "go albums"`.
  - Then `try await commands.execute("history")`. Check that `palette.isOpen && palette.mode == .history`.
  - Then set `text = ":seek +1q"` in a fresh `.commands` session and submit. Check `palette.error != nil` and `palette.isOpen`.
  - Append a case label for each, following the existing `cases` style.
- [ ] **Step 2: Build the app and run the commands gate:** `scripts/build_macos.sh debug && cargo build && python3 scripts/client_gate.py --commands-only --server target/debug/tyranny-likes-smoke-server --app "target/tyranny likes smoke.app"`. Expected: `COMMAND_SMOKE PASS`. If it fails for missing permissions rather than a test failure, record that and continue.
- [ ] **Step 3: Full gates.** Run `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`, both Swift builds, `swift test`, and `python3 scripts/client_gate.py --model-only`. Expected: all pass.
- [ ] **Step 4: Commit** `git commit -am "Exercise the command palette in the command smoke"`.
