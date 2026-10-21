import Foundation

/// An sRGB colour with components in 0...1.
public struct RGB: Hashable, Sendable {
    public let red: Double
    public let green: Double
    public let blue: Double

    /// Clamps each component to 0...1. NaN becomes 0.
    public init(red: Double, green: Double, blue: Double) {
        self.red = RGB.clamp(red)
        self.green = RGB.clamp(green)
        self.blue = RGB.clamp(blue)
    }

    /// Parses exactly "#" followed by six ASCII hex digits, in any case.
    public init?(hex: String) {
        let bytes = Array(hex.utf8)
        guard bytes.count == 7, bytes[0] == UInt8(ascii: "#") else { return nil }
        var values: [Double] = []
        for pair in [1, 3, 5] {
            guard let high = RGB.hexValue(bytes[pair]),
                  let low = RGB.hexValue(bytes[pair + 1]) else { return nil }
            values.append(Double(high * 16 + low) / 255)
        }
        self.init(red: values[0], green: values[1], blue: values[2])
    }

    /// "#RRGGBB", upper case, each component rounded to the nearest 1/255.
    public var hex: String {
        func byte(_ value: Double) -> Int { Int((value * 255).rounded()) }
        return String(format: "#%02X%02X%02X", byte(red), byte(green), byte(blue))
    }

    private static func clamp(_ value: Double) -> Double {
        value.isNaN ? 0 : min(max(value, 0), 1)
    }

    private static func hexValue(_ byte: UInt8) -> Int? {
        switch byte {
        case UInt8(ascii: "0")...UInt8(ascii: "9"): return Int(byte - UInt8(ascii: "0"))
        case UInt8(ascii: "a")...UInt8(ascii: "f"): return Int(byte - UInt8(ascii: "a")) + 10
        case UInt8(ascii: "A")...UInt8(ascii: "F"): return Int(byte - UInt8(ascii: "A")) + 10
        default: return nil
        }
    }
}

public enum ColourToken: String, CaseIterable, Sendable {
    case accent, playing, selection, success, warning, error

    public var defaultsKey: String { "colour." + rawValue }
}

public struct ColourSettings: Equatable, Sendable {
    public static let tintKey = "colour.tintFromArtwork"

    /// Absent token = System.
    public var custom: [ColourToken: RGB]
    public var tintFromArtwork: Bool

    public init(custom: [ColourToken: RGB] = [:], tintFromArtwork: Bool = true) {
        self.custom = custom
        self.tintFromArtwork = tintFromArtwork
    }

    public subscript(token: ColourToken) -> RGB? {
        get { custom[token] }
        set { custom[token] = newValue }
    }

    /// Loads saved settings. An unparseable value is System.
    public init(defaults: UserDefaults) {
        var custom: [ColourToken: RGB] = [:]
        for token in ColourToken.allCases {
            if let text = defaults.string(forKey: token.defaultsKey), let rgb = RGB(hex: text) {
                custom[token] = rgb
            }
        }
        self.init(custom: custom,
                  tintFromArtwork: defaults.object(forKey: ColourSettings.tintKey) as? Bool ?? true)
    }

    /// Writes hex strings. Removes the key for System.
    public func save(to defaults: UserDefaults) {
        for token in ColourToken.allCases {
            if let rgb = custom[token] {
                defaults.set(rgb.hex, forKey: token.defaultsKey)
            } else {
                defaults.removeObject(forKey: token.defaultsKey)
            }
        }
        defaults.set(tintFromArtwork, forKey: ColourSettings.tintKey)
    }

    /// Applies a parsed `colour`, `colours` or `tint` command.
    public mutating func apply(_ command: ParsedCommand) throws {
        let arguments = command.arguments
        switch command.id {
        case .colour:
            guard let token = ColourToken(rawValue: arguments[0]) else { throw CommandLanguageError("Unknown colour token.") }
            self[token] = arguments[1] == "system" ? nil : RGB(hex: arguments[1])
        case .colours:
            self = ColourSettings()
        case .tint:
            tintFromArtwork = arguments[0] == "on"
        default:
            throw CommandLanguageError("Not a colour command.")
        }
    }
}
