# Customisable Colours and Cover-Art Tint

Date: 2026-10-21. Scope: macOS client (`macos/`) only. No server or protocol change. Tasks:
TSK-moist-fir-w5nqf (customisable colours) and TSK-feisty-helmet-649cx (tint from cover art).

REQUIREMENTS.md: "Customizable colors" (Interface) and "UI updates to reflect currently playing
track's cover art colors" (Misc). The user decided **the cover-art tint is on by default**. Every
other decision below is Claude's call and is marked "(Claude)".

## Intent

Today the client uses only system colours and the system accent. The user wants to choose the
colours that carry meaning: accent, playing row, selection, status. They also want the app to
pick up the playing album's colour, the way the cover sets the mood.

## Goals

1. **Colour tokens.** One place decides every meaningful colour: `accent`, `playing`, `selection`,
   `success`, `warning`, `error`. Every current use moves to a token.
2. **Settings → Appearance** tab. Each token has "System" (default) or a custom colour from a
   colour well, plus a "Reset to Defaults" button. It works in light and dark appearance.
3. **Cover-art tint**, on by default, with a toggle in the same tab. While a track with artwork
   plays, `accent` and `playing` follow the artwork's dominant colour, raised to a contrast floor.
4. Colour extraction is cached per artwork and never runs on the main thread.

## Non-goals

- No full themes: backgrounds, text, materials and alternating rows stay system colours.
- No per-appearance custom values. One custom colour applies in both light and dark (Claude).
- No tint of `selection` or status colours (Claude).
- No app-wide `.tint(...)`. `accent` applies only at the call sites in the table below. Stock
  controls (toggles, buttons, pickers, focus rings, the sidebar selection, the seek and volume
  sliders) keep the system accent (Claude).

## Anti-goals

- A tinted colour must never become unreadable in either appearance. Custom colours are drawn as
  picked. Settings warns when one falls below the contrast floor (below).
- The tint must never flicker. It changes only when the playing artwork changes, never on pause or
  seek.
- No visible change for a user who never opens the tab and plays nothing with artwork. With the
  tint off or no artwork, everything looks as it does today.

## Tokens and where they apply

| Token | Default | Current uses that move to it |
|---|---|---|
| `accent` | `controlAccentColor` | Player bar toggles (PlayerBar.swift:144/173/222/233), lyrics active line (LyricsPane.swift:98), drop-target highlight (TrackDrag.swift:40), album grid selected border (AlbumGridItem.swift:164) |
| `playing` | follows `accent` | playing-row speaker icon (VirtualTrackTable.swift:604, UpNextTable.swift:400) |
| `selection` | system selection | table row selection (VirtualTrackTable, AlbumTrackTable, UpNext row views) |
| `success` | `systemGreen` | connection dot "connected" (StatusBar.swift:86-92), import done icon (ImportProgressView.swift:32) |
| `warning` | `systemOrange` | orange warnings (PlayerBar.swift:63, SettingsView.swift:114, PlaylistSheets.swift:170/192/256/323) |
| `error` | `systemRed` | connection dot "failed", red errors (PlayerBar.swift:114, SettingsView.swift:186, RadioView.swift:54/192, EqualizerView.swift:49, UpNextView.swift:96/152, PlaylistSheets.swift:68, MainWindowView.swift:115) |

The line numbers are from the 2026-10-21 survey. The builder greps for every `Color.accentColor`,
`controlAccentColor`, `Color.green/orange/red` and system colour use, not only these lines.

The "connecting" dot stays system yellow outside the tokens (Claude). It is a short-lived state,
not a warning, and mapping it to orange would change the default look.

When a row is selected, the speaker icon uses the row's interior text colour (white on an
emphasized fill) instead of `playing`, so it never disappears into the selection (Claude).

`playing` keeps its own custom value when set. When it is "System", it shows the resolved
`accent`, tint included.

## Selection drawing (Claude)

The tables use plain `NSTableRowView`. Add one shared row-view subclass, `TokenRowView`:

- With `selection` set to "System", draw nothing new; keep the default behaviour.
- With a custom `selection`, override `drawSelection(in:)` to fill with that colour. When the
  table is not first responder, fill at reduced opacity (0.45, Claude).
- Override `interiorBackgroundStyle`: `.emphasized` (light text) when the fill's relative
  luminance is below 0.4, else `.normal` (dark text). Custom colours of any lightness stay
  readable.

`QueueRowView` (UpNextTable) becomes a subclass of `TokenRowView`.

Every `NSTableView` uses `TokenRowView`: the track, album-track, Up Next, History and column
browser tables (Genres, Artists, Albums). The Radio list is a SwiftUI `Table` and keeps the
system selection.

## Resolution

Pure logic in TyrannyCore, which has no AppKit or SwiftUI imports:

- `RGB` (sRGB, components 0...1) with `#RRGGBB` parse and format.
- `ColourSettings`: an optional custom `RGB` per token, plus `tintFromArtwork: Bool` (default
  `true`). It loads from and saves to an injected `UserDefaults`. Keys: `colour.accent`,
  `colour.playing`, `colour.selection`, `colour.success`, `colour.warning`, `colour.error`
  (hex strings, absent means System) and `colour.tintFromArtwork`.
- `Contrast`: WCAG relative luminance, contrast ratio, and
  `raise(_ colour: RGB, against background: RGB, to ratio: Double) -> RGB`. It moves lightness
  in OKLCH (hue and chroma kept, chroma clipped to the sRGB gamut) until the ratio is met.
- `ArtworkPalette.dominant(rgba: [UInt8], width: Int, height: Int) -> RGB?`:
  - ignore pixels that are near-grey (chroma below a threshold), near-black or near-white;
  - bucket the rest by hue (for example 24 buckets);
  - choose the bucket with the greatest count weighted by chroma;
  - return the bucket's mean colour, or `nil` if too few pixels qualify (mostly grey covers).
- `ThemeResolver.resolve(settings:, artColour: RGB?) -> ResolvedTokens`. For each token it gives
  either `.system` or a concrete `RGB`, with this precedence for `accent`:
  1. tint on and an art colour exists: the art colour;
  2. a custom accent;
  3. system.

  The art colour does not override a custom `playing`.

The contrast floor is applied in the app, per appearance, to tinted colours only (Claude). A
dynamic `NSColor` uses `Contrast.raise` with ratio 3.0 against the window background of the
current appearance. 3.0 is WCAG's floor for UI components and large text. Custom colours are
used as the user picked them.

## App side

- `ColourTheme`: an `@MainActor @Observable` class owned by `PlayerModel`. It holds
  `ColourSettings`, the current art colour, and the resolved tokens as `NSColor` and `Color`.
  It is injected into SwiftUI with `.environment(theme)` at the window roots, including Settings
  and the Equalizer window. AppKit tables receive the colours they need as parameters, as they
  receive `playingTrackID` today, and redraw when those change.
- **Art colour:**
  - When `artwork.nowPlaying` changes to a decoded image, `ColourTheme` checks a cache keyed by
    `ArtworkImage.cacheKey` (64 entries, LRU).
  - On a miss, a detached task draws the `CGImage` into a 32×32 RGBA buffer and calls
    `ArtworkPalette.dominant`.
  - The result is applied only if the now-playing artwork is still that key. A stale result
    is dropped.
  - **Clears the art colour:** the playing track's artwork resolves to missing or unavailable
    while connected, or playback stops (nothing playing while connected).
  - **Keeps the previous colour** (Claude): loading, disconnect, reconnect, a server restart, and
    any state where the store has no connection context. A reconnect therefore never flickers.
- **Animation:** token changes from the tint animate over 0.3 s in SwiftUI views (Claude). AppKit
  views just redraw.

## Settings → Appearance

- A new `SettingsTab.appearance` ("Appearance", symbol `paintpalette`). Update the command
  language's tab parsing and the render-views tab list along with it.
- One row per token: label, a "System" / "Custom" popup and a `ColorPicker` enabled for Custom.
  The `playing` row's System option reads "Same as Accent".
- "Tint accent from cover art" toggle, with the footnote "While a track with artwork plays, the
  accent follows its cover."
- "Reset to Defaults" sets every token to System and the tint back on.
- Under each custom colour whose contrast against the window background is below 3.0 in light
  or dark appearance, a footnote names the failing appearance, for example "Low contrast in
  Dark appearance" (Claude).
- Every control has an `accessibilityIdentifier` (`settings-colour-accent` …,
  `settings-tint-from-artwork`, `settings-colours-reset`).

## Commands (Claude)

Every other setting is drivable from the command language, so these are too. They are
added to `CommandLanguage` and `CommandDispatcher` with tests:

- `colour <token> <#rrggbb|system>`: token names are as in the table.
- `colours reset`.
- `tint on|off`.

They also let a native smoke drive the tint.

## Verification

- **TyrannyCore tests (swift-testing):**
  - hex round-trip and rejection;
  - `ColourSettings` load and save with a UserDefaults suite, defaults `tintFromArtwork == true`;
  - contrast ratio known values (black/white = 21);
  - `raise` meets the ratio against both a light (#ECECEC) and a dark (#1E1E1E) background and
    keeps hue within a few degrees;
  - `dominant` on synthetic buffers: solid red returns red; grey returns nil; a 70 % blue / 30 %
    grey image returns blue; a mostly-black image with a small saturated area returns that area's
    hue;
  - resolver precedence, including tint over custom accent, tint off, and no art.
- **Builds and gates:**
  - debug and release builds with `-warnings-as-errors`;
  - `swift test`;
  - `client_gate.py --model-only`.
- **Standard gates:** `cargo fmt --check`, clippy `-D warnings` and `cargo test`. No Rust change
  is expected.
- **Native:**
  - Render the Appearance tab with `--render-views`. This self-renders and needs no permission,
    so Claude runs it and looks at the image.
  - Extend `NativeArtworkSmoke` to check that the resolved accent follows the playing artwork and
    reverts with `tint off`. This and a visual check of selection need Accessibility, so they are
    the user's to run.
- **CommandLanguage tests** for the three new commands.

## Left to the builder

The palette thresholds and bucket count within the description, the exact OKLCH conversion, and
the layout of the Appearance tab.
