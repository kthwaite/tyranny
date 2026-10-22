/// `.tinted` is an art colour. Only it gets the contrast floor (app side).
public enum ResolvedColour: Equatable, Sendable { case system, custom(RGB), tinted(RGB) }

public struct ResolvedTokens: Equatable, Sendable {
    public var accent, playing, selection, success, warning, error: ResolvedColour

    public static let system = ResolvedTokens(
        accent: .system, playing: .system, selection: .system,
        success: .system, warning: .system, error: .system)

    public subscript(token: ColourToken) -> ResolvedColour {
        switch token {
        case .accent: return accent
        case .playing: return playing
        case .selection: return selection
        case .success: return success
        case .warning: return warning
        case .error: return error
        }
    }
}

public enum ThemeResolver {
    public static func resolve(settings: ColourSettings, artColour: RGB?) -> ResolvedTokens {
        func plain(_ token: ColourToken) -> ResolvedColour {
            settings[token].map(ResolvedColour.custom) ?? .system
        }
        let accent: ResolvedColour
        if settings.tintFromArtwork, let artColour {
            accent = .tinted(artColour)
        } else {
            accent = plain(.accent)
        }
        return ResolvedTokens(
            accent: accent,
            playing: settings[.playing].map(ResolvedColour.custom) ?? accent,
            selection: plain(.selection),
            success: plain(.success),
            warning: plain(.warning),
            error: plain(.error))
    }
}
