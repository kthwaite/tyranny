import Foundation
import Testing
@testable import TyrannyCore

@Suite struct QuitSuspensionTests {
    private let note = QuitSuspension(endpoint: "ws://a", entryID: 4)

    private func freshDefaults() -> UserDefaults {
        UserDefaults(suiteName: "QuitSuspensionTests.\(UUID())")!
    }

    @Test func noteWhenPlaying() {
        #expect(QuitSuspension.note(endpoint: "ws://a", transport: "playing", currentID: 4) == note)
    }

    @Test func noteTreatsBufferingAsPlaying() {
        #expect(QuitSuspension.note(endpoint: "ws://a", transport: "buffering", currentID: 4) == note)
    }

    @Test func noNoteWhenPausedStoppedOrUnknown() {
        for transport in ["paused", "stopped", "ended", "error", nil] as [String?] {
            #expect(QuitSuspension.note(endpoint: "ws://a", transport: transport, currentID: 4) == nil)
        }
        #expect(QuitSuspension.note(endpoint: "ws://a", transport: "playing", currentID: nil) == nil)
    }

    @Test func resumesWhenPausedOnTheSameEntry() {
        #expect(note.shouldResume(endpoint: "ws://a", transport: "paused", currentID: 4))
    }

    @Test func doesNotResumeOnAnotherEndpoint() {
        #expect(!note.shouldResume(endpoint: "ws://b", transport: "paused", currentID: 4))
    }

    @Test func doesNotResumeWhenTheCurrentEntryChanged() {
        #expect(!note.shouldResume(endpoint: "ws://a", transport: "paused", currentID: 5))
    }

    @Test func doesNotResumeWhenAlreadyPlaying() {
        for transport in ["playing", "stopped", nil] as [String?] {
            #expect(!note.shouldResume(endpoint: "ws://a", transport: transport, currentID: 4))
        }
    }

    @Test func storeTakeReturnsTheNoteOnce() {
        let store = QuitSuspensionStore(defaults: freshDefaults())
        store.save(note)
        #expect(store.take() == note)
        #expect(store.take() == nil)
    }

    @Test func storeTakeDropsUndecodableData() {
        let defaults = freshDefaults()
        defaults.set(Data("x".utf8), forKey: QuitSuspensionStore.key)
        let store = QuitSuspensionStore(defaults: defaults)
        #expect(store.take() == nil)
        #expect(defaults.object(forKey: QuitSuspensionStore.key) == nil)
    }

    @Test func deadlineReturnsTheOperationResult() async {
        let value = await Deadline.race(.seconds(1)) { 42 }
        #expect(value == 42)
    }

    /// The operation swallows cancellation; the race must still return at the deadline.
    @Test func deadlineReturnsNilWhenOperationOutlivesIt() async {
        let clock = ContinuousClock()
        let start = clock.now
        let value: Int? = await Deadline.race(.milliseconds(100)) {
            while true { try? await Task.sleep(for: .seconds(10)) }
        }
        #expect(value == nil)
        #expect(clock.now - start < .seconds(1))
    }

    @Test func deadlineReturnsNilWhenOperationThrows() async {
        struct Boom: Error {}
        let value: Int? = await Deadline.race(.seconds(1)) { throw Boom() }
        #expect(value == nil)
    }
}
