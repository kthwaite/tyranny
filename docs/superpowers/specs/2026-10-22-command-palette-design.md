# Command Palette (Vi-Style Command Mode)

Date: 2026-10-22. Scope: macOS client (`macos/`) only. No server or protocol change. Task:
TSK-basic-puppet-q7pb3.

REQUIREMENTS.md, Interface: "Vi style command mode with tab completion". The command language
already exists (TSK-agile-trout-ar44h): `CommandLanguage` in `TyrannyLikesSmokeCore` parses,
validates and completes text; `CommandDispatcher` runs it; `NavigationModel.submitCommand(_:)` and
`completeCommand(_:)` expose both from the mounted window. This task adds the UI on top.

Decisions marked "(user)" were made by the user. Decisions marked "(Claude)" are Claude's call.

## Intent

The user wants to drive the player from the keyboard by typing commands such as `:seek +1m`.
Pressing `:` opens a palette like VS Code's or Raycast's, not a vim bottom bar. Typing narrows a
list of matching commands, and Tab accepts one. Previous commands can be picked back from a
history list and edited before they run.

## Goals

1. **Open.** `:` opens the palette when the main window is key, no sheet is up, and no text field
   has focus (Claude). In the Equalizer or Settings window, `:` does nothing. A **Command…** menu
   item also opens it, for discoverability; it brings the main window forward first (Claude). No
   other key shortcut (Claude).
2. **Placement.** A floating panel centred horizontally, with its centre two-thirds of the way up
   the main window (user). It overlays the window content; it is not a separate window (Claude).
3. **Input line** at the top of the panel, prefilled with `:` and the caret after it (Claude). The
   leading colon is optional to the parser, so deleting it is harmless.
4. **Dropdown** under the input line, filled from `NavigationModel.completeCommand(_:)` on every
   edit.
   - At the command-name stage, each row shows the command name and its registry `summary`
     (Claude).
   - At an argument stage with candidates (fixed choices, playlist names, command names, `system`
     colour), each row shows the candidate value (Claude).
   - At an argument stage with no candidates (`seek`, `volume`, free text), the dropdown is
     replaced by a usage hint: the command's `usage` line and `summary` (Claude). The same hint
     shows when an argument stage that normally has candidates has no matching rows: a typo, no
     saved playlists, or an id argument such as `browser-album <album-id|all>` (Claude).
   - At the command-name stage with no matching rows, the dropdown is empty. Return still runs the
     text, and the parser's "unknown command" error shows inline (Claude).
   - Matching is by prefix, as `CommandLanguage.completions` already does (Claude). `:album` does
     not find `selection-album`.
5. **Highlight.** When the dropdown has rows, the first row is highlighted. ↑ and ↓ move the
   highlight and stop at the ends (user, ends: Claude).
6. **Tab** replaces the input line with the highlighted row's completion line (user). When the
   command takes another argument after the completed token, optional or required, a trailing
   space is added so the dropdown moves to the next argument's candidates or hint (Claude). The
   tokenizer ignores trailing whitespace, so Return still parses. Tab with no rows does nothing.
7. **Return** runs the input line exactly as typed through `NavigationModel.submitCommand(_:)`
   (Claude). It never runs the highlighted row instead.
   - On success the palette closes.
   - On failure (parse error or run error) the palette stays open, keeps the text, and shows the
     error message inline in red under the input line. No modal (Claude). Editing the text clears
     the message.
   - While a command is in flight, a small spinner shows on the input line, and Return and Tab
     are ignored (user: spinner; Claude: keys).
8. **Esc** closes the palette without running anything (Claude). Clicking outside it also closes
   it (Claude).
   - Esc or an outside click while a command is in flight also closes the palette (user). The
     command keeps running and is not cancelled. If it later fails, its error goes to the
     status-bar message (`player.message`) instead of the palette. If it later succeeds, it is
     still recorded in history (user).
   - Reopening the palette while that command is still in flight starts a fresh, idle palette
     (Claude).
9. **History.**
   - `:history` + Return switches the dropdown to a list of previous commands instead of running
     anything (user). The input line clears, and typing filters the list by prefix (Claude). The
     palette model intercepts it: when the text parses to the `history` command, Return switches
     mode and does not call `submitCommand` (Claude).
   - ↑ and ↓ move through the list (user). Tab or Return puts the highlighted entry into the input
     line, unrun, and leaves history mode (user: pick then edit; keys: Claude). Return then runs it
     as in goal 7.
   - Esc in history mode closes the palette (Claude).
   - Only commands that succeeded are recorded. A repeat moves the entry to the top instead of
     adding a duplicate. `history` itself is never recorded. Entries are stored as typed, with
     surrounding whitespace trimmed and the leading colon removed (Claude).
   - Newest first, capped at 100 entries, persisted in `UserDefaults` on the client (Claude).
10. **Registry.** `history` joins `CommandLanguage.registry` with the summary "Pick a previous
    command." so it appears in completion and `:help` (Claude). If `CommandDispatcher` is asked to
    run `history` outside the palette (for example by a future keybinding), it opens the palette in
    history mode (Claude).

## Non-goals

- No fuzzy or substring matching.
- No ↑/↓ history recall on the input line; history is only through `:history`.
- No changes to the grammar, the parser, or `CommandDispatcher` behaviour beyond `history`.
- No user-bindable keys. That is TSK-loamy-timber-1d769.
- `:help` output keeps going to the status-bar message, as today (Claude). The palette rows and
  the usage hint already show summaries.

## Anti-goals

- `:` must never be swallowed while a text field has focus, including the toolbar filter, the
  palette's own input, rename fields and settings fields.
- Menu key equivalents beat text fields in this app (`TyrannyLikesSmokeApp.swift`,
  `installTextFieldKeyGuard`). `:` must therefore **not** be a menu key equivalent. Detect it
  where it cannot fire inside a field editor.
- The palette must never run a command the user did not type or explicitly pick.
- An error must never close the palette or lose the text. The one exception is goal 8: the user
  closes the palette while a command is in flight, and its error goes to the status bar.
- An error must never be lost. It shows in the palette or in the status bar.

## Constraints

- Swift gates: debug and release builds with `-warnings-as-errors`, and `swift test`. Rust gates
  are unaffected but still run: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.
  Also `scripts/client_gate.py --model-only`.
- The native UI gates (`client_gate.py --app`) need Accessibility and Screen Recording permissions
  that this terminal does not have. Behaviour must therefore be testable without UI: the palette's
  state machine (input, rows, highlight, mode, Tab/Return/Esc/arrow handling, error, history) lives
  in a plain model with injected completion and submission closures. The palette view only renders
  it and forwards keys. History storage is a separate type with an injected `UserDefaults`, in the
  style of `NavigationModel(defaults:)`.
- The existing Up Next prompt flow stays as is. `ActivationModel.execute` returns after presenting
  the choice sheet, so the command succeeds and the palette closes before the sheet is used.
- Colours come from the existing colour tokens (`ColourSettings` / `.colourTheme`), so the palette
  follows customised colours. The error line uses the `error` token.
- Merge into `dev` per the repo CLAUDE.md. No protocol `VERSION` bump: there is no wire change.

## Left to the builder

- File layout and type names, within the existing `TyrannyLikesSmokeCore` / `TyrannyLikesSmoke`
  split.
- How to detect `:` outside text fields: a local key monitor, a responder in the track tables, or
  similar. It must satisfy the anti-goals. Keyboard layouts where `:` needs Shift count as `:`.
- Exact visual details: width, row count before scrolling, fonts, corner radius, shadow.
  Follow the mockups in `.superpowers/brainstorm/` (not committed): a dark rounded panel, a
  monospaced input line, and a highlighted row in the accent colour.
- Whether the palette also extends `NativeCommandSmoke` or adds a native smoke. Any native smoke
  is a bonus; it cannot be required to pass in this environment.
