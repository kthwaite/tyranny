import Foundation

/// A key plus ⌃, ⌥ and ⌘, as the user binds it to a command.
///
/// A character key (`l`, `N`, `/`, `:`) does not carry ⇧, because its case or symbol already says
/// Shift. A named key (an arrow, Return, F5) can carry ⇧.
/// Canonical text lists modifiers as `ctrl+opt+shift+cmd`, then the key: `opt+cmd+e`, `shift+f5`, `N`.
public struct KeyChord: Hashable, Sendable {
    public struct Modifiers: OptionSet, Hashable, Sendable {
        public let rawValue: Int
        public init(rawValue: Int) { self.rawValue = rawValue }

        public static let control = Modifiers(rawValue: 1 << 0)
        public static let option = Modifiers(rawValue: 1 << 1)
        public static let shift = Modifiers(rawValue: 1 << 2)
        public static let command = Modifiers(rawValue: 1 << 3)
    }

    /// `l`, `N`, `/`, or a named key: `left`, `f5`, `return`.
    public let key: String
    /// ⇧ is only ever set with a named key.
    public let modifiers: Modifiers

    /// Reads canonical text. Modifier order does not matter. Returns nil when the text is unreadable.
    public init?(_ canonical: String) {
        var rest = Substring(canonical)
        let keyText: String
        if rest.hasSuffix("+") {
            // The key is `+` itself, so `cmd++` works.
            keyText = "+"
            rest = rest.dropLast()
            if !rest.isEmpty {
                guard rest.hasSuffix("+") else { return nil }
                rest = rest.dropLast()
            }
        } else if let split = rest.lastIndex(of: "+") {
            keyText = String(rest[rest.index(after: split)...])
            rest = rest[..<split]
        } else {
            keyText = String(rest)
            rest = ""
        }

        var modifiers: Modifiers = []
        if !rest.isEmpty {
            for token in rest.split(separator: "+", omittingEmptySubsequences: false) {
                guard let modifier = Self.modifierNames.first(where: { $0.name == token })?.modifier else { return nil }
                modifiers.insert(modifier)
            }
        }

        let key: String
        if keyText.count == 1 {
            guard !keyText.first!.isWhitespace else { return nil }
            key = keyText
        } else if Self.namedKeys.contains(where: { $0.name == keyText.lowercased() }) {
            key = keyText.lowercased()
        } else {
            return nil
        }
        if modifiers.contains(.shift), key.count == 1 { return nil }
        self.key = key
        self.modifiers = modifiers
    }

    private init(key: String, modifiers: Modifiers) {
        self.key = key
        self.modifiers = modifiers
    }

    public var isNamedKey: Bool { key.count > 1 }

    /// A menu item can carry the chord only with ⌘. Without it the menu would take the key from text fields.
    public var hasMenuShortcut: Bool { modifiers.contains(.command) }

    /// `ctrl+opt+shift+cmd+key`.
    public var canonical: String {
        let names = Self.modifierNames.filter { modifiers.contains($0.modifier) }.map(\.name)
        return (names + [key]).joined(separator: "+")
    }

    /// Menu-style text: `⌥⌘E`, `⌘→`, `n`. A letter is upper-case when a modifier is present.
    public var display: String {
        let symbols = Self.modifierNames.filter { modifiers.contains($0.modifier) }.map(\.symbol).joined()
        let keyText: String
        if let named = Self.namedKeys.first(where: { $0.name == key }) {
            keyText = named.symbol
        } else {
            keyText = symbols.isEmpty ? key : key.uppercased()
        }
        return symbols + keyText
    }

    /// Reads a key press. Named keys come from the key code. Other keys come from `characters`
    /// (the characters ignoring modifiers). ⇧ turns a letter upper-case and is then dropped, as it is
    /// for any character key. Caps Lock without ⇧ gives the lower-case letter.
    /// Returns nil for a key with no characters and no known key code.
    public static func event(characters: String?, keyCode: UInt16, modifiers: Modifiers, capsLock: Bool) -> KeyChord? {
        if let named = namedKeys.first(where: { $0.codes.contains(keyCode) }) {
            return KeyChord(key: named.name, modifiers: modifiers)
        }
        guard let first = characters?.first, !first.isWhitespace else { return nil }
        var key = String(first)
        if key.lowercased() != key.uppercased() {
            key = modifiers.contains(.shift) ? key.uppercased() : key.lowercased()
        }
        return KeyChord(key: key, modifiers: modifiers.subtracting(.shift))
    }

    private static let modifierNames: [(name: String, symbol: String, modifier: Modifiers)] = [
        ("ctrl", "⌃", .control), ("opt", "⌥", .option), ("shift", "⇧", .shift), ("cmd", "⌘", .command),
    ]

    private typealias NamedKey = (name: String, symbol: String, codes: [UInt16])

    private static let namedKeys: [NamedKey] = {
        var keys: [NamedKey] = [
            ("left", "←", [123]), ("right", "→", [124]), ("down", "↓", [125]), ("up", "↑", [126]),
            ("space", "Space", [49]), ("return", "↩", [36, 76]), ("escape", "⎋", [53]), ("tab", "⇥", [48]),
            ("delete", "⌫", [51]), ("forwarddelete", "⌦", [117]),
            ("home", "↖", [115]), ("end", "↘", [119]), ("pageup", "⇞", [116]), ("pagedown", "⇟", [121]),
        ]
        // The key codes of F1 through F20.
        let functionKeyCodes: [UInt16] = [
            122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90,
        ]
        for (index, code) in functionKeyCodes.enumerated() {
            keys.append(("f\(index + 1)", "F\(index + 1)", [code]))
        }
        return keys
    }()
}
