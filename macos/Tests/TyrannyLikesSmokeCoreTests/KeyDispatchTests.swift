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
            while gates.count < expected { await Task.yield() }
            #expect(gates.count == expected)
            gates[expected - 1].resume()
            while log.count < expected * 2 { await Task.yield() }
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
        while ran.count < 2 || reports.isEmpty { await Task.yield() }
        #expect(ran == ["bad", "good"])
        #expect(reports == ["boom"])
    }
}
