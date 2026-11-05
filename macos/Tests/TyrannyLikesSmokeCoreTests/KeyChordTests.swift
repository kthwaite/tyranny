import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct KeyChordTests {
    @Test func parsesAndNormalisesOrder() {
        #expect(KeyChord("cmd+opt+e")?.canonical == "opt+cmd+e")
        #expect(KeyChord("cmd++")?.key == "+")
        #expect(KeyChord("shift+f5")?.canonical == "shift+f5")
    }

    @Test func rejectsUnreadableChords() {
        #expect(KeyChord("shift+n") == nil)
        #expect(KeyChord("") == nil)
        #expect(KeyChord("hyper+x") == nil)
    }

    @Test func displaysSymbols() {
        #expect(KeyChord("opt+cmd+e")?.display == "⌥⌘E")
        #expect(KeyChord("cmd+right")?.display == "⌘→")
        #expect(KeyChord("n")?.display == "n")
        #expect(KeyChord("N")?.display == "N")
    }

    @Test func eventLetterCase() {
        #expect(KeyChord.event(characters: "n", keyCode: 45, modifiers: [], capsLock: true)?.canonical == "n")
        #expect(KeyChord.event(characters: "N", keyCode: 45, modifiers: [], capsLock: true)?.canonical == "n")
        #expect(KeyChord.event(characters: "N", keyCode: 45, modifiers: [.shift], capsLock: false)?.canonical == "N")
        #expect(KeyChord.event(characters: "n", keyCode: 45, modifiers: [.shift, .command], capsLock: false)?.canonical == "cmd+N")
    }

    @Test func eventDropsShiftFromCharacterKeys() {
        #expect(KeyChord.event(characters: ":", keyCode: 41, modifiers: [.shift], capsLock: false)?.canonical == ":")
    }

    @Test func eventNamedKeysComeFromKeyCode() {
        #expect(KeyChord.event(characters: "\u{F702}", keyCode: 123, modifiers: [.command], capsLock: false)?.canonical == "cmd+left")
        #expect(KeyChord.event(characters: "\u{F702}", keyCode: 123, modifiers: [.shift], capsLock: false)?.canonical == "shift+left")
        #expect(KeyChord.event(characters: "\r", keyCode: 36, modifiers: [], capsLock: false)?.canonical == "return")
        #expect(KeyChord.event(characters: "\r", keyCode: 76, modifiers: [], capsLock: false)?.canonical == "return")
        #expect(KeyChord.event(characters: nil, keyCode: 96, modifiers: [], capsLock: false)?.canonical == "f5")
    }

    @Test func eventWithNoCharactersAndUnknownKeyCodeIsNil() {
        #expect(KeyChord.event(characters: "", keyCode: 200, modifiers: [], capsLock: false) == nil)
        #expect(KeyChord.event(characters: nil, keyCode: 200, modifiers: [.command], capsLock: false) == nil)
    }

    @Test func isNamedKey() {
        #expect(KeyChord("cmd+left")?.isNamedKey == true)
        #expect(KeyChord("n")?.isNamedKey == false)
    }

    @Test func menuShortcutNeedsCommand() {
        for text in ["cmd+l", "ctrl+cmd+l"] { #expect(KeyChord(text)?.hasMenuShortcut == true, "\(text)") }
        for text in ["n", "opt+x", "ctrl+a", "shift+f5"] { #expect(KeyChord(text)?.hasMenuShortcut == false, "\(text)") }
    }
}
