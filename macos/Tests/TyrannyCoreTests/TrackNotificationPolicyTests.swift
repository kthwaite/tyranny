import Testing
@testable import TyrannyCore

@Suite struct TrackNotificationPolicyTests {
    @Test func reconnectAndPositionTicksDoNotRepeatNotifications() {
        var policy = TrackNotificationPolicy()
        #expect(!step(&policy, 1, newSession: true))
        #expect(!step(&policy, 1))
        #expect(step(&policy, 2))
        #expect(!step(&policy, 2))
        #expect(!step(&policy, 3, newSession: true))
        #expect(!step(&policy, 3))
        #expect(step(&policy, 4))
    }

    @Test func bufferingWaitsForPlaybackAndPauseResumeIsQuiet() {
        var policy = TrackNotificationPolicy()
        #expect(!step(&policy, nil, newSession: true))
        #expect(!step(&policy, 1, transport: "buffering"))
        #expect(step(&policy, 1))
        #expect(!step(&policy, 1, transport: "paused"))
        #expect(!step(&policy, 1))
        #expect(!step(&policy, 2, transport: "buffering"))
        #expect(step(&policy, 3)) // Skipped buffered occurrence never notifies.
    }

    @Test func suppressedOccurrencesAreNotReplayedWhenPreferenceOrFocusChanges() {
        var policy = TrackNotificationPolicy()
        #expect(!step(&policy, nil, newSession: true))
        #expect(!step(&policy, 1, enabled: false))
        #expect(!step(&policy, 1))
        #expect(!step(&policy, 2, frontmost: true))
        #expect(!step(&policy, 2))
        #expect(step(&policy, 3, frontmost: true, showWhileActive: true))
        #expect(!step(&policy, 3, frontmost: true, showWhileActive: true))
    }

    private func step(_ policy: inout TrackNotificationPolicy, _ entry: UInt64?, transport: String = "playing",
                      newSession: Bool = false, enabled: Bool = true, frontmost: Bool = false,
                      showWhileActive: Bool = false) -> Bool {
        policy.shouldNotify(entryID: entry, transport: transport, newSession: newSession,
                            enabled: enabled, frontmost: frontmost, showWhileActive: showWhileActive)
    }
}
