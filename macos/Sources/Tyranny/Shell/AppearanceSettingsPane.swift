import AppKit
import SwiftUI
import TyrannyCore

/// One colour per token: System or Custom, a colour well, and a contrast warning.
/// Below the rows: the cover-art tint toggle and Reset to Defaults.
struct AppearanceSettingsPane: View {
    let theme: ColourTheme

    var body: some View {
        Form {
            ForEach(ColourToken.allCases, id: \.self) { token in
                row(token)
            }
            Toggle("Tint accent from cover art", isOn: Binding(
                get: { theme.settings.tintFromArtwork },
                set: { value in
                    var settings = theme.settings
                    settings.tintFromArtwork = value
                    theme.update(settings)
                }))
                .accessibilityIdentifier("settings-tint-from-artwork")
            Text("While a track with artwork plays, the accent follows its cover.")
                .font(.caption).foregroundStyle(.secondary)
            Button("Reset to Defaults") { theme.update(ColourSettings()) }
                .accessibilityIdentifier("settings-colours-reset")
        }
        .formStyle(.grouped)
        .padding(12).frame(width: 600)
    }

    private func label(_ token: ColourToken) -> String {
        switch token {
        case .accent: return "Accent"
        case .playing: return "Playing"
        case .selection: return "Selection"
        case .success: return "Success"
        case .warning: return "Warning"
        case .error: return "Error"
        }
    }

    private func row(_ token: ColourToken) -> some View {
        let custom = theme.settings.custom[token]
        return VStack(alignment: .leading, spacing: 2) {
            HStack {
                Picker(label(token), selection: Binding(
                    get: { theme.settings.custom[token] == nil ? Choice.system : Choice.custom },
                    set: { setChoice($0, for: token) })
                ) {
                    Text(token == .playing ? "Same as Accent" : "System").tag(Choice.system)
                    Text("Custom").tag(Choice.custom)
                }
                .accessibilityIdentifier("settings-colour-\(token.rawValue)")
                ColorPicker("", selection: wellBinding(token), supportsOpacity: false)
                    .labelsHidden()
                    .disabled(custom == nil)
                    .accessibilityLabel(label(token))
                    .accessibilityIdentifier("settings-colour-\(token.rawValue)-well")
            }
            if let custom, let warning = Contrast.warning(for: custom) {
                Text(warning).font(.caption).foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings-colour-\(token.rawValue)-contrast")
            }
        }
    }

    private enum Choice: Hashable { case system, custom }

    private func setChoice(_ choice: Choice, for token: ColourToken) {
        var settings = theme.settings
        switch choice {
        case .system:
            settings.custom[token] = nil
        case .custom:
            guard settings.custom[token] == nil else { return }
            settings.custom[token] = Self.rgb(theme.nsColour(token)) ?? RGB(red: 0, green: 0, blue: 0)
        }
        theme.update(settings)
    }

    private func wellBinding(_ token: ColourToken) -> Binding<Color> {
        Binding(
            get: { Color(nsColor: theme.nsColour(token)) },
            set: { colour in
                guard let rgb = Self.rgb(NSColor(colour)) else { return }
                var settings = theme.settings
                settings.custom[token] = rgb
                theme.update(settings)
            })
    }

    private static func rgb(_ colour: NSColor) -> RGB? {
        guard let srgb = colour.usingColorSpace(.sRGB) else { return nil }
        return RGB(red: srgb.redComponent, green: srgb.greenComponent, blue: srgb.blueComponent)
    }
}
