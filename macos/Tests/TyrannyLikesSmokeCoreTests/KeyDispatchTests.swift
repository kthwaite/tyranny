import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@MainActor @Suite struct KeyDispatchTests {
    private func run(
        _ text: String, isMainWindow: Bool = true, hasSheet: Bool = false, overlayOpen: Bool = false,
        isEditingText: Bool = false
    ) -> Bool {
        KeyDispatch.shouldRun(
            KeyChord(text)!, isMainWindow: isMainWindow, hasSheet: hasSheet, overlayOpen: overlayOpen,
            isEditingText: isEditingText)
    }

    /// Yields until `condition` holds. Gives up after 10000 yields, so a regression fails instead of hanging.
    private func settle(_ what: String, until condition: () -> Bool) async {
        for _ in 0..<10_000 {
            if condition() { return }
            await Task.yield()
        }
        Issue.record("gave up waiting for \(what)")
    }

    @Test func plainChordsRunInTheMainWindow() {
        #expect(run("cmd+l"))
        #expect(run("n"))
    }

    @Test func otherWindowsSheetsAndOverlaysBlockEveryChord() {
        for text in ["cmd+l", "n"] {
            #expect(!run(text, isMainWindow: false))
            #expect(!run(text, hasSheet: true))
            #expect(!run(text, overlayOpen: true))
        }
    }

    @Test func editingTextKeepsKeysThatTheFieldTypes() {
        for text in ["n", "/", "opt+x", "ctrl+a"] {
            #expect(!run(text, isEditingText: true), "\(text)")
        }
    }

    @Test func editingTextKeepsCommandCursorAndDeleteKeys() {
        for text in ["cmd+left", "cmd+right", "cmd+up", "cmd+down", "cmd+delete", "cmd+forwarddelete"] {
            #expect(!run(text, isEditingText: true), "\(text)")
        }
    }

    @Test func editingTextStillRunsOtherCommandChords() {
        for text in ["cmd+f", "cmd+l", "cmd+opt+e"] {
            #expect(run(text, isEditingText: true), "\(text)")
        }
    }

    @Test func notEditingRunsEveryChord() {
        for text in ["n", "opt+x", "cmd+left"] {
            #expect(run(text), "\(text)")
        }
    }

    @Test func queueRunsCommandsInOrderOneAtATime() async {
        var log: [String] = []
        var gates: [CheckedContinuation<Void, Never>] = []
        let queue = KeyCommandQueue(
            run: { command in
                log.append("start \(command)")
                await withCheckedContinuation { gates.append($0) }
                log.append("end \(command)")
            },
            report: { _ in })
        queue.enqueue("a")
        queue.enqueue("b")
        queue.enqueue("c")
        for expected in 1...3 {
            await settle("gate \(expected)") { gates.count >= expected }
            #expect(gates.count == expected)
            gates[expected - 1].resume()
            await settle("log \(expected * 2)") { log.count >= expected * 2 }
        }
        #expect(log == ["start a", "end a", "start b", "end b", "start c", "end c"])
    }

    /// Enqueues `commands` while the first one is held, then lets every run finish. Returns the runs.
    private func runsHeldQueue(_ commands: [String]) async -> [String] {
        var ran: [String] = []
        var gates: [CheckedContinuation<Void, Never>] = []
        let queue = KeyCommandQueue(
            run: { command in
                ran.append(command)
                await withCheckedContinuation { gates.append($0) }
            },
            report: { _ in })
        queue.enqueue(commands[0])
        await settle("first gate") { gates.count >= 1 }
        for command in commands.dropFirst() { queue.enqueue(command) }
        var released = 0
        while gates.count > released {
            gates[released].resume()
            released += 1
            for _ in 0..<20 { await Task.yield() }
        }
        return ran
    }

    @Test func heldSearchKeyKeepsOneRunningAndOneWaiting() async {
        let ran = await runsHeldQueue(Array(repeating: "search-next", count: 5))
        #expect(ran == ["search-next", "search-next"])
    }

    @Test func searchKeysSeparatedByAnotherCommandAllRun() async {
        let ran = await runsHeldQueue(["search-next", "volume +5", "search-next"])
        #expect(ran == ["search-next", "volume +5", "search-next"])
    }

    @Test func otherCommandsAreNeverDropped() async {
        let ran = await runsHeldQueue(Array(repeating: "volume +5", count: 5))
        #expect(ran.count == 5)
    }

    @Test func heldSearchPreviousIsDroppedToo() async {
        let ran = await runsHeldQueue(Array(repeating: ":search-previous", count: 4))
        #expect(ran.count == 2)
    }

    @Test func queueReportsAFailureAndRunsTheNextCommand() async {
        struct Failure: LocalizedError { var errorDescription: String? { "boom" } }
        var ran: [String] = []
        var reports: [String] = []
        let queue = KeyCommandQueue(
            run: { command in
                ran.append(command)
                if command == "bad" { throw Failure() }
            },
            report: { reports.append($0) })
        queue.enqueue("bad")
        queue.enqueue("good")
        await settle("both commands and the report") { ran.count >= 2 && !reports.isEmpty }
        #expect(ran == ["bad", "good"])
        #expect(reports == ["boom"])
    }

    @Test func queueIgnoresACancelledCommandAndRunsTheNext() async {
        var ran: [String] = []
        var reports: [String] = []
        let queue = KeyCommandQueue(
            run: { command in
                ran.append(command)
                if command == "cancelled" { throw CancellationError() }
            },
            report: { reports.append($0) })
        queue.enqueue("cancelled")
        queue.enqueue("next")
        await settle("both commands") { ran.count >= 2 }
        await Task.yield()
        #expect(ran == ["cancelled", "next"])
        #expect(reports.isEmpty)
    }

    @Test func fieldKeepsCommandCursorAndDeleteChords() {
        for text in ["cmd+right", "shift+cmd+right", "opt+cmd+left", "cmd+down", "cmd+delete", "cmd+forwarddelete"] {
            #expect(KeyDispatch.fieldKeepsKey(KeyChord(text)!), "\(text)")
        }
        for text in ["cmd+f", "right", "opt+left"] {
            #expect(!KeyDispatch.fieldKeepsKey(KeyChord(text)!), "\(text)")
        }
    }

    @Test func recordingActionByWindowTextAndEscape() {
        typealias Recording = KeyRecording
        #expect(Recording.action(isRecorderWindow: true, isEditingText: false, isEscape: false) == .record)
        #expect(Recording.action(isRecorderWindow: true, isEditingText: false, isEscape: true) == .cancel)
        #expect(Recording.action(isRecorderWindow: false, isEditingText: false, isEscape: false) == .abandon)
        #expect(Recording.action(isRecorderWindow: false, isEditingText: false, isEscape: true) == .abandon)
        #expect(Recording.action(isRecorderWindow: true, isEditingText: true, isEscape: false) == .abandon)
        #expect(Recording.action(isRecorderWindow: true, isEditingText: true, isEscape: true) == .abandon)
    }
}
