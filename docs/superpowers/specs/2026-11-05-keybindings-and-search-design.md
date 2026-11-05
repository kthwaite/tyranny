# Keybindings and Search Mode

Date: 2026-11-05. Tasks: TSK-loamy-timber-1d769 (keybindings), TSK-sodden-log-q9dz6 (search).
Keybindings are client only. Search adds one wire command, so its merge bumps the protocol.

REQUIREMENTS.md, Interface: "Dynamic keybindings. You can bind a key to any command, :seek +1m for
example" and "Vi / less style search mode".

The user skipped the design questions. Every decision here is Claude's call. The user reads this
document after the fact and can override any of it.

## Intent

The user drives the player from the keyboard. Any key can run any command from the command
language. `/` jumps to a match in the current list, and `n` and `N` move between matches, without
narrowing the list the way the filter does.

## Part 1: Keybindings

### Goals

1. **Chords.** A `KeyChord` is a key plus ⌃, ⌥ and ⌘. Named keys (arrows, Return, F1–F20 and so on)
   also carry ⇧. A character key does not: its case or symbol already says Shift, so `N` and `:` are
   their own keys. Canonical text: `ctrl+opt+shift+cmd+key`, in that order. Examples: `cmd+l`,
   `cmd+opt+e`, `cmd+right`, `shift+f5`, `n`, `N`, `/`. Display text uses ⌃⌥⇧⌘ and an upper-case
   letter when a modifier is present: `⌥⌘E`.
2. **Reading a key press.** Named keys come from the key code. Other keys come from
   `charactersIgnoringModifiers`. Shift with a lower-case letter gives the upper-case letter. Caps
   Lock without Shift gives the lower-case letter, so Caps Lock never turns `n` into `N`.
3. **Default keymap.** It matches today's shortcuts:

   | Chord | Command |
   |---|---|
   | `cmd+f` | `focus-filter` |
   | `cmd+b` | `browser toggle` |
   | `cmd+opt+u` | `drawer toggle` |
   | `cmd+right` / `cmd+left` | `next` / `previous` |
   | `cmd+l` | `reveal-playing` |
   | `cmd+opt+e` | `equalizer` (new command) |
   | `cmd+opt+l` | `lyrics toggle` (new command) |
   | `cmd+1` … `cmd+9` | `go songs`, `albums`, `artists`, `genres`, `recent`, `up-next`, `history`, `stats`, `radio` |

4. **Persistence.** `UserDefaults` holds only the user's changes: a dictionary of chord to command,
   where an empty command means "this default is removed". The effective keymap is the defaults plus
   the changes. New defaults in a later version therefore reach existing users. An unreadable chord
   is ignored.
5. **Dispatch.** A local key monitor on the main window looks up each key press and runs the command
   through `NavigationModel.submitCommand`. Commands from keys run one at a time, in order, so a held
   `volume +5` adds up. A failure goes to `player.message`. Key commands do not join command history.
6. **When a key runs.** Only when the main window is key, no sheet is up, the Wrapped slideshow is
   closed and the palette is closed. While text is being edited (the first responder is an editable
   text view or a field editor, so selectable text such as the lyrics does not count):
   - a chord with no ⌘ stays with the field;
   - ⌘ with an arrow, Delete or Forward Delete stays with the field, even when a menu item has that
     chord;
   - any other ⌘ chord runs, as menu shortcuts do today (⌘F inside the filter).
7. **Menus follow the keymap.** Filter, the Column Browser and Up Next items, Next, Previous, Show
   Playing Track, Equalizer, the Go items and the lyrics toggle show the chord bound to their command,
   or none. Only chords with ⌘ become menu shortcuts. Menu keys beat text fields, so a plain or ⌥
   chord in a menu would stop that key typing. Hard-coded chords in help text follow the keymap too.
   In other windows (Settings, Equalizer), the menus still handle their keys as they do today.
   Custom bindings with no menu item only run in the main window.
8. **Reserved chords** cannot be bound:
   - ⌘Q, ⌘W, ⌘H, ⌘M, ⌘, and the editing keys ⌘A, ⌘C, ⌘V, ⌘X, ⌘Z and ⌘⇧Z (key `Z`);
   - unmodified or Shift-only Space, Return, Keypad Enter, Escape, Tab, Delete, Forward Delete and
     arrows, which belong to the tables;
   - `:`, which opens the palette.
9. **Settings.** A new **Keys** tab (`settings keys`) lists every effective binding: chord, command,
   and whether it is custom. Rows can be edited and removed. **Add Binding…** opens an editor with a
   key recorder and a command field. Validation runs as the user edits:
   - an invalid command shows the parser's message and blocks Save;
   - a reserved chord shows why and blocks Save;
   - a chord already bound shows what it runs, and Save becomes **Replace**.

   **Reset to Defaults** asks first, then clears every change. A stored command that no longer parses
   is flagged in its row, and pressing its key puts the parser's error in the status bar.
10. **New commands.** `equalizer` opens the Equalizer window. `lyrics <on|off|toggle>` sets the lyrics
    panel. The `settings` command gains `keys`.

11. **Accepted behaviour changes.** Keys in the main window now run through `CommandDispatcher`, so
    the default chords match the old shortcuts but differ from them in these cases:
    - ⌘F on Up Next, History and Radio says "This view does not have a filter." and does not focus.
      The Filter menu item still focuses the toolbar field there.
    - ⌘L on Albums and similar views shows a status error instead of nothing, and works on Up Next.
      The menu item and the player-bar title keep their old path.
    - ⌘→ or ⌘← with no entry to move to, or while the model is busy, shows a status message. The
      menu item is disabled in the same case.
    - A local command such as ⌘1 waits behind a server command already running from a key, up to
      that command's deadline, because key commands run one at a time.

### Non-goals

- No `:bind` command. Binding is done in Settings.
- No multi-key sequences such as `gg`.
- No import or export of keymaps.
- `:` is not rebindable, and Space stays play/pause in the tables.

### Anti-goals

- A key must never be swallowed while a text field wants it.
- A command must never run from a key press in a sheet, the Settings window or the Equalizer window.
- The recorder must never trigger the command or menu item bound to the key it records.

## Part 2: Search mode

### Goals

1. **Keys.** `/` opens the search prompt. `n` moves to the next match and `N` to the previous one.
   All three are default keymap entries (`search`, `search-next`, `search-previous`), so they follow
   Part 1's rules and can be rebound.
2. **Commands.** `search [term]`: with a term it searches, and without one it opens the prompt.
3. **Prompt.** The command palette panel in a new `.search` mode. The input starts as `/`. There is no
   dropdown of rows, only a hint: "Jump to a match. Uses filter syntax. n and N repeat." Return runs
   the search. Esc or an outside click closes it and leaves the selection alone. Tab does nothing.
   Return with an empty term repeats the last search, or closes if there is none. Searches do not join
   command history. A no-match or invalid-term error shows inline and keeps the prompt open.
4. **Terms use the filter syntax.** `/beatles`, `/artist:beatles`, `/year:1969 & !live` match exactly
   what the same text would match in the filter field.
5. **Selection, not filtering.** A match selects that one row and scrolls to it. The list's rows and
   the filter do not change.
6. **Direction.** Forward means strictly after the last selected row. Backward means strictly before
   the first selected row. With no selection, forward starts at the top and backward at the bottom.
   `search` moves forward, like vi. `N` is always backward: there is no `?`, so "reverse of the last
   direction" is not needed.
7. **Wrap.** The search wraps at the ends and says so in the status bar: "Search wrapped to the top."
   or "Search wrapped to the bottom." A lone match wraps onto itself.
8. **No match.** The selection does not change. The error is `No match for “term”.` `n` or `N` with no
   earlier search gives "No previous search."
9. **Beyond the loaded windows.** The server finds the match, so an unloaded row is found the same way
   as a loaded one. The client selects its position and asks the table to reveal it, and the table
   loads that window as it scrolls.
10. **Scope.** Track lists: Songs, Recently Added, playlists, and the Artists and Genres detail lists.
    Other views give "Search requires a track list." The album grid, Up Next, History, Radio and
    devices are not searched.
11. **Last term.** Held for the session and shared across views. Each search uses the current view's
    filter, browser selection, sort and grouping.
12. **Stale lists.** The request pins the cache's revision. When the library moved, the list resyncs
    and the error is "The list changed. Search again."

### Wire command

Added to `src/protocol.rs`, with `revision` required and nullable like `list_tracks`:

```json
{"id":1,"command":"find_in_list","list":{…},"revision":3,"term":"beatles","from":41,"backward":false}
```

`from` is a zero-based position or null. The search is strictly after it, or strictly before it when
`backward`. Null starts at the end the direction begins from.

```json
{"id":1,"ok":true,"kind":"list_match","revision":3,"match":{"position":57,"row_id":812},"wrapped":false}
```

`match` is null when no row matches. `wrapped` is true when the match came from going around the end.
`position` is zero-based, like `offset`. An empty or whitespace-only term is `invalid`. A bad term is
`invalid_filter`, as in `list_tracks`. A moved revision is `stale_library_revision`.

### Non-goals

- No incremental search while typing.
- No match highlighting or match count.
- No `?` backward prompt.

### Anti-goals

- Search must never change the filter or the list's rows.
- A failed or cancelled search must never change the selection.
- `/`, `n` and `N` must always type normally in a text field.

## Constraints

- Swift gates: debug and release builds with `-warnings-as-errors`, and `swift test`. Rust gates:
  `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --locked`. Also
  `scripts/client_gate.py --model-only` and `--commands-only`.
- Native UI gates may lack Accessibility and Screen Recording permissions. All decisions live in plain
  `TyrannyLikesSmokeCore` types with tests. Views only render and forward. Anything that can only be
  checked natively goes on the task's hold.
- Protocol: Part 1 changes nothing on the wire. Part 2 bumps `VERSION` from 24 to 25 only in its merge
  into `dev`, with the copies listed in the repo `CLAUDE.md`.
- Each part is its own branch from `dev`, merged in order. Part 2 starts from the merged Part 1.
