import Foundation
import Testing
@testable import TyrannyCore

@Suite struct LaunchPrefetchTests {
    private let r: [String: JSONValue] = ["list": .string("songs"), "revision": .null,
                                          "offset": .int(0), "limit": .int(200)]
    private let data = Data("reply".utf8)
    private var soon: ContinuousClock.Instant { ContinuousClock.now + .seconds(1) }

    private func seconds(_ d: Duration) -> Double {
        Double(d.components.seconds) + Double(d.components.attoseconds) / 1e18
    }

    @Test func fulfilledEqualRequestIsClaimed() async {
        let slot = LaunchPrefetchSlot(request: r)
        await slot.fulfil(data)
        #expect(await slot.claim(r, until: soon) == data)
    }

    @Test func differentFieldsFallBackAndEmptyTheSlot() async {
        let slot = LaunchPrefetchSlot(request: r)
        await slot.fulfil(data)
        var other = r
        other["limit"] = .int(100)
        #expect(await slot.claim(other, until: soon) == nil)
        #expect(await slot.claim(r, until: soon) == nil)
    }

    @Test func onlyTheFirstClaimCanSucceed() async {
        let slot = LaunchPrefetchSlot(request: r)
        await slot.fulfil(data)
        #expect(await slot.claim(r, until: soon) == data)
        #expect(await slot.claim(r, until: soon) == nil)
    }

    @Test func inFlightPrefetchIsAwaited() async throws {
        let slot = LaunchPrefetchSlot(request: r)
        let request = r
        let deadline = soon
        let claim = Task { await slot.claim(request, until: deadline) }
        try await Task.sleep(for: .milliseconds(20))
        await slot.fulfil(data)
        #expect(await claim.value == data)
    }

    @Test func failedPrefetchReturnsNilAtOnce() async {
        let slot = LaunchPrefetchSlot(request: r)
        await slot.fulfil(nil)
        let clock = ContinuousClock()
        var result: Data?
        let elapsed = await clock.measure { result = await slot.claim(r, until: clock.now + .seconds(5)) }
        #expect(result == nil)
        #expect(seconds(elapsed) < 0.5)
    }

    @Test func inFlightFailureReturnsNilPromptly() async throws {
        let slot = LaunchPrefetchSlot(request: r)
        let request = r
        let clock = ContinuousClock()
        let start = clock.now
        let claim = Task { await slot.claim(request, until: clock.now + .seconds(5)) }
        try await Task.sleep(for: .milliseconds(20))
        await slot.fulfil(nil)
        #expect(await claim.value == nil)
        #expect(seconds(clock.now - start) < 0.5)
    }

    @Test func claimGivesUpAtTheDeadline() async {
        let slot = LaunchPrefetchSlot(request: r)
        let clock = ContinuousClock()
        var result: Data?
        let elapsed = await clock.measure { result = await slot.claim(r, until: clock.now + .milliseconds(50)) }
        #expect(result == nil)
        #expect(seconds(elapsed) >= 0.04)
        #expect(seconds(elapsed) < 1)
    }

    @Test func fulfilAfterTheClaimIsIgnored() async {
        let slot = LaunchPrefetchSlot(request: r)
        let clock = ContinuousClock()
        #expect(await slot.claim(r, until: clock.now + .milliseconds(30)) == nil)
        await slot.fulfil(data)
        #expect(await slot.claim(r, until: soon) == nil)
    }

    @Test func equalityIsStructural() async {
        var a: [String: JSONValue] = [:]
        a["list"] = .string("songs"); a["revision"] = .null; a["offset"] = .int(0); a["limit"] = .int(200)
        var b: [String: JSONValue] = [:]
        b["limit"] = .int(200); b["offset"] = .int(0); b["revision"] = .null; b["list"] = .string("songs")
        let slot = LaunchPrefetchSlot(request: a)
        await slot.fulfil(data)
        #expect(await slot.claim(b, until: soon) == data)
    }
}
