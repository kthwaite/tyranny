import SwiftUI
import TyrannyLikesSmokeCore

extension KeyChord {
    /// The menu key equivalent, or nil when the chord has no ⌘ (see `hasMenuShortcut`) or SwiftUI has no
    /// key for it. A character key says Shift by its case, so an upper-case letter adds ⇧ here.
    var menuShortcut: KeyboardShortcut? {
        guard hasMenuShortcut else { return nil }
        var flags: EventModifiers = [.command]
        if modifiers.contains(.control) { flags.insert(.control) }
        if modifiers.contains(.option) { flags.insert(.option) }
        if modifiers.contains(.shift) { flags.insert(.shift) }
        let equivalent: KeyEquivalent
        if isNamedKey {
            switch key {
            case "left": equivalent = .leftArrow
            case "right": equivalent = .rightArrow
            case "up": equivalent = .upArrow
            case "down": equivalent = .downArrow
            case "space": equivalent = .space
            case "return": equivalent = .return
            case "escape": equivalent = .escape
            case "tab": equivalent = .tab
            case "delete": equivalent = .delete
            case "forwarddelete": equivalent = .deleteForward
            case "home": equivalent = .home
            case "end": equivalent = .end
            case "pageup": equivalent = .pageUp
            case "pagedown": equivalent = .pageDown
            default: return nil
            }
        } else {
            let character = Character(key)
            if character.isUppercase { flags.insert(.shift) }
            equivalent = KeyEquivalent(Character(key.lowercased()))
        }
        return KeyboardShortcut(equivalent, modifiers: flags)
    }
}

extension KeymapStore {
    /// Help text that names the command's key, or just `text` when the command has none.
    func help(_ text: String, for command: String) -> String {
        chord(forCommand: command).map { "\(text) (\($0.display))" } ?? text
    }
}
