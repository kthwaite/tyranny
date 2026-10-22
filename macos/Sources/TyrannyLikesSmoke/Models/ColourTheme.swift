import AppKit
import Observation
import SwiftUI
import TyrannyLikesSmokeCore

/// SwiftUI colour tokens. The defaults are today's look.
struct ThemeColours {
    var accent: Color = .accentColor
    var success: Color = .green
    var warning: Color = .orange
    var error: Color = .red
}

extension EnvironmentValues {
    @Entry var themeColours = ThemeColours()
}

private struct ColourThemeModifier: ViewModifier {
    let theme: ColourTheme

    func body(content: Content) -> some View {
        content
            .environment(theme)
            .environment(\.themeColours, theme.colours)
    }
}

extension View {
    func colourTheme(_ theme: ColourTheme) -> some View {
        modifier(ColourThemeModifier(theme: theme))
    }
}

/// What AppKit views draw with. Equal when `revision` is equal.
struct TableColours: Equatable {
    let accent: NSColor
    let playing: NSColor
    let selection: RGB?                // nil: "System", the default row selection
    let revision: UInt64
}

/// The user's colour settings, resolved to the colours the app draws with.
@MainActor @Observable
final class ColourTheme {
    @ObservationIgnored private let defaults: UserDefaults
    private(set) var settings: ColourSettings
    private(set) var tokens: ResolvedTokens
    private(set) var colours: ThemeColours
    /// Incremented whenever `tokens` changes. AppKit views compare it to know when to redraw.
    private(set) var revision: UInt64 = 0
    /// The dominant colour of the playing cover, or nil when there is none to show.
    private(set) var artColour: RGB?
    @ObservationIgnored private var tint = ArtTint()

    init(defaults: UserDefaults) {
        self.defaults = defaults
        let settings = ColourSettings(defaults: defaults)
        let tokens = ThemeResolver.resolve(settings: settings, artColour: nil)
        self.settings = settings
        self.tokens = tokens
        self.colours = Self.colours(for: tokens)
    }

    /// The harness suite for gate, smoke and render launches, so they never read the user's colours.
    static func forLaunch(arguments: [String]) -> ColourTheme {
        let defaults = QuitSuspensionStore.suiteName(forArguments: arguments)
            .flatMap(UserDefaults.init(suiteName:)) ?? .standard
        return ColourTheme(defaults: defaults)
    }

    /// Saves, re-resolves and publishes without animation.
    func update(_ settings: ColourSettings) {
        settings.save(to: defaults)
        self.settings = settings
        publish(animated: false)
    }

    /// Called when the playing track, its context or its cover changes.
    func artworkChanged(hasContext: Bool, playing: Bool, image: ArtworkImage) {
        let artwork = ArtTint.artwork(hasContext: hasContext, playing: playing,
                                      state: image.state, cacheKey: image.cacheKey)
        let key = tint.observe(artwork)
        if let key {
            if let cover = image.image {
                Task { [weak self] in
                    let colour = await Task.detached(priority: .utility) {
                        ArtworkPalette.dominant(image: cover)
                    }.value
                    self?.finishExtraction(colour, for: key)
                }
            } else {
                tint.extracted(nil, for: key)
            }
        }
        applyTint()
    }

    private func finishExtraction(_ colour: RGB?, for key: String) {
        tint.extracted(colour, for: key)
        applyTint()
    }

    private func applyTint() {
        guard tint.colour != artColour else { return }
        withAnimation(.easeInOut(duration: 0.3)) {
            artColour = tint.colour
            publish(animated: true)
        }
    }

    private func publish(animated: Bool) {
        let resolved = ThemeResolver.resolve(settings: settings, artColour: artColour)
        guard resolved != tokens else { return }
        var transaction = Transaction()
        if animated {
            transaction.animation = .easeInOut(duration: 0.3)
        } else {
            transaction.disablesAnimations = true
        }
        withTransaction(transaction) {
            tokens = resolved
            colours = Self.colours(for: resolved)
            revision &+= 1
        }
    }

    var tableColours: TableColours {
        let selection: RGB?
        if case .custom(let rgb) = tokens[.selection] { selection = rgb } else { selection = nil }
        return TableColours(accent: nsColour(.accent), playing: nsColour(.playing), selection: selection,
                            revision: revision)
    }

    func nsColour(_ token: ColourToken) -> NSColor {
        Self.nsColour(token, resolved: tokens[token])
    }

    private static func systemColour(_ token: ColourToken) -> NSColor {
        switch token {
        case .accent, .playing: return .controlAccentColor
        case .selection: return .selectedContentBackgroundColor
        case .success: return .systemGreen
        case .warning: return .systemOrange
        case .error: return .systemRed
        }
    }

    private static func nsColour(_ token: ColourToken, resolved: ResolvedColour) -> NSColor {
        switch resolved {
        case .system:
            return systemColour(token)
        case .custom(let rgb):
            return NSColor(srgbRed: rgb.red, green: rgb.green, blue: rgb.blue, alpha: 1)
        case .tinted(let rgb):
            return NSColor(name: nil) { @Sendable appearance in
                let dark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
                let raised = Contrast.raise(
                    rgb, against: dark ? Contrast.darkBackground : Contrast.lightBackground, to: Contrast.floor)
                return NSColor(srgbRed: raised.red, green: raised.green, blue: raised.blue, alpha: 1)
            }
        }
    }

    private static func colours(for tokens: ResolvedTokens) -> ThemeColours {
        var colours = ThemeColours()
        func swiftUI(_ token: ColourToken) -> Color? {
            tokens[token] == .system ? nil : Color(nsColor: nsColour(token, resolved: tokens[token]))
        }
        if let accent = swiftUI(.accent) { colours.accent = accent }
        if let success = swiftUI(.success) { colours.success = success }
        if let warning = swiftUI(.warning) { colours.warning = warning }
        if let error = swiftUI(.error) { colours.error = error }
        return colours
    }
}
