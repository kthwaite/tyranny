import Foundation
import Testing
@testable import TyrannyCore

@Suite struct HistoryDisplayTests {
    /// UTC+2 and a fixed "now": 2026-10-18 15:00 local.
    private static let calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 2 * 3600)!
        return calendar
    }()
    private static let now = at(2026, 10, 18, 15, 0)

    private static func at(_ year: Int, _ month: Int, _ day: Int, _ hour: Int, _ minute: Int) -> Date {
        calendar.date(from: DateComponents(year: year, month: month, day: day, hour: hour, minute: minute))!
    }

    private func text(_ date: Date) -> String {
        DisplayFormat.playedAt(Int64(date.timeIntervalSince1970), now: Self.now, calendar: Self.calendar)
    }

    @Test func todayShowsTheTime() {
        #expect(text(Self.at(2026, 10, 18, 14, 32)) == "Today, 14:32")
        #expect(text(Self.at(2026, 10, 18, 0, 5)) == "Today, 00:05")
    }

    @Test func yesterdayShowsTheTime() {
        #expect(text(Self.at(2026, 10, 17, 9, 10)) == "Yesterday, 09:10")
        #expect(text(Self.at(2026, 10, 17, 23, 59)) == "Yesterday, 23:59")
    }

    @Test func olderPlaysShowDateAndTime() {
        #expect(text(Self.at(2026, 10, 16, 23, 59)) == "16 Oct 2026, 23:59")
        #expect(text(Self.at(2025, 1, 3, 7, 4)) == "3 Jan 2025, 07:04")
    }

    @Test func dayBoundariesFollowTheCalendarTimeZone() {
        #expect(text(Self.at(2026, 10, 17, 22, 30)) == "Yesterday, 22:30")
        // 00:30 on the 18th at UTC+2 is 22:30 on the 17th in UTC.
        var other = Calendar(identifier: .gregorian)
        other.timeZone = TimeZone(identifier: "UTC")!
        let seconds = Int64(Self.at(2026, 10, 18, 0, 30).timeIntervalSince1970)
        #expect(DisplayFormat.playedAt(seconds, now: Self.now, calendar: other) == "Yesterday, 22:30")
        #expect(DisplayFormat.playedAt(seconds, now: Self.now, calendar: Self.calendar) == "Today, 00:30")
    }

    @Test func aPlayAfterNowOnAnotherDayShowsTheDate() {
        #expect(text(Self.at(2026, 10, 19, 1, 0)) == "19 Oct 2026, 01:00")
    }
}

@Suite struct HistoryPlaybackTests {
    @Test func oneTrackByIDFromSongsNamedHistory() {
        #expect(HistoryPlayback.selection(trackID: 42, libraryRevision: 9) == .ids(libraryRevision: 9, trackIDs: [42]))
        #expect(HistoryPlayback.source == QueueSource(kind: .songs, name: "History"))
    }
}
