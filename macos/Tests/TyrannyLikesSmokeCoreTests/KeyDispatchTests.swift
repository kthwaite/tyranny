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
