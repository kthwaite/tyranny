import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@MainActor @Suite struct KeymapEditorTests {
    private func isolatedDefaults() -> (UserDefaults, () -> Void) {
        let name = "KeymapEditorTests.\(UUID())"
        let defaults = UserDefaults(suiteName: name)!
        return (defaults, { defaults.removePersistentDomain(forName: name) })
    }

    private func chord(_ text: String) -> KeyChord { KeyChord(text)! }

    @Test func newEditorIsIncomplete() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let editor = KeymapEditor(store: KeymapStore(defaults: defaults))
        #expect(editor.verdict == .incomplete)
        #expect(!editor.canSave)
    }

    @Test func invalidCommandCarriesTheParserMessage() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let editor = KeymapEditor(store: KeymapStore(defaults: defaults))
        editor.chord = chord("cmd+s")
        editor.command = "seek +1q"
        var message = ""
        do { _ = try CommandLanguage.parse("seek +1q") } catch { message = error.localizedDescription }
        #expect(!message.isEmpty)
        #expect(editor.verdict == .invalidCommand(message))
        #expect(!editor.canSave)
    }

    @Test func reservedChordCannotBeSaved() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let editor = KeymapEditor(store: KeymapStore(defaults: defaults))
        editor.chord = chord("cmd+q")
        for command in ["seek +1m", "", "seek +1q"] {
            editor.command = command
            guard case .reserved = editor.verdict else { Issue.record("not reserved for \(command)"); continue }
            #expect(!editor.canSave)
        }
    }

    @Test func conflictReplacesTheOldBinding() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        let editor = KeymapEditor(store: store)
        editor.chord = chord("cmd+l")
        editor.command = "seek +1m"
        #expect(editor.verdict == .conflict(command: "reveal-playing"))
        #expect(editor.canSave)
        #expect(editor.saveTitle == "Replace")
        editor.save()
        #expect(store.command(for: chord("cmd+l")) == "seek +1m")
    }

    @Test func readyEditorSaysSave() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let editor = KeymapEditor(store: KeymapStore(defaults: defaults))
        editor.chord = chord("cmd+j")
        editor.command = "seek +1m"
        #expect(editor.verdict == .ready)
        #expect(editor.saveTitle == "Save")
    }

    @Test func keepingTheEditedChordIsNotAConflict() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        let row = store.bindings.first { $0.chord == chord("cmd+b") }!
        let editor = KeymapEditor(store: store, editing: row)
        #expect(editor.chord == chord("cmd+b"))
        #expect(editor.command == "browser toggle")
        #expect(editor.verdict == .ready)
    }

    @Test func movingARowUnbindsItsOldChord() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        let row = store.bindings.first { $0.chord == chord("cmd+b") }!
        let editor = KeymapEditor(store: store, editing: row)
        editor.chord = chord("cmd+j")
        editor.save()
        #expect(store.command(for: chord("cmd+b")) == nil)
        #expect(store.command(for: chord("cmd+j")) == "browser toggle")
    }

    @Test func leadingColonIsNotStored() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        let editor = KeymapEditor(store: store)
        editor.chord = chord("cmd+j")
        editor.command = ":seek +1m"
        #expect(editor.verdict == .ready)
        editor.save()
        #expect(store.command(for: chord("cmd+j")) == "seek +1m")
    }

    @Test func boundChordShowsItsCommandBeforeACommandIsTyped() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let editor = KeymapEditor(store: KeymapStore(defaults: defaults))
        editor.chord = chord("cmd+l")
        #expect(editor.conflictingCommand == "reveal-playing")
        #expect(editor.saveTitle == "Replace")
        #expect(!editor.canSave)
        #expect(editor.verdict == .incomplete)
    }

    @Test func freeReservedAndOwnChordsHaveNoConflictingCommand() {
        let (defaults, cleanup) = isolatedDefaults(); defer { cleanup() }
        let store = KeymapStore(defaults: defaults)
        let editor = KeymapEditor(store: store)
        #expect(editor.conflictingCommand == nil)
        editor.chord = chord("cmd+j")
        #expect(editor.conflictingCommand == nil)
        editor.chord = chord("cmd+q")
        #expect(editor.conflictingCommand == nil)
        let row = store.bindings.first { $0.chord == chord("cmd+b") }!
        #expect(KeymapEditor(store: store, editing: row).conflictingCommand == nil)
    }
}
