import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct CommandHistoryTests {
    @Test func recordNormalisesAndDeduplicates() {
        var history = CommandHistory()
        history.record(":go albums"); history.record("seek +1m"); history.record(" go albums ")
        #expect(history.entries == ["go albums", "seek +1m"])
    }

    @Test func recordSkipsEmptyAndHistory() {
        var history = CommandHistory()
        history.record("  "); history.record(":"); history.record(":history"); history.record("history")
        #expect(history.entries.isEmpty)
    }

    @Test func recordCapsAtLimit() {
        var history = CommandHistory()
        for index in 0..<105 { history.record("volume \(index)") }
        #expect(history.entries.count == 100)
        #expect(history.entries.first == "volume 104")
        #expect(history.entries.last == "volume 5")
    }

    @Test func roundTripsThroughDefaults() {
        let name = "CommandHistoryTests.\(UUID())"
        let defaults = UserDefaults(suiteName: name)!
        defer { defaults.removePersistentDomain(forName: name) }
        #expect(CommandHistory.load(from: defaults).entries.isEmpty)
        var history = CommandHistory()
        history.record("go albums"); history.record("seek +1m")
        history.save(to: defaults)
        #expect(CommandHistory.load(from: defaults) == history)
    }

    @Test func loadCapsAndDropsEmptyStrings() {
        let name = "CommandHistoryTests.\(UUID())"
        let defaults = UserDefaults(suiteName: name)!
        defer { defaults.removePersistentDomain(forName: name) }
        defaults.set([""] + (0..<105).map { "volume \($0)" }, forKey: CommandHistory.defaultsKey)
        let loaded = CommandHistory.load(from: defaults)
        #expect(loaded.entries.count == 100)
        #expect(loaded.entries.first == "volume 0")
    }
}
