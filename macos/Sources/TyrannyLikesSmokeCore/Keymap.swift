import Foundation
import Observation

/// One effective binding: a chord and the command it runs. `isCustom` is false only for an
/// untouched default.
public struct KeyBinding: Equatable, Sendable {
    public let chord: KeyChord
    public let command: String
    public let isCustom: Bool
}

public enum Keymap {
    /// The bindings every user starts with. Commands have no leading `:`.
    public static let defaults: [(chord: String, command: String)] = [
        ("cmd+f", "focus-filter"),
        ("cmd+b", "browser toggle"),
        ("cmd+opt+u", "drawer toggle"),
        ("cmd+right", "next"),
        ("cmd+left", "previous"),
        ("cmd+l", "reveal-playing"),
        ("cmd+opt+e", "equalizer"),
        ("cmd+opt+l", "lyrics toggle"),
        ("cmd+1", "go songs"),
        ("cmd+2", "go albums"),
        ("cmd+3", "go artists"),
        ("cmd+4", "go genres"),
        ("cmd+5", "go recent"),
        ("cmd+6", "go up-next"),
        ("cmd+7", "go history"),
        ("cmd+8", "go stats"),
        ("cmd+9", "go radio"),
    ]

    private static let reservedCommandKeys: Set<String> = ["q", "w", "h", "m", ",", "a", "c", "v", "x", "z"]
    private static let tableKeys: Set<String> = [
        "space", "return", "escape", "tab", "delete", "forwarddelete", "left", "right", "up", "down",
    ]

    /// Why a chord cannot be bound, or nil when it can.
    public static func reservedReason(_ chord: KeyChord) -> String? {
        if chord.modifiers == [.command], reservedCommandKeys.contains(chord.key.lowercased()) {
            return "\(chord.display) is a system or editing shortcut."
        }
        if chord.modifiers.subtracting(.shift).isEmpty, tableKeys.contains(chord.key) {
            return "\(chord.display) belongs to the lists and text fields."
        }
        if chord.key == ":", chord.modifiers.isEmpty {
            return "\(chord.display) opens the command palette."
        }
        return nil
    }
}

/// The effective keymap: `Keymap.defaults` plus the user's changes.
///
/// `UserDefaults` holds only the changes, as chord text to command. An empty command removes a
/// default. Defaults added in a later version therefore reach existing users. A stored chord that
/// does not read is ignored.
@MainActor @Observable
public final class KeymapStore {
    public static let defaultsKey = "keymapChanges"

    /// Defaults in their own order, then custom chords by canonical text.
    public private(set) var bindings: [KeyBinding] = []

    private let defaults: UserDefaults
    private var changes: [String: String]

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let stored = defaults.dictionary(forKey: Self.defaultsKey) as? [String: String] ?? [:]
        var changes: [String: String] = [:]
        for (text, command) in stored.sorted(by: { $0.key < $1.key }) {
            guard let chord = KeyChord(text) else { continue }
            changes[chord.canonical] = command
        }
        self.changes = changes
        rebuild()
    }

    public func command(for chord: KeyChord) -> String? {
        bindings.first { $0.chord == chord }?.command
    }

    /// The first chord bound to the command, compared after normalising both sides.
    public func chord(forCommand command: String) -> KeyChord? {
        let wanted = CommandHistory.normalised(command)
        return bindings.first { CommandHistory.normalised($0.command) == wanted }?.chord
    }

    /// Binds the chord to the command, replacing what it ran. An empty command does nothing.
    public func bind(_ chord: KeyChord, to command: String) {
        let command = CommandHistory.normalised(command)
        guard !command.isEmpty else { return }
        changes[chord.canonical] = command
        commit()
    }

    /// A default is kept as a removal. A custom chord is dropped.
    public func remove(_ chord: KeyChord) {
        if Keymap.defaults.contains(where: { KeyChord($0.chord) == chord }) {
            changes[chord.canonical] = ""
        } else {
            changes.removeValue(forKey: chord.canonical)
        }
        commit()
    }

    public func resetAll() {
        changes = [:]
        commit()
    }

    private func commit() {
        if changes.isEmpty {
            defaults.removeObject(forKey: Self.defaultsKey)
        } else {
            defaults.set(changes, forKey: Self.defaultsKey)
        }
        rebuild()
    }

    private func rebuild() {
        var result: [KeyBinding] = []
        var defaultChords: Set<String> = []
        for entry in Keymap.defaults {
            guard let chord = KeyChord(entry.chord) else { continue }
            defaultChords.insert(chord.canonical)
            switch changes[chord.canonical] {
            case nil: result.append(KeyBinding(chord: chord, command: entry.command, isCustom: false))
            case "": break
            case let command?: result.append(KeyBinding(chord: chord, command: command, isCustom: true))
            }
        }
        for (text, command) in changes.sorted(by: { $0.key < $1.key })
        where !command.isEmpty && !defaultChords.contains(text) {
            guard let chord = KeyChord(text) else { continue }
            result.append(KeyBinding(chord: chord, command: command, isCustom: true))
        }
        bindings = result
    }
}
