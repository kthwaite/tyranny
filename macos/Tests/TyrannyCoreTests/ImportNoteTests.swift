import Foundation
import Testing
@testable import TyrannyCore

@Suite struct ImportNoteTests {
    private func withFreshDefaults(_ body: (UserDefaults) -> Void) {
        let name = "dev.tyranny.test.importnote.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defer { defaults.removePersistentDomain(forName: name) }
        body(defaults)
    }

    private let note = ImportNote(endpoint: "ws://a", rootID: 7)

    @Test func takeReturnsTheNoteOnce() {
        withFreshDefaults { defaults in
            let store = ImportNoteStore(defaults: defaults)
            store.save(note)
            #expect(store.take() == note)
            #expect(store.take() == nil)
        }
    }

    @Test func clearRemovesTheNote() {
        withFreshDefaults { defaults in
            let store = ImportNoteStore(defaults: defaults)
            store.save(note)
            store.clear()
            #expect(store.take() == nil)
        }
    }

    @Test func takeDropsUndecodableData() {
        withFreshDefaults { defaults in
            defaults.set(Data("x".utf8), forKey: ImportNoteStore.key)
            #expect(ImportNoteStore(defaults: defaults).take() == nil)
            #expect(defaults.object(forKey: ImportNoteStore.key) == nil)
        }
    }
}
