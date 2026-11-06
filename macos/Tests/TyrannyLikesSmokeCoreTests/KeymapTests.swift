import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@MainActor @Suite struct KeymapTests {
    private func isolatedDefaults() -> (UserDefaults, () -> Void) {
        let name = "KeymapTests.\(UUID())"
        let defaults = UserDefaults(suiteName: name)!
        return (defaults, { defaults.removePersistentDomain(forName: name) })
    }

    private func chord(_ text: String) -> KeyChord { KeyChord(text)! }

    @Test func defaultChordsAndCommandsParse() {
        for entry in Keymap.defaults {
            #expect(KeyChord(entry.chord) != nil, "chord \(entry.chord)")
            #expect((try? CommandLanguage.parse(entry.command)) != nil, "command \(entry.command)")
        }
    }

    @Test func defaultsIncludeGoShortcuts() {
        #expect(Keymap.defaults.contains { $0.chord == "cmd+1" && $0.command == "go songs" })
        #expect(Keymap.defaults.contains { $0.chord == "cmd+9" && $0.command == "go radio" })
    }

    @Test func searchKeysAreBoundAndFreeOfTheEditor() {
        let expected = [("/", "search"), ("n", "search-next"), ("N", "search-previous")]
        for (text, command) in expected {
            #expect(Keymap.defaults.contains { $0.chord == text && $0.command == command }, "\(text)")
            #expect(Keymap.reservedReason(chord(text)) == nil, "\(text)")
            #expect(!KeyDispatch.shouldRun(chord(text), isMainWindow: true, hasSheet: false, overlayOpen: false, isEditingText: true), "\(text)")
            #expect(KeyDispatch.shouldRun(chord(text), isMainWindow: true, hasSheet: false, overlayOpen: false, isEditingText: false), "\(text)")
            #expect(!chord(text).hasMenuShortcut, "\(text)")
        }
    }

    @Test func reservedChords() {
        for text in ["cmd+q", "cmd+w", "cmd+h", "cmd+m", "cmd+,", "cmd+a", "cmd+c", "cmd+v", "cmd+x", "cmd+z", "cmd+Z",
                     "space", "return", "escape", "tab", "delete", "forwarddelete", "left", "shift+down", ":"] {
            #expect(Keymap.reservedReason(chord(text)) != nil, "\(text)")
        }
        for text in ["cmd+left", "opt+space", "n", "/", "cmd+l",
                     "cmd+C", "cmd+Q", "cmd+W", "cmd+H", "cmd+M", "cmd+A", "cmd+V", "cmd+X"] {
            #expect(Keymap.reservedReason(chord(text)) == nil, "\(text)")
        }
    }

    @Test func storeStartsWithDefaults() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        #expect(store.bindings.map(\.command) == Keymap.defaults.map(\.command))
        #expect(store.bindings.allSatisfy { !$0.isCustom })
        #expect(store.command(for: chord("cmd+l")) == "reveal-playing")
    }

    @Test func customBindingSurvivesANewStore() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        KeymapStore(defaults: defaults).bind(chord("ctrl+j"), to: "go albums")
        let reloaded = KeymapStore(defaults: defaults)
        #expect(reloaded.command(for: chord("ctrl+j")) == "go albums")
        #expect(reloaded.bindings.last?.isCustom == true)
    }

    @Test func removedDefaultStaysRemoved() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        KeymapStore(defaults: defaults).remove(chord("cmd+l"))
        let reloaded = KeymapStore(defaults: defaults)
        #expect(reloaded.command(for: chord("cmd+l")) == nil)
        #expect(reloaded.command(for: chord("cmd+f")) == "focus-filter")
    }

    @Test func tombstoneKeepsOtherDefaultsIncludingNewOnes() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        defaults.set(["opt+cmd+u": ""], forKey: KeymapStore.defaultsKey)
        let store = KeymapStore(defaults: defaults)
        #expect(store.command(for: chord("cmd+opt+u")) == nil)
        #expect(store.bindings.count == Keymap.defaults.count - 1)
        #expect(store.command(for: chord("cmd+1")) == "go songs")
        #expect(store.command(for: chord("cmd+l")) == "reveal-playing")
    }

    @Test func bindingOverADefaultMarksItCustom() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        store.bind(chord("cmd+l"), to: "go albums")
        #expect(store.command(for: chord("cmd+l")) == "go albums")
        #expect(store.bindings.first { $0.chord == chord("cmd+l") }?.isCustom == true)
        #expect(store.bindings.count == Keymap.defaults.count)
    }

    @Test func removingACustomChordDropsIt() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        store.bind(chord("ctrl+j"), to: "go albums")
        store.remove(chord("ctrl+j"))
        #expect(store.command(for: chord("ctrl+j")) == nil)
        #expect(KeymapStore(defaults: defaults).command(for: chord("ctrl+j")) == nil)
        #expect(defaults.object(forKey: KeymapStore.defaultsKey) == nil)
    }

    @Test func resetAllRestoresDefaultsAndClearsTheKey() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        store.remove(chord("cmd+l")); store.bind(chord("ctrl+j"), to: "go albums")
        store.resetAll()
        #expect(store.bindings.map(\.command) == Keymap.defaults.map(\.command))
        #expect(defaults.object(forKey: KeymapStore.defaultsKey) == nil)
    }

    @Test func unreadableStoredChordIsIgnored() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        defaults.set(["hyper+x": "go albums", "shift+n": "go songs"], forKey: KeymapStore.defaultsKey)
        let store = KeymapStore(defaults: defaults)
        #expect(store.bindings.count == Keymap.defaults.count)
    }

    @Test func findsTheChordForACommand() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        #expect(store.chord(forCommand: ":reveal-playing") == chord("cmd+l"))
        #expect(store.chord(forCommand: "no-such-command") == nil)
    }

    @Test func bindNormalisesTheCommand() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        store.bind(chord("ctrl+j"), to: ":seek +1m")
        #expect(store.command(for: chord("ctrl+j")) == "seek +1m")
    }

    @Test func bindingADefaultsOwnCommandRemovesTheChange() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        store.bind(chord("cmd+l"), to: "go albums")
        store.bind(chord("cmd+l"), to: ":reveal-playing")
        #expect(store.bindings.first { $0.chord == chord("cmd+l") }?.isCustom == false)
        #expect(defaults.object(forKey: KeymapStore.defaultsKey) == nil)
    }

    @Test func addingBackARemovedDefaultIsNotCustom() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        store.remove(chord("cmd+l"))
        store.bind(chord("cmd+l"), to: "reveal-playing")
        #expect(store.bindings.first { $0.chord == chord("cmd+l") }?.isCustom == false)
        let stored = defaults.dictionary(forKey: KeymapStore.defaultsKey) as? [String: String] ?? [:]
        #expect(stored[chord("cmd+l").canonical] == nil)
    }
}
