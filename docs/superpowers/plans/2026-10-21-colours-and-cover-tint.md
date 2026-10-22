# Customisable Colours and Cover-Art Tint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Six colour tokens (`accent`, `playing`, `selection`, `success`, `warning`, `error`) that the user can set in Settings → Appearance or by command. While a track with artwork plays, `accent` and `playing` follow the cover's dominant colour, which is on by default.

**Architecture:** TyrannyLikesSmokeCore holds the pure logic. That is `RGB` and hex, `ColourSettings` in UserDefaults, `Contrast` (WCAG and an OKLCH lightness raise), `ArtworkPalette` (dominant colour), `ThemeResolver` (precedence) and `ArtTint` (the art-colour state machine and its 64-entry cache). The app adds one `@MainActor @Observable ColourTheme`, which `PlayerModel` owns. SwiftUI views read the tokens from the environment. AppKit tables get a `TableColours` value as a parameter and draw selection through one `TokenRowView`.

**Tech Stack:** Swift 6.2 package (`macos/Package.swift`, macOS 14, strict concurrency), SwiftUI + AppKit, CoreGraphics, swift-testing.

**Spec:** `docs/superpowers/specs/2026-10-21-colours-and-cover-tint-design.md` (binding). Read it before every task.

## Global Constraints

- Work only in the worktree `/Users/kit/Source/_p.music/tyranny-colours`, on branch `feature/colours` (cut from `dev`). At the end it merges into `dev`, never `prod` (`CLAUDE.md`).
- macOS client only. No change to the Rust server, the protocol, or `scripts/*_gate.py`.
- `TyrannyLikesSmokeCore` imports only `Foundation` and `CoreGraphics`. It never imports AppKit or SwiftUI.
- Swift 6 strict concurrency. App models are `@MainActor`. Colour extraction runs in `Task.detached(priority: .utility)` and never on the main thread.
- Tests use swift-testing (`import Testing`, `@Suite`, `@Test`, `#expect`) with `@testable import TyrannyLikesSmokeCore`. A defaults test uses a fresh `UserDefaults(suiteName:)` and removes it afterwards, as `ImportNoteTests.withFreshDefaults` does.
- UserDefaults keys, verbatim: `colour.accent`, `colour.playing`, `colour.selection`, `colour.success`, `colour.warning`, `colour.error` hold `#RRGGBB` strings; an absent key means System. `colour.tintFromArtwork` is a Bool; an absent key means `true`.
- Fixed values:
  - contrast floor `3.0`;
  - contrast backgrounds `#ECECEC` (light) and `#1E1E1E` (dark);
  - unfocused custom selection opacity `0.45`;
  - light text on a fill below relative luminance `0.4`;
  - art cache 64 entries, LRU, keyed by `ArtworkImage.cacheKey`;
  - sample buffer 32×32 RGBA;
  - tint animation `.easeInOut(duration: 0.3)`.
- Copy, verbatim:
  - tab "Appearance", symbol `paintpalette`;
  - row labels "Accent", "Playing", "Selection", "Success", "Warning", "Error";
  - popup items "System" and "Custom"; for the playing row, "Same as Accent" and "Custom";
  - toggle "Tint accent from cover art", footnote "While a track with artwork plays, the accent follows its cover.";
  - button "Reset to Defaults";
  - warnings "Low contrast in Light appearance" and "Low contrast in Dark appearance".
- Accessibility identifiers:
  - `settings-colour-<token>` for each popup, `settings-colour-<token>-well` for each colour well, `settings-colour-<token>-contrast` for each warning;
  - `settings-tint-from-artwork`;
  - `settings-colours-reset`.
- Gate, smoke and render launches must not read or write the user's colours. `ColourTheme.forLaunch(arguments:)` picks the suite with `QuitSuspensionStore.suiteName(forArguments:)`, as `ImportNoteStore.forLaunch` does.
- No app-wide `.tint(...)`. Stock controls keep the system accent. The "connecting" status dot stays `.yellow`.
- Gates every task keeps green:
  - from `macos/`: `swift build -Xswiftc -warnings-as-errors`, `swift build -c release -Xswiftc -warnings-as-errors`, `swift test`;
  - from the repo root: `python3 scripts/client_gate.py --model-only --app "macos/.build/debug/tyranny likes smoke"`.
  - The script's default `--app` is `"target/tyranny likes smoke.app"`, a bundle that only `scripts/build_macos.sh` refreshes. So pass the fresh debug binary. The script needs `target/debug/tyranny-likes-smoke-server`; run `cargo build --locked --bins` if it is missing.
  - Expected: both builds print `Build complete!`, `swift test` reports no failures, and the gate exits 0.
- Commits: one plain imperative sentence, sentence case, no prefix, no attribution or `Co-Authored-By` lines. Stage files by explicit path. Never stage `.claude/`.

## Review Focus

1. **A view outside the window roots.**
   - Cases: `AlbumExpansionView`'s own `NSHostingView`, a sheet, a popover, and render-views' `NSHostingView`.
   - Expected: the view must draw with system colours, never trap on a missing environment object.
   - Pinned by: Task 5 puts the SwiftUI tokens in an `EnvironmentKey` whose default is the system colours. It also greps that no leaf view reads `@Environment(ColourTheme.self)`. Task 6's render of `05-albums-grid-expanded` exercises the hosted panel.
2. **Skipping quickly through several tracks.**
   - Expected: a late extraction for an earlier cover must never paint the accent.
   - Pinned by: Task 4 test `staleExtractionIsCachedButNotApplied`.
3. **Disconnect, reconnect, a server restart or a rescan.**
   - Expected: the tint holds and never flashes to System. Back on the same cover, the cache answers without a new extraction.
   - Pinned by: Task 4 tests `noContextAndLoadingKeepTheColour` and `returningToACachedCoverNeedsNoExtraction`.
4. **Grey and black-and-white covers.**
   - Expected: no muddy grey accent. The tint clears, and `accent` falls back to the custom colour or System.
   - Pinned by: Task 3 tests `greyReturnsNil` and `greyWithAFewColouredPixelsReturnsNil`, and Task 4 test `greyCoverClearsTheColour`.
5. **A hand-edited or corrupt preference.**
   - Examples: `colour.accent = "blue"`, a number, or a non-Bool tint flag.
   - Expected: the token reads as System and the tint as on. Nothing crashes.
   - Pinned by: Task 1 test `malformedStoredValuesReadAsSystem`.

---

### Task 1: `RGB`, `ColourToken` and `ColourSettings`

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/ColourSettings.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/ColourSettingsTests.swift`

**Interfaces:**
- Produces:

```swift
public struct RGB: Hashable, Sendable {
    public let red: Double, green: Double, blue: Double      // sRGB, 0...1
    public init(red: Double, green: Double, blue: Double)    // clamps to 0...1; NaN becomes 0
    public init?(hex: String)                                 // exactly "#" + 6 ASCII hex digits, any case
    public var hex: String                                    // "#RRGGBB", upper case, each component rounded to the nearest 1/255
}
public enum ColourToken: String, CaseIterable, Sendable {
    case accent, playing, selection, success, warning, error
    public var defaultsKey: String                            // "colour." + rawValue
}
public struct ColourSettings: Equatable, Sendable {
    public static let tintKey = "colour.tintFromArtwork"
    public var custom: [ColourToken: RGB]                     // absent = System
    public var tintFromArtwork: Bool
    public init(custom: [ColourToken: RGB] = [:], tintFromArtwork: Bool = true)
    public subscript(token: ColourToken) -> RGB? { get set }
    public init(defaults: UserDefaults)                       // load; an unparseable value is System
    public func save(to defaults: UserDefaults)               // writes hex, removes the key for System
}
```

- [ ] **Step 1: Write the failing tests** in `@Suite struct ColourSettingsTests`:
  - `hexRoundTripsAndNormalisesCase`:
    - `RGB(hex: "#3366ff")?.hex == "#3366FF"`;
    - `RGB(hex: "#000000") == RGB(red: 0, green: 0, blue: 0)`;
    - `RGB(red: 0.5, green: 0.5, blue: 0.5).hex == "#808080"`;
    - for every hex in `["#000000", "#FFFFFF", "#3366FF", "#ECECEC", "#1E1E1E"]`, `RGB(hex: h)?.hex == h`.
  - `hexRejectsAnythingButHashAndSixHexDigits`, with `@Test(arguments:)` over `["", "#", "3366FF", "#3366F", "#3366FFF", "#3366FF00", "#GG0000", " #3366FF", "#3366FF ", "#+12345", "#-12345", "#٣٣٦٦FF", "#３３66FF"]`: `RGB(hex:)` returns nil.
  - `componentsClampToUnitRange`: `RGB(red: 1.5, green: -0.2, blue: .nan) == RGB(red: 1, green: 0, blue: 0)`.
  - `defaultsAreSystemWithTintOn`:
    - `ColourSettings().custom.isEmpty`;
    - `ColourSettings().tintFromArtwork`;
    - `ColourSettings(defaults: fresh) == ColourSettings()`.
  - `saveAndLoadRoundTrip`. Set accent `#3366FF`, selection `#FFE066` and tint false, then save and reload. Expect:
    - the reloaded settings are equal;
    - `defaults.string(forKey: "colour.accent") == "#3366FF"`;
    - `defaults.object(forKey: "colour.playing") == nil`;
    - `defaults.object(forKey: "colour.tintFromArtwork") as? Bool == false`.
  - `savingSystemRemovesTheKey`: set an accent, save, set it to nil, save again. Then `defaults.object(forKey: "colour.accent") == nil`.
  - `malformedStoredValuesReadAsSystem`: set `"colour.accent" = "blue"`, `"colour.error" = 42` and `"colour.tintFromArtwork" = "yes"`. Then `ColourSettings(defaults:) == ColourSettings()`.

- [ ] **Step 2: Run** `cd macos && swift test --filter ColourSettingsTests`. Expected: the build fails because `RGB` is not defined.
- [ ] **Step 3: Implement** the Interfaces block in `ColourSettings.swift`. Check hex digits byte by byte, ASCII `0-9 a-f A-F` only, because `Int(_:radix:)` accepts a sign. Load the tint with `defaults.object(forKey:) as? Bool ?? true`.
- [ ] **Step 4: Run** `cd macos && swift test --filter ColourSettingsTests`. Expected: PASS. Then run the Global Constraints gates.
- [ ] **Step 5: Commit**

```bash
git add macos/Sources/TyrannyLikesSmokeCore/ColourSettings.swift macos/Tests/TyrannyLikesSmokeCoreTests/ColourSettingsTests.swift
git commit -m "Add colour tokens and their saved settings"
```

### Task 2: `Contrast`

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/Contrast.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/ContrastTests.swift`

**Interfaces:**
- Consumes: `RGB` (Task 1).
- Produces:

```swift
public enum Contrast {
    public static let floor = 3.0
    public static let lightBackground: RGB                     // #ECECEC
    public static let darkBackground: RGB                      // #1E1E1E
    public static func luminance(_ colour: RGB) -> Double      // WCAG 2 relative luminance
    public static func ratio(_ a: RGB, _ b: RGB) -> Double     // (lighter + 0.05) / (darker + 0.05), symmetric
    public static func raise(_ colour: RGB, against background: RGB, to ratio: Double) -> RGB
    public static func composite(_ colour: RGB, opacity: Double, over background: RGB) -> RGB  // per component c*o + b*(1-o)
    public static func prefersLightText(on fill: RGB) -> Bool  // luminance(fill) < 0.4
    /// "Low contrast in Light appearance" if ratio(colour, lightBackground) < floor,
    /// else "Low contrast in Dark appearance" if ratio(colour, darkBackground) < floor, else nil.
    public static func warning(for colour: RGB) -> String?
}
struct OKLCH: Equatable { var l: Double; var c: Double; var h: Double  // h in degrees, 0..<360
    init(_ rgb: RGB); func rgb() -> RGB? }                    // nil when the colour is outside the sRGB gamut
```

No colour fails against both backgrounds. That would need luminance above 0.246 and below 0.139 at once.

- [ ] **Step 1: Write the failing tests** in `@Suite struct ContrastTests`, with tolerances as `abs(x - y) < ε`:
  - `blackWhiteIs21`:
    - `ratio(black, white)` is 21 (ε 1e-9);
    - `ratio(white, black) == ratio(black, white)`;
    - `luminance(white) == 1`, `luminance(black) == 0`;
    - `luminance(#808080)` is 0.21586 (ε 1e-4).
  - `oklchKnownValueAndRoundTrip`:
    - `OKLCH(#FF0000)` gives l 0.62796, c 0.25768, h 29.23 (ε 1e-3);
    - `OKLCH(#3366FF).rgb()` returns `#3366FF` within 1e-6 per component.
  - `raiseMeetsTheFloor`:
    - colours `#0000FF`, `#800080`, `#FFFF00`, `#00FF00`, `#FF8000`, `#808080`;
    - backgrounds `lightBackground` and `darkBackground`;
    - expect `ratio(raise(c, against: bg, to: 3), bg) >= 3`.
  - `raiseKeepsHue`: for the same pairs, wherever the result's OKLCH chroma is above 0.02, its hue is within 3° of the input's (compare on the circle).
  - `raiseLeavesPassingColoursUnchanged`: `raise(#FF0000, against: darkBackground, to: 3) == #FF0000`.
  - `compositeAndTextChoice`:
    - `composite(black, opacity: 0.45, over: white)` gives (0.55, 0.55, 0.55);
    - `prefersLightText` is true on black and on `#1A4D99`;
    - it is false on white and on `#FFE066`.
  - `warningNamesTheFailingAppearance`:
    - `warning(for: #FFFF00) == "Low contrast in Light appearance"`;
    - `warning(for: #000080) == "Low contrast in Dark appearance"`;
    - `warning(for: #808080) == nil`.

- [ ] **Step 2: Run** `cd macos && swift test --filter ContrastTests`. Expected: the build fails because `Contrast` is not defined.
- [ ] **Step 3: Implement** in `Contrast.swift`:
  - **Linearisation:** WCAG sRGB. Divide by 12.92 at or below 0.04045, else use `((c + 0.055) / 1.055)^2.4`.
  - **OKLCH:** Ottosson's OKLab with the sRGB D65 matrices.
  - **`raise` algorithm:**
    1. Return `colour` if it already meets `ratio`.
    2. Darken when `ratio(black, bg) >= ratio(white, bg)`; otherwise lighten.
    3. Bisect OKLCH `l` between the colour's `l` and 0 (or 1) for 32 steps. Keep the value nearest the original that meets `ratio`.
    4. At each `l`, keep `h`. Use the largest chroma at or below the original that `rgb()` accepts (bisect 20 steps).
    5. If even the end point fails, return black or white.
- [ ] **Step 4: Run** `cd macos && swift test --filter ContrastTests`. Expected: PASS. Then run the gates.
- [ ] **Step 5: Commit**

```bash
git add macos/Sources/TyrannyLikesSmokeCore/Contrast.swift macos/Tests/TyrannyLikesSmokeCoreTests/ContrastTests.swift
git commit -m "Add WCAG contrast and an OKLCH lightness raise"
```

### Task 3: `ArtworkPalette`

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/ArtworkPalette.swift` (imports `CoreGraphics`)
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/ArtworkPaletteTests.swift`

**Interfaces:**
- Consumes: `RGB` (Task 1).
- Produces:

```swift
public enum ArtworkPalette {
    public static let sampleSize = 32
    /// `rgba` is premultiplied RGBA8, row-major, `width * height * 4` bytes; any other count returns nil.
    public static func dominant(rgba: [UInt8], width: Int, height: Int) -> RGB?
    /// Draws `image` into a 32×32 sRGB premultiplied-last RGBA8 buffer, then calls `dominant(rgba:width:height:)`.
    public static func dominant(image: CGImage) -> RGB?
}
```

- **Thresholds**, the builder's choice within the spec:
  - skip a pixel with alpha < 128; un-premultiply the rest;
  - skip near-black (max component < 0.15) and near-white (min component > 0.90);
  - skip near-grey (HSV chroma, max − min, < 0.15);
  - use 24 HSV hue buckets, weighted by the sum of chroma;
  - return nil when fewer than `max(1, 3% of all pixels)` qualify;
  - return the arithmetic mean of the winning bucket's components.

- [ ] **Step 1: Write the failing tests** in `@Suite struct ArtworkPaletteTests`. Use a helper that builds a 32×32 buffer from `(x, y) -> (UInt8, UInt8, UInt8)` with alpha 255. Compare `RGB` results per component within 1e-9, because a mean of equal values need not round-trip exactly.
  - `solidRedReturnsRed`: `dominant == RGB(red: 1, green: 0, blue: 0)`.
  - `greyReturnsNil`: an all-`#808080` buffer gives nil, and so does an all-black one.
  - `greyWithAFewColouredPixelsReturnsNil`: 10 red pixels (under 1 %) on `#808080` give nil.
  - `seventyPercentBlueBeatsGrey`: rows `0..<22` (69 %) are `#0000FF` and the rest `#808080`. The result is `RGB(red: 0, green: 0, blue: 1)`.
  - `smallSaturatedAreaOnBlackWins`: an 8×8 `#FF8000` square on black gives `RGB(hex: "#FF8000")`.
  - `wrongBufferSizeReturnsNil`: a 10-byte buffer, and a width of 0.
  - `cgImagePathSamplesTheImage`: a 64×64 solid `#0000FF` `CGImage` (made with a `CGContext`). `dominant(image:)` is within 1/255 of blue per component.
- [ ] **Step 2: Run** `cd macos && swift test --filter ArtworkPaletteTests`. Expected: the build fails because `ArtworkPalette` is not defined.
- [ ] **Step 3: Implement** the Interfaces block. Draw `dominant(image:)` with `CGColorSpace(name: CGColorSpace.sRGB)`, `CGImageAlphaInfo.premultipliedLast` and `interpolationQuality = .medium`.
- [ ] **Step 4: Run** `cd macos && swift test --filter ArtworkPaletteTests`. Expected: PASS. Then run the gates.
- [ ] **Step 5: Commit**

```bash
git add macos/Sources/TyrannyLikesSmokeCore/ArtworkPalette.swift macos/Tests/TyrannyLikesSmokeCoreTests/ArtworkPaletteTests.swift
git commit -m "Find a cover's dominant colour"
```

### Task 4: `ThemeResolver` and `ArtTint`

**Files:**
- Create: `macos/Sources/TyrannyLikesSmokeCore/ThemeResolver.swift`
- Create: `macos/Sources/TyrannyLikesSmokeCore/ArtTint.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/ThemeResolverTests.swift`
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/ArtTintTests.swift`

**Interfaces:**
- Consumes: `RGB`, `ColourToken`, `ColourSettings` (Task 1), and `RecentKeys` (existing, `RecentKeys.swift`).
- Produces:

```swift
/// `.tinted` is an art colour. Only it gets the contrast floor (app side).
public enum ResolvedColour: Equatable, Sendable { case system, custom(RGB), tinted(RGB) }
public struct ResolvedTokens: Equatable, Sendable {
    public var accent, playing, selection, success, warning, error: ResolvedColour
    public static let system: ResolvedTokens                  // every token .system
    public subscript(token: ColourToken) -> ResolvedColour { get }
}
public enum ThemeResolver {
    public static func resolve(settings: ColourSettings, artColour: RGB?) -> ResolvedTokens
}
public struct ArtTint: Sendable {
    public enum Artwork: Equatable, Sendable {
        case noContext, nothingPlaying, loading, missing, unavailable, image(key: String)
    }
    /// noContext if !hasContext; nothingPlaying if !playing; then by state "loading" / "missing" /
    /// "unavailable"; any other state is .image(key) when cacheKey is non-nil, else .loading.
    public static func artwork(hasContext: Bool, playing: Bool, state: String, cacheKey: String?) -> Artwork
    public init(capacity: Int = 64)
    public private(set) var colour: RGB?
    /// Returns a key to extract when the image is neither cached nor already in flight.
    public mutating func observe(_ artwork: Artwork) -> String?
    /// Caches the result (nil = no usable colour). It sets `colour` only if `key` is still the observed image.
    public mutating func extracted(_ colour: RGB?, for key: String)
}
```

- **Resolver rules:**
  - `accent`: `.tinted(art)` when the tint is on and an art colour exists; else `.custom` when set; else `.system`.
  - `playing`: `.custom` when set; else the resolved `accent`.
  - The other four tokens: `.custom` or `.system`. The art colour never touches them.
- **`ArtTint.observe` rules:**
  - `.noContext` and `.loading` keep `colour`.
  - `.nothingPlaying`, `.missing` and `.unavailable` set `colour = nil`.
  - All of these clear the current key.
  - `.image(k)` makes `k` current. A cache hit sets `colour` to the cached value at once, and nil clears it. A miss returns `k` unless it is already in flight.
  - Cache: `[String: RGB?]` plus `RecentKeys<String>(capacity:)`. Drop the keys that `touch` returns.

- [ ] **Step 1: Write the failing resolver tests** in `@Suite struct ThemeResolverTests`. Let A = `#FF0000` (art), X = `#3366FF` (custom accent) and P = `#00AA00` (custom playing).
  - `defaultsResolveToSystem`: `resolve(settings: ColourSettings(), artColour: nil) == .system`.
  - `tintBeatsCustomAccent`: with custom accent X and art A, `accent == .tinted(A)` and `playing == .tinted(A)`.
  - `tintOffUsesCustomAccent`: with tint off, X and A, `accent == .custom(X)` and `playing == .custom(X)`.
  - `noArtUsesCustomOrSystem`: with X and no art, `accent == .custom(X)`. With nothing set, `accent == .system`.
  - `customPlayingIgnoresArt`: with P and A, `playing == .custom(P)` and `accent == .tinted(A)`.
  - `otherTokensNeverTint`: with A, a custom `error` and nothing else, `selection`, `success` and `warning` are `.system`, and `error == .custom(...)`.
- [ ] **Step 2: Write the failing `ArtTint` tests** in `@Suite struct ArtTintTests`. Let R = red and B = blue.
  - `artworkMapping`: one expectation per row of the `artwork(...)` doc comment, including `"embedded"` with a key giving `.image`, and `"folder"` with no key giving `.loading`.
  - `missReturnsKeyOnceThenAppliesResult`:
    - `observe(.image("k1")) == "k1"`;
    - a second `observe(.image("k1")) == nil` (in flight);
    - `colour == nil`;
    - after `extracted(R, for: "k1")`, `colour == R`.
  - `staleExtractionIsCachedButNotApplied`:
    1. observe k1, then extract R, so `colour == R`;
    2. observe k2, `.loading`, then k3;
    3. `extracted(B, for: "k2")` leaves `colour == R`;
    4. observing `.image("k2")` again returns nil and sets `colour == B`.
  - `noContextAndLoadingKeepTheColour`: from `colour == R`, observing `.noContext` and then `.loading` leaves `colour == R`.
  - `returningToACachedCoverNeedsNoExtraction`: after k1 is extracted to R, observe `.noContext`, then `.image("k1")`. The call returns nil and `colour == R`.
  - `stopMissingAndUnavailableClear`: from R, each of `.nothingPlaying`, `.missing` and `.unavailable` sets `colour == nil`.
  - `greyCoverClearsTheColour`: from R, observe k2 and call `extracted(nil, for: "k2")`. Then `colour == nil`, and observing k2 again returns nil.
  - `cacheEvictsLeastRecentlyUsed`: with `ArtTint(capacity: 2)`, extract k1, k2 and k3. Observing k1 then returns `"k1"` (a miss), and observing k3 returns nil.
- [ ] **Step 3: Run** `cd macos && swift test --filter "ThemeResolverTests|ArtTintTests"`. Expected: the build fails because the types are not defined.
- [ ] **Step 4: Implement** the Interfaces block and the rules above.
- [ ] **Step 5: Run** the same filter. Expected: PASS. Then run the gates.
- [ ] **Step 6: Commit**

```bash
git add macos/Sources/TyrannyLikesSmokeCore/ThemeResolver.swift macos/Sources/TyrannyLikesSmokeCore/ArtTint.swift macos/Tests/TyrannyLikesSmokeCoreTests/ThemeResolverTests.swift macos/Tests/TyrannyLikesSmokeCoreTests/ArtTintTests.swift
git commit -m "Resolve colour tokens and track the cover-art colour"
```

### Task 5: `ColourTheme`, environment injection and the SwiftUI call sites

**Files:**
- Create: `macos/Sources/TyrannyLikesSmoke/Models/ColourTheme.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/PlayerModel.swift` (`let theme`, `init` parameter)
- Modify: `macos/Sources/TyrannyLikesSmoke/TyrannyLikesSmokeApp.swift` (the `PlayerModel(...)` call; `.colourTheme(model.theme)` on the `MainWindow`, `EqualizerView` and `SettingsView` roots)
- Modify: `macos/Sources/TyrannyLikesSmoke/NativeRenderViews.swift` (`renderSettings`'s `NSHostingView` root gets `.colourTheme(model.theme)`)
- Modify the call sites:
  - `Shell/PlayerBar.swift`: lines 63, 114, 144, 173, 222 and 233;
  - `Views/LyricsPane.swift`: line 98;
  - `TrackDrag.swift`: line 40;
  - `Shell/ImportProgressView.swift`: line 32;
  - `Shell/StatusBar.swift`: lines 86–92;
  - `Shell/SettingsView.swift`: lines 114 and 186;
  - `Shell/EqualizerView.swift`: line 49;
  - `Views/UpNextView.swift`: lines 96 and 152;
  - `Views/PlaylistSheets.swift`: lines 68, 170, 192, 256 and 323;
  - `Views/RadioView.swift`: lines 54 and 192;
  - `Shell/MainWindowView.swift`: line 115.

**Interfaces:**
- Consumes: `ColourSettings`, `ColourToken`, `Contrast`, `ThemeResolver`, `ResolvedTokens`, `ResolvedColour` (Tasks 1–4).
- Produces, in `ColourTheme.swift`:

```swift
struct ThemeColours {                         // SwiftUI tokens; the defaults are today's look
    var accent: Color = .accentColor
    var success: Color = .green
    var warning: Color = .orange
    var error: Color = .red
}
extension EnvironmentValues { @Entry var themeColours = ThemeColours() }
extension View { func colourTheme(_ theme: ColourTheme) -> some View }  // a ViewModifier: .environment(theme) + .environment(\.themeColours, theme.colours)

@MainActor @Observable
final class ColourTheme {
    init(defaults: UserDefaults)
    static func forLaunch(arguments: [String]) -> ColourTheme   // harness suite or .standard (Global Constraints)
    private(set) var settings: ColourSettings
    private(set) var tokens: ResolvedTokens
    private(set) var colours: ThemeColours
    /// Incremented whenever `tokens` changes. AppKit views compare it to know when to redraw.
    private(set) var revision: UInt64
    func update(_ settings: ColourSettings)                     // save, re-resolve, no animation
    /// .system → `system`; .custom → NSColor(srgbRed:…); .tinted → a dynamic NSColor that applies
    /// Contrast.raise(rgb, against: dark ? Contrast.darkBackground : Contrast.lightBackground, to: Contrast.floor)
    /// to the appearance it resolves in (dark = bestMatch(from: [.aqua, .darkAqua]) == .darkAqua).
    func nsColour(_ token: ColourToken) -> NSColor
}
```

- System colours per token, for `nsColour`:
  - `accent` and `playing`: `.controlAccentColor`;
  - `selection`: `.selectedContentBackgroundColor`;
  - `success`, `warning`, `error`: `.systemGreen`, `.systemOrange`, `.systemRed`.
- `colours` uses `Color(nsColor: nsColour(token))` for each non-`.system` token and the `ThemeColours` default for `.system`.
- `PlayerModel.init(quitSuspensions:importNotes:theme:)` stores `let theme: ColourTheme`. `TyrannyLikesSmokeApp` passes `.forLaunch(arguments: CommandLine.arguments)`.

- [ ] **Step 1: Implement** `ColourTheme.swift` and the `PlayerModel` / `TyrannyLikesSmokeApp` / `NativeRenderViews` changes.
- [ ] **Step 2: Migrate each SwiftUI call site** to `@Environment(\.themeColours) private var colours`:
  - `Color.accentColor` and `.accentColor` become `colours.accent`;
  - `.green` becomes `colours.success`;
  - `.orange` becomes `colours.warning`;
  - `.red` and `Color.red` become `colours.error`.
  - StatusBar maps "connected" to `success`, "connecting" to `.yellow` (unchanged), and every other state to `error`.
- [ ] **Step 3: Verify no SwiftUI site is left.** Run from the repo root:

```bash
grep -rnE "Color\.accentColor|\.foregroundStyle\(\.(green|orange|red)\)|Color\.(green|orange|red)\b|^[[:space:]]*case .*: \.(green|red)$" macos/Sources/tyranny likes smoke --include='*.swift' | grep -v '/Native' | grep -v ColourTheme.swift
grep -rn "Environment(ColourTheme.self)" macos/Sources/tyranny likes smoke --include='*.swift'
```

Expected: both print nothing. Outside `ColourTheme.swift`, `controlAccentColor` remains only in `VirtualTrackTable.swift`, `UpNextTable.swift` and `AlbumGridItem.swift`, which Task 6 handles.
- [ ] **Step 4: Run the gates.** Then run `cargo build --locked --bins && python3 scripts/render_views.py --app "macos/.build/debug/tyranny likes smoke" --out target/render-colours`, and look at `15-player-bar-collapsed.png`, `19-player-bar-disconnected.png`, `20-player-bar-error.png` and `14-settings-playback.png`. Expected: they look exactly as before, because the harness suite has no custom colours.
- [ ] **Step 5: Commit**

```bash
git add macos/Sources/TyrannyLikesSmoke/Models/ColourTheme.swift macos/Sources/TyrannyLikesSmoke/PlayerModel.swift macos/Sources/TyrannyLikesSmoke/TyrannyLikesSmokeApp.swift macos/Sources/TyrannyLikesSmoke/NativeRenderViews.swift macos/Sources/TyrannyLikesSmoke/Shell/PlayerBar.swift macos/Sources/TyrannyLikesSmoke/Views/LyricsPane.swift macos/Sources/TyrannyLikesSmoke/TrackDrag.swift macos/Sources/TyrannyLikesSmoke/Shell/ImportProgressView.swift macos/Sources/TyrannyLikesSmoke/Shell/StatusBar.swift macos/Sources/TyrannyLikesSmoke/Shell/SettingsView.swift macos/Sources/TyrannyLikesSmoke/Shell/EqualizerView.swift macos/Sources/TyrannyLikesSmoke/Views/UpNextView.swift macos/Sources/TyrannyLikesSmoke/Views/PlaylistSheets.swift macos/Sources/TyrannyLikesSmoke/Views/RadioView.swift macos/Sources/TyrannyLikesSmoke/Shell/MainWindowView.swift
git commit -m "Draw accent and status colours from colour tokens"
```

### Task 6: `TokenRowView` and colours in the AppKit tables and grid

**Files:**
- Create: `macos/Sources/TyrannyLikesSmoke/Lists/TokenRowView.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/Models/ColourTheme.swift` (add `TableColours`, `var tableColours`)
- Modify: `macos/Sources/TyrannyLikesSmoke/Lists/VirtualTrackTable.swift`. That covers the `colours` property, `rowViewForRow`, the redraw in `update`, and `TrackCellView`.
- Modify: `macos/Sources/TyrannyLikesSmoke/Lists/TrackListView.swift`, which passes `colours: player.theme.tableColours`.
- Modify: `macos/Sources/TyrannyLikesSmoke/Views/UpNextTable.swift`. `QueueRowView` becomes `TokenRowView`'s subclass; `QueueCellView` changes too.
- Modify: `macos/Sources/TyrannyLikesSmoke/Views/UpNextView.swift`, which passes `colours`.
- Modify: `macos/Sources/TyrannyLikesSmoke/Albums/AlbumTrackTable.swift` and `AlbumExpansionView.swift`, which passes `colours`.
- Modify: `macos/Sources/TyrannyLikesSmoke/Albums/AlbumGridItem.swift` and `AlbumGridView.swift`. `AlbumCollection` gets `colours`.

**Interfaces:**
- Consumes: `ColourTheme.nsColour(_:)`, `ColourTheme.revision`, `ColourTheme.tokens` (Task 5), and `Contrast.composite`, `Contrast.prefersLightText`, `Contrast.lightBackground`, `Contrast.darkBackground` (Task 2).
- Produces:

```swift
/// What AppKit views draw with. Equal when `revision` is equal.
struct TableColours: Equatable {
    let accent: NSColor
    let playing: NSColor
    let selection: RGB?                // nil: "System", the default row selection
    let revision: UInt64
}
extension ColourTheme { var tableColours: TableColours }      // selection: the custom RGB, else nil

/// A cell that shows the playing indicator. Its row calls it when selection or emphasis changes.
@MainActor protocol PlayingIndicatorCell: NSView {
    func rowChanged(selected: Bool, style: NSView.BackgroundStyle)
}
class TokenRowView: NSTableRowView {
    var selection: RGB? { didSet }     // redraws and refreshes indicators
    override func drawSelection(in dirtyRect: NSRect)   // nil → super; else fill bounds, opacity 1 if isEmphasized else 0.45
    override var interiorBackgroundStyle: NSView.BackgroundStyle  // selection set and isSelected: .emphasized if
        // Contrast.prefersLightText(on: composite(selection, opacity, over: dark ? darkBackground : lightBackground)) else .normal
}
```

- `TokenRowView` calls `rowChanged(selected:style:)` on each `PlayingIndicatorCell` subview:
  - from `isSelected` and `isEmphasized` `didSet`;
  - from `selection` `didSet`;
  - from `didAddSubview(_:)`.
- `TrackCellView` and `QueueCellView` conform to the protocol.
  - `showPlaying(_:colour:identifier:)` stores `colour`.
  - `rowChanged` sets `icon.contentTintColor`:
    - in a selected row, `.alternateSelectedControlTextColor` for `.emphasized` and `.labelColor` for `.normal`;
    - otherwise the stored `playing` colour.
  - The `.controlAccentColor` assignments in their `init`s go.
- `AlbumGridCellView` gains `var selectionColour: NSColor = .controlAccentColor`, which redraws on change. `AlbumGridItem.show(...)` gains a `selectionColour: NSColor` parameter. The border stroke uses it in place of `.controlAccentColor`.
- Each coordinator keeps `shownColours: UInt64?`. When `parent.colours.revision` changes, it:
  - sets `selection` on every row view from `table.enumerateAvailableRowViews`;
  - reloads the visible status column (`reloadVisible(columns:)` in `VirtualTrackTable`, the `status` column in `UpNextTable`);
  - for the grid, calls `reconfigureVisible()`.

- [ ] **Step 1: Implement** `TokenRowView.swift` and `TableColours`.
- [ ] **Step 2: Wire the three tables.** Each `rowViewForRow` makes a `TokenRowView` (`QueueRowView` in Up Next) and sets `selection`. Each status cell passes `parent.colours.playing`. Add the redraw on revision change.
- [ ] **Step 3: Wire the album grid border** through `AlbumCollection.colours.accent`.
- [ ] **Step 4: Verify.** Running `grep -rn "controlAccentColor" macos/Sources/tyranny likes smoke --include='*.swift'` prints only lines in `ColourTheme.swift`. Run the gates. Re-run the render-views command from Task 5. Expected: `01-songs-ungrouped`, `05-albums-grid-expanded` and `10-up-next` look as before (System selection, system accent speaker).
- [ ] **Step 5: Commit**

```bash
git add macos/Sources/TyrannyLikesSmoke/Lists/TokenRowView.swift macos/Sources/TyrannyLikesSmoke/Models/ColourTheme.swift macos/Sources/TyrannyLikesSmoke/Lists/VirtualTrackTable.swift macos/Sources/TyrannyLikesSmoke/Lists/TrackListView.swift macos/Sources/TyrannyLikesSmoke/Views/UpNextTable.swift macos/Sources/TyrannyLikesSmoke/Views/UpNextView.swift macos/Sources/TyrannyLikesSmoke/Albums/AlbumTrackTable.swift macos/Sources/TyrannyLikesSmoke/Albums/AlbumExpansionView.swift macos/Sources/TyrannyLikesSmoke/Albums/AlbumGridItem.swift macos/Sources/TyrannyLikesSmoke/Albums/AlbumGridView.swift
git commit -m "Draw table selection and the playing icon from colour tokens"
```

### Task 7: Cover-art tint

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmoke/ArtworkStore.swift`. Add `hasContext` and `onNowPlayingChange`, called at the end of `synchronize(...)` and of `insert(_:for:)`.
- Modify: `macos/Sources/TyrannyLikesSmoke/Models/ColourTheme.swift`. Add `artColour`, `artworkChanged`, the extraction and the animation.
- Modify: `macos/Sources/TyrannyLikesSmoke/PlayerModel.swift`, which installs the hook in `init`.

**Interfaces:**
- Consumes: `ArtTint`, `ArtTint.artwork(hasContext:playing:state:cacheKey:)` (Task 4), `ArtworkPalette.dominant(image:)` (Task 3), and `ArtworkImage` (existing, `@unchecked Sendable`).
- Produces:

```swift
// ArtworkStore
var hasContext: Bool                                   // reads `generation` for observation; context != nil
@ObservationIgnored var onNowPlayingChange: (@MainActor () -> Void)?
// ColourTheme
private(set) var artColour: RGB?
func artworkChanged(hasContext: Bool, playing: Bool, image: ArtworkImage)
```

- **`artworkChanged`:**
  1. Feed `ArtTint.artwork(hasContext:playing:state: image.state, cacheKey: image.cacheKey)` to `tint.observe`.
  2. If that returns a key and `image.image` exists, start a `Task` that awaits `Task.detached(priority: .utility) { ArtworkPalette.dominant(image:) }`. Then, back on the main actor, it calls `tint.extracted(_:for:)` and applies the result. If `image.image` is nil, call `extracted(nil, for: key)` at once.
  3. **Applying:** when `tint.colour != artColour`, call `withAnimation(.easeInOut(duration: 0.3)) { artColour = tint.colour; re-resolve }`.
  4. `update(_:)` re-resolves with the current `artColour` and does not animate.
- `PlayerModel.init` installs `artwork.onNowPlayingChange = { [weak self] in … self.theme.artworkChanged(hasContext: artwork.hasContext, playing: artwork.nowPlayingRequest != nil, image: artwork.nowPlaying) }`.
- Pause and seek change only `position_frames` and `transport`, not the `entryID` passed to `synchronize`. They therefore reach `ArtTint` as the same `.image(key)`, which is a cache hit with the same colour, so nothing changes.

- [ ] **Step 1: Implement** the three changes above.
- [ ] **Step 2: Run the gates.** All `ArtTint` behaviour is pinned by Task 4's tests. Task 9 extends `NativeArtworkSmoke` to exercise it end to end.
- [ ] **Step 3: Self-check.** Run `grep -n "Task.detached" macos/Sources/TyrannyLikesSmoke/Models/ColourTheme.swift`; it shows the one extraction. No `ArtworkPalette` call runs outside it.
- [ ] **Step 4: Commit**

```bash
git add macos/Sources/TyrannyLikesSmoke/ArtworkStore.swift macos/Sources/TyrannyLikesSmoke/Models/ColourTheme.swift macos/Sources/TyrannyLikesSmoke/PlayerModel.swift
git commit -m "Tint the accent from the playing cover"
```

### Task 8: Settings → Appearance

**Files:**
- Create: `macos/Sources/TyrannyLikesSmoke/Shell/AppearanceSettingsPane.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/Models/NavigationModel.swift` (`SettingsTab.appearance`)
- Modify: `macos/Sources/TyrannyLikesSmoke/Shell/SettingsView.swift` (fourth tab, update the doc comment)
- Modify: `macos/Sources/TyrannyLikesSmoke/NativeRenderViews.swift` (`renderSettings` tab list and two extra renders)

**Interfaces:**
- Consumes: `ColourTheme.settings`, `update(_:)`, `nsColour(_:)` (Task 5); `Contrast.warning(for:)` (Task 2); `ColourToken`, `RGB` (Task 1).
- Produces:
  - `enum SettingsTab: String, Hashable { case connection, library, playback, appearance }`;
  - `struct AppearanceSettingsPane: View { let theme: ColourTheme }`, tagged `.appearance` with `Label("Appearance", systemImage: "paintpalette")`.

- **Each row** (`ForEach(ColourToken.allCases)`):
  - the label from Global Constraints;
  - a `Picker` popup with "System" / "Custom" ("Same as Accent" / "Custom" for `playing`);
  - a `ColorPicker(supportsOpacity: false)`, disabled for System, which then shows the system colour.
  - Choosing Custom with no stored value seeds `nsColour(token)` converted to sRGB.
  - The well's binding converts `Color` to `RGB` through `NSColor(color).usingColorSpace(.sRGB)`.
  - Under a custom colour, `Contrast.warning(for:)` shows as a caption when it is non-nil.
- The tint `Toggle` sits below the rows, with its footnote.
- Then the "Reset to Defaults" button, which calls `theme.update(ColourSettings())`.
- Use `.formStyle(.grouped)`, `.padding(12)` and `.frame(width: 600)` as in `PlaybackSettingsPane`.
- In `renderSettings`, add `(.appearance, "26-settings-appearance")`. Then render twice more:
  - `27-settings-appearance-custom`, with `theme.update(ColourSettings(custom: [.accent: #FFE066, .error: #000080]))`, which shows both warnings;
  - `28-settings-appearance-dark`, the same custom settings with `host.appearance = NSAppearance(named: .darkAqua)`.
  - Restore the saved settings with `theme.update(saved)` afterwards.

- [ ] **Step 1: Implement** the tab, the pane and the renders.
- [ ] **Step 2: Run the gates.** Then run `python3 scripts/render_views.py --app "macos/.build/debug/tyranny likes smoke" --out target/render-colours` and look at PNGs 26–28. Expected:
  - every row and the toggle show; "Same as Accent" is on the playing row;
  - 27 shows "Low contrast in Light appearance" under Accent and "Low contrast in Dark appearance" under Error;
  - 28 is legible in dark.
- [ ] **Step 3: Commit**

```bash
git add macos/Sources/TyrannyLikesSmoke/Shell/AppearanceSettingsPane.swift macos/Sources/TyrannyLikesSmoke/Models/NavigationModel.swift macos/Sources/TyrannyLikesSmoke/Shell/SettingsView.swift macos/Sources/TyrannyLikesSmoke/NativeRenderViews.swift
git commit -m "Add the Appearance settings tab"
```

### Task 9: Colour commands, smokes and final gates

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmokeCore/CommandLanguage.swift`
- Modify: `macos/Sources/TyrannyLikesSmokeCore/ColourSettings.swift` (add `apply`)
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/CommandLanguageTests.swift`, `macos/Tests/TyrannyLikesSmokeCoreTests/ColourSettingsTests.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/CommandDispatcher.swift`
- Modify: `macos/Sources/TyrannyLikesSmoke/NativeCommandSmoke.swift`, `macos/Sources/TyrannyLikesSmoke/NativeArtworkSmoke.swift`

**Interfaces:**
- Consumes: `ColourSettings`, `ColourToken`, `RGB` (Task 1); `ColourTheme.settings`, `update(_:)`, `tokens` (Tasks 5, 7); `SettingsTab.appearance` (Task 8).
- Produces:
  - `CommandID`: `case colour, colours, tint`;
  - `CommandArgumentKind.colour`, which is valid when the value is `"system"` or `RGB(hex:)` is non-nil; its completion candidates are `["system"]`;
  - registry entries:
    - `.init(.colour, "Set a colour token, or return it to System.", [.init("token", .choices(ColourToken.allCases.map(\.rawValue))), .init("#rrggbb|system", .colour)])`;
    - `.init(.colours, "Return every colour to System and turn the cover tint on.", [.init("reset", .choices(["reset"]))])`;
    - `.init(.tint, "Turn the accent tint from cover art on or off.", [.init("on|off", .choices(["on", "off"]))])`;
    - the `.settings` choices gain `"appearance"`.
  - `ColourSettings.apply(_ command: ParsedCommand) throws`:
    - `.colour` sets the token, or clears it for `system`;
    - `.colours` resets to `ColourSettings()`;
    - `.tint` sets `tintFromArtwork`;
    - any other id throws `CommandLanguageError("Not a colour command.")`.
  - Dispatcher: `case .colour, .colours, .tint: var settings = player.theme.settings; try settings.apply(command); player.theme.update(settings)`. These commands need no connection.

- [ ] **Step 1: Write the failing tests.**
  - In `CommandLanguageTests.parsesEverySemanticActionWithItsArguments`, add `("colour accent #3366ff", .colour, ["accent", "#3366ff"])`, `("colours reset", .colours, ["reset"])` and `("tint off", .tint, ["off"])`.
  - Add `@Test func settingsAppearanceParses()`.
  - Add to `rejectsInvalidInput`: `"colour accent"`, `"colour accent red"`, `"colour accent #12345"`, `"colour accent #1234567"`, `"colour accent #GGGGGG"`, `"colour background #000000"`, `"colour Accent system"`, `"colours"`, `"colours clear"`, `"tint toggle"`, `"tint yes"`, `"settings appearances"`.
  - Add `@Test func colourCompletions()`:
    - `completions("colour ac") == ["colour accent"]`;
    - `completions("colour accent ") == ["colour accent system"]`;
    - `completions("tint ") == ["tint off", "tint on"]`;
    - `completions("colours ") == ["colours reset"]`.
  - In `ColourSettingsTests`:
    - `applySetsAndClearsAToken`: after `colour error #AA0000`, `[.error] == RGB(hex: "#AA0000")`; after `colour error system`, it is nil;
    - `coloursResetRestoresDefaults`: from a custom accent with the tint off, the result `== ColourSettings()`;
    - `tintCommandSetsTheToggle`;
    - `applyRejectsOtherCommands`: `#expect(throws: CommandLanguageError.self)` for `parse("play")`.
- [ ] **Step 2: Run** `cd macos && swift test --filter "CommandLanguageTests|ColourSettingsTests"`. Expected: the build fails because `.colour` is not a member of `CommandID`.
- [ ] **Step 3: Implement** the core changes, then the dispatcher case.
- [ ] **Step 4: Extend `NativeCommandSmoke.exercise`.** Before `disconnect`:
  1. Save `player.theme.settings`.
  2. Run `colour accent #3366ff` and check `settings[.accent] == RGB(hex: "#3366FF")`.
  3. Run `tint off` and check `!tintFromArtwork`.
  4. Run `colours reset` and check `settings == ColourSettings()`.
  5. Run `settings appearance` and check `navigation.settingsTab == .appearance`.
  6. Restore the saved settings.
  7. Append the case `colour-tint-reset-and-appearance-commands-apply-locally`.
- [ ] **Step 5: Extend `NativeArtworkSmoke`.**
  - In `startIfRequested`:
    - save `model.theme.settings`;
    - call `model.theme.update(ColourSettings())`;
    - `defer` the restore.
  - In `exercise`, after `play("Embedded Track")`, wait until `model.theme.tokens.accent` is `.tinted(c)` with `c.red > 0.8, c.green < 0.2, c.blue < 0.2`.
  - After `play("Folder Track")`, wait for the same shape with blue. Then:
    1. Pause with `NativeSmoke.drive("press", "transport-play-pause")`. Wait until paused. Check that `tokens.accent` is unchanged.
    2. Press again and wait until playing.
    3. Execute `tint off` through `CommandDispatcher(player: model, navigation: navigation, lists: TrackLists())`. Check `tokens.accent == .system`.
    4. Execute `tint on`. Wait for the same blue `.tinted` value.
    5. Append `native-accent-follows-playing-cover-holds-on-pause-and-reverts-with-tint-off`.
- [ ] **Step 6: Run** the gates, then the checks below.
  - `python3 scripts/client_gate.py --commands-only --app "macos/.build/debug/tyranny likes smoke"`. Expected: exit 0, and `commands.json` lists the new case.
  - The standard Rust gates, though no Rust change is expected: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked --all-targets`. Expected: all pass.
  - The artwork smoke needs Accessibility and a release server, so the user runs it: `cargo build --locked --release --bin tyranny-likes-smoke-server && python3 scripts/artwork_gate.py --app "macos/.build/debug/tyranny likes smoke"`. The user also checks a custom selection colour by eye.
- [ ] **Step 7: Commit**

```bash
git add macos/Sources/TyrannyLikesSmokeCore/CommandLanguage.swift macos/Sources/TyrannyLikesSmokeCore/ColourSettings.swift macos/Tests/TyrannyLikesSmokeCoreTests/CommandLanguageTests.swift macos/Tests/TyrannyLikesSmokeCoreTests/ColourSettingsTests.swift macos/Sources/TyrannyLikesSmoke/CommandDispatcher.swift macos/Sources/TyrannyLikesSmoke/NativeCommandSmoke.swift macos/Sources/TyrannyLikesSmoke/NativeArtworkSmoke.swift
git commit -m "Add colour, colours and tint commands"
```

---

## Decisions this plan makes where the spec is open

- **`ResolvedColour`.** It has three cases (`.system`, `.custom`, `.tinted`), not two. The app applies the contrast floor to tinted colours only, so it must know which colours are tinted.
- **Contrast backgrounds.** Both the floor and the Settings warnings use the fixed backgrounds `#ECECEC` and `#1E1E1E`, the spec's test values. Each is stricter than the real table background in its appearance, and fixed values keep the rule pure and tested.
- **Unfocused selection.** Its text colour follows the fill as drawn, which is the custom colour at 0.45 composited over that background.
- **SwiftUI tokens.** Leaf views read them through an `EnvironmentKey` whose default is the system colours. `.environment(theme)` is injected too, as the spec asks. A missing provider therefore falls back to today's look instead of trapping. AppKit views read `player.theme` directly.
- **Colour defaults.** A gate, smoke or render launch uses the harness defaults suite, so automation never changes the user's colours.
- **Command order.** The colour commands come last (Task 9), not with the other core logic. `CommandDispatcher`'s `switch` is exhaustive, and `ColourTheme` must exist first.
- **Command syntax.** `colours reset` is the command `colours` with the single choice `reset`. `tint` takes `on|off` only, as the spec gives it, with no `toggle`.
