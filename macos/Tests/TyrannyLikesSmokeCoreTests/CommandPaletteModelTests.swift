import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

/// Holds a stub `submit` until the test resumes it.
@MainActor
private final class SubmitGate {
    private var held: CheckedContinuation<Void, Error>?
    private var entered: CheckedContinuation<Void, Never>?

    /// Called by the stub. Suspends until `resume`.
    func hold() async throws {
        entered?.resume()
        entered = nil
        try await withCheckedThrowingContinuation { held = $0 }
    }

    /// Returns once the stub is held.
    func waitUntilHeld() async {
        if held != nil { return }
        await withCheckedContinuation { entered = $0 }
    }

    func resume(throwing error: Error? = nil) {
        let continuation = held
        held = nil
        if let error { continuation?.resume(throwing: error) } else { continuation?.resume() }
    }
}

/// Records the stub `submit` and `report` calls.
@MainActor
private final class Calls {
    var submitted: [String] = []
    var reported: [String] = []
}

private extension CommandPaletteModel.Dropdown {
    var rows: [CommandPaletteModel.Row] {
        if case .rows(let rows) = self { return rows }
        return []
    }
}

private func fixedRows(_ count: Int) -> [CommandSuggestion] {
    (0..<count).map { CommandSuggestion(line: ":row\($0)", value: "row\($0)", summary: nil, continues: false) }
}

@MainActor @Suite struct CommandPaletteModelTests {
    private let suite = "CommandPaletteModelTests.\(UUID())"
    private let defaults: UserDefaults
    private let calls = Calls()

    init() {
        defaults = UserDefaults(suiteName: suite)!
    }

    /// A model with recording stubs. `submit` succeeds unless `failure` is given.
    private func model(history: [String] = [], failure: String? = nil, gate: SubmitGate? = nil) -> CommandPaletteModel {
        if !history.isEmpty { CommandHistory(entries: history).save(to: defaults) }
        let model = CommandPaletteModel(defaults: defaults)
        let calls = calls
        model.submit = { text in
            calls.submitted.append(text)
            if let gate { try await gate.hold() }
            if let failure { throw CommandLanguageError(failure) }
        }
        model.report = { calls.reported.append($0) }
        return model
    }

    private func clear() { defaults.removePersistentDomain(forName: suite) }

    @Test func opensWithColonAndCommandRows() {
        defer { clear() }
        let model = model()
        model.open()
        #expect(model.isOpen)
        #expect(model.mode == .commands)
        #expect(model.text == ":")
        #expect(!model.dropdown.rows.isEmpty)
        #expect(model.highlighted == 0)
        #expect(model.error == nil)
        #expect(!model.isRunning)
    }

    @Test func highlightClampsAtEnds() {
        defer { clear() }
        let model = model()
        model.suggest = { _ in fixedRows(3) }
        model.open()
        model.moveHighlight(-1)
        #expect(model.highlighted == 0)
        model.moveHighlight(5)
        #expect(model.highlighted == 2)
        model.moveHighlight(-1)
        #expect(model.highlighted == 1)
    }

    @Test func highlightPicksARowClampedToTheRows() {
        defer { clear() }
        let model = model()
        model.suggest = { _ in fixedRows(3) }
        model.open()
        model.highlight(2)
        #expect(model.highlighted == 2)
        model.highlight(9)
        #expect(model.highlighted == 2)
        model.highlight(-1)
        #expect(model.highlighted == 0)
    }

    @Test func highlightWithoutRowsDoesNothing() {
        defer { clear() }
        let model = model()
        model.open()
        model.text = ":seek +1"
        model.highlight(0)
        #expect(model.highlighted == nil)
    }

    @Test func clickingARowAcceptsItLikeTab() {
        defer { clear() }
        let model = model()
        model.open()
        model.text = ":rep"
        model.highlight(0)
        model.acceptHighlighted()
        model.highlight(3)
        model.acceptHighlighted()
        #expect(model.text == ":repeat one")
        #expect(calls.submitted.isEmpty)
    }

    @Test func tabAcceptsAndAddsSpaceWhenContinuing() {
        defer { clear() }
        let model = model()
        model.open()
        model.text = ":rep"
        model.acceptHighlighted()
        #expect(model.text == ":repeat ")
        #expect(model.dropdown.rows.map(\.value) == ["all", "cycle", "off", "one"])
        #expect(model.highlighted == 0)
    }

    @Test func tabWithoutRowsDoesNothing() {
        defer { clear() }
        let model = model()
        model.open()
        model.text = ":zz"
        model.acceptHighlighted()
        #expect(model.text == ":zz")
    }

    @Test func argumentStageWithoutCandidatesShowsHint() {
        defer { clear() }
        let model = model()
        model.open()
        model.text = ":seek +1"
        #expect(model.dropdown == .hint(usage: "seek <[+-]number[s|m|h|%]>",
                                        summary: "Seek absolutely, or relatively with a sign; default unit is seconds."))
        #expect(model.highlighted == nil)
    }

    @Test func unknownNameShowsEmptyAndReturnShowsError() async {
        defer { clear() }
        let model = model()
        model.submit = { _ = try CommandLanguage.parse($0) }
        model.open()
        model.text = ":zz"
        #expect(model.dropdown == .empty)
        #expect(model.highlighted == nil)
        await model.submitText()
        #expect(model.isOpen)
        #expect(model.error == "Unknown command: zz.")
        #expect(model.text == ":zz")
        #expect(!model.isRunning)
    }

    @Test func editingClearsError() async {
        defer { clear() }
        let model = model()
        model.submit = { _ = try CommandLanguage.parse($0) }
        model.open()
        model.text = ":zz"
        await model.submitText()
        #expect(model.error == "Unknown command: zz.")
        model.text = ":z"
        #expect(model.error == nil)
    }

    @Test func successClosesAndRecords() async {
        defer { clear() }
        let model = model()
        model.open()
        model.text = ":go albums"
        await model.submitText()
        #expect(calls.submitted == [":go albums"])
        #expect(!model.isOpen)
        #expect(model.history.entries == ["go albums"])
        #expect(CommandHistory.load(from: defaults).entries == ["go albums"])
    }

    @Test func failureIsNotRecorded() async {
        defer { clear() }
        let model = model(failure: "No library.")
        model.open()
        model.text = ":go albums"
        await model.submitText()
        #expect(model.isOpen)
        #expect(model.error == "No library.")
        #expect(model.history.entries.isEmpty)
        #expect(CommandHistory.load(from: defaults).entries.isEmpty)
    }

    @Test func returnRunsTypedTextNotHighlightedRow() async {
        defer { clear() }
        let model = model()
        model.open()
        model.text = ":se"
        #expect(model.highlighted == 0)
        await model.submitText()
        #expect(calls.submitted == [":se"])
    }

    @Test func historyCommandOpensPickerWithoutSubmitting() async {
        defer { clear() }
        let model = model(history: ["seek +1m", "go albums"])
        model.open()
        model.text = ":history"
        await model.submitText()
        #expect(model.isOpen)
        #expect(model.mode == .history)
        #expect(model.text == "")
        #expect(calls.submitted.isEmpty)
        #expect(model.dropdown.rows.map(\.value) == ["seek +1m", "go albums"])
        #expect(model.dropdown.rows.first == CommandPaletteModel.Row(line: ":seek +1m", value: "seek +1m", summary: nil, continues: false))
        #expect(model.highlighted == 0)
        model.text = "go"
        #expect(model.dropdown.rows.map(\.value) == ["go albums"])
        model.text = "zz"
        #expect(model.dropdown == .empty)
        #expect(model.highlighted == nil)
    }

    @Test func pickingHistoryFillsInputUnrun() async {
        defer { clear() }
        let model = model(history: ["seek +1m", "go albums"])
        model.open(.history)
        #expect(model.text == "")
        await model.submitText()
        #expect(model.mode == .commands)
        #expect(model.text == ":seek +1m")
        #expect(model.isOpen)
        #expect(calls.submitted.isEmpty)
    }

    @Test func tabPicksHistoryEntry() {
        defer { clear() }
        let model = model(history: ["seek +1m", "go albums"])
        model.open(.history)
        model.moveHighlight(1)
        model.acceptHighlighted()
        #expect(model.mode == .commands)
        #expect(model.text == ":go albums")
    }

    @Test func returnInEmptyHistoryDoesNothing() async {
        defer { clear() }
        let model = model()
        model.open(.history)
        #expect(model.dropdown == .empty)
        await model.submitText()
        #expect(model.mode == .history)
        #expect(model.text == "")
        #expect(calls.submitted.isEmpty)
    }

    @Test func tabAndReturnIgnoredWhileRunning() async {
        defer { clear() }
        let gate = SubmitGate()
        let model = model(gate: gate)
        model.open()
        model.text = ":rep"
        let run = Task { await model.submitText() }
        await gate.waitUntilHeld()
        #expect(model.isRunning)
        model.acceptHighlighted()
        await model.submitText()
        #expect(model.text == ":rep")
        #expect(calls.submitted == [":rep"])
        gate.resume()
        await run.value
        #expect(!model.isOpen)
    }

    @Test func staleFailureReportsWithoutTouchingNewSession() async {
        defer { clear() }
        let gate = SubmitGate()
        let model = model(failure: "Seek failed.", gate: gate)
        model.open()
        model.text = ":seek +1m"
        let run = Task { await model.submitText() }
        await gate.waitUntilHeld()
        model.close()
        model.open()
        #expect(!model.isRunning)
        gate.resume()
        await run.value
        #expect(calls.reported == ["Seek failed."])
        #expect(model.isOpen)
        #expect(model.error == nil)
        #expect(!model.isRunning)
        #expect(model.text == ":")
        #expect(model.history.entries.isEmpty)
    }

    @Test func staleSuccessRecordsWithoutTouchingNewSession() async {
        defer { clear() }
        let gate = SubmitGate()
        let model = model(gate: gate)
        model.open()
        model.text = ":seek +1m"
        let run = Task { await model.submitText() }
        await gate.waitUntilHeld()
        model.close()
        model.open()
        gate.resume()
        await run.value
        #expect(model.history.entries == ["seek +1m"])
        #expect(CommandHistory.load(from: defaults).entries == ["seek +1m"])
        #expect(calls.reported.isEmpty)
        #expect(model.isOpen)
        #expect(model.error == nil)
        #expect(!model.isRunning)
    }

    @Test func closedRunFailureReports() async {
        defer { clear() }
        let gate = SubmitGate()
        let model = model(failure: "Seek failed.", gate: gate)
        model.open()
        model.text = ":seek +1m"
        let run = Task { await model.submitText() }
        await gate.waitUntilHeld()
        model.close()
        gate.resume()
        await run.value
        #expect(!model.isOpen)
        #expect(model.error == nil)
        #expect(calls.reported == ["Seek failed."])
    }

    @Test func defaultSubmitAsksForTheLibraryWindow() async {
        defer { clear() }
        let model = CommandPaletteModel(defaults: defaults)
        model.open()
        model.text = ":go albums"
        await model.submitText()
        #expect(model.error == "Open the library window before submitting commands.")
    }
}
