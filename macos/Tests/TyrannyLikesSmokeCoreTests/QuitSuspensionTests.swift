import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct QuitSuspensionTests {
    private let note = QuitSuspension(socketPath: "/private/tmp/a/control.sock", entryID: 4)

    /// A per-test preference domain; `body` runs with it, then the domain is removed.
    private func withFreshDefaults(_ body: (UserDefaults) -> Void) {
        let name = "QuitSuspensionTests.\(UUID())"
        let defaults = UserDefaults(suiteName: name)!
        defer { defaults.removePersistentDomain(forName: name) }
        body(defaults)
    }

    @Test func noteWhenPlaying() {
        #expect(QuitSuspension.note(socketPath: "/private/tmp/a/control.sock", transport: "playing", currentID: 4) == note)
    }

    @Test func noteTreatsBufferingAsPlaying() {
        #expect(QuitSuspension.note(socketPath: "/private/tmp/a/control.sock", transport: "buffering", currentID: 4) == note)
    }

    @Test func noNoteWhenPausedStoppedOrUnknown() {
        for transport in ["paused", "stopped", "ended", "error", nil] as [String?] {
            #expect(QuitSuspension.note(socketPath: "/private/tmp/a/control.sock", transport: transport, currentID: 4) == nil)
        }
        #expect(QuitSuspension.note(socketPath: "/private/tmp/a/control.sock", transport: "playing", currentID: nil) == nil)
    }

    @Test func resumesWhenPausedOnTheSameEntry() {
        #expect(note.shouldResume(socketPath: "/private/tmp/a/control.sock", transport: "paused", currentID: 4))
    }

    @Test func doesNotResumeOnAnotherSocket() {
        #expect(!note.shouldResume(socketPath: "/private/tmp/b/control.sock", transport: "paused", currentID: 4))
    }

    @Test func doesNotResumeWhenTheCurrentEntryChanged() {
        #expect(!note.shouldResume(socketPath: "/private/tmp/a/control.sock", transport: "paused", currentID: 5))
    }

    @Test func doesNotResumeWhenAlreadyPlaying() {
        for transport in ["playing", "stopped", nil] as [String?] {
            #expect(!note.shouldResume(socketPath: "/private/tmp/a/control.sock", transport: transport, currentID: 4))
        }
    }

    @Test func storeTakeReturnsTheNoteOnce() {
        withFreshDefaults { defaults in
            let store = QuitSuspensionStore(defaults: defaults)
            store.save(note)
            #expect(store.take() == note)
            #expect(store.take() == nil)
        }
    }

    @Test func storeTakeDropsUndecodableData() {
        withFreshDefaults { defaults in
            defaults.set(Data("x".utf8), forKey: QuitSuspensionStore.key)
            let store = QuitSuspensionStore(defaults: defaults)
            #expect(store.take() == nil)
            #expect(defaults.object(forKey: QuitSuspensionStore.key) == nil)
        }
    }

    @Test func normalLaunchUsesStandardDefaults() {
        #expect(QuitSuspensionStore.suiteName(forArguments: ["/Applications/tyranny likes smoke.app/Contents/MacOS/tyranny likes smoke"]) == nil)
        #expect(QuitSuspensionStore.suiteName(forArguments: ["tyranny likes smoke", "--socket", "/private/tmp/s/control.sock"]) == nil)
    }

    @Test func harnessLaunchesUseTheHarnessSuite() {
        let launches: [[String]] = [
            ["--render-views", "/out"],
            ["--ui-smoke-output", "/r.json", "--ui-smoke-folder", "/m"],
            ["--ui-smoke-relaunch"],
            ["--shell-smoke-output", "/r.json"],
            ["--lifecycle-output", "/r.json", "--lifecycle-action", "observe"],
            ["--managed-state-dir", "/s", "--managed-label", "dev.tyranny-likes-smoke.spike.test", "--managed-agent-dir", "/a"],
            ["--model-gate-output", "/r.json"],
            ["--scale-output", "/r.json"],
        ]
        for arguments in launches {
            #expect(QuitSuspensionStore.suiteName(forArguments: ["tyranny likes smoke"] + arguments)
                    == QuitSuspensionStore.harnessSuiteName)
        }
    }

    /// The first argument is the executable path, never a flag.
    @Test func theExecutablePathIsNotAFlag() {
        #expect(QuitSuspensionStore.suiteName(forArguments: ["--render-views"]) == nil)
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
            await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
                DispatchQueue.global().asyncAfter(deadline: .now() + 5) { c.resume() }
            }
            return 0
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
