import Foundation
import Testing
@testable import TyrannyCore

@Suite struct StatsPeriodTests {
    /// UTC+2, like `HistoryDisplayTests`. Week starts on Monday.
    private static let calendar = Calendar.statsCalendar(timeZone: TimeZone(secondsFromGMT: 2 * 3600)!)

    private static func at(_ year: Int, _ month: Int, _ day: Int, _ hour: Int = 15) -> Date {
        calendar.date(from: DateComponents(year: year, month: month, day: day, hour: hour))!
    }

    private func period(_ kind: StatsPeriodKind, _ year: Int, _ month: Int, _ day: Int) -> StatsPeriod {
        StatsPeriod.current(kind, now: Self.at(year, month, day))
    }

    private func label(_ kind: StatsPeriodKind, _ year: Int, _ month: Int, _ day: Int) -> String {
        period(kind, year, month, day).label(calendar: Self.calendar)
    }

    @Test func statsCalendarIsGregorianWithMondayFirst() {
        #expect(Self.calendar.identifier == .gregorian)
        #expect(Self.calendar.firstWeekday == 2)
    }

    @Test func dayMonthYearAndAllLabels() {
        #expect(label(.day, 2026, 10, 21) == "Wed 21 Oct 2026")
        #expect(label(.month, 2026, 10, 21) == "October 2026")
        #expect(label(.year, 2026, 10, 21) == "2026")
        #expect(label(.all, 2026, 10, 21) == "All time")
    }

    @Test func weekLabelsWithinOneMonth() {
        #expect(label(.week, 2026, 10, 21) == "19\u{2013}25 Oct 2026")
    }

    @Test func weekLabelsAcrossMonthsAndYears() {
        #expect(label(.week, 2026, 9, 30) == "28 Sep \u{2013} 4 Oct 2026")
        #expect(label(.week, 2026, 1, 1) == "29 Dec 2025 \u{2013} 4 Jan 2026")
    }

    @Test func anchorStringIsTheLocalDate() {
        // 00:30 local on the 21st is still the 20th in UTC.
        let early = StatsPeriod(kind: .day, anchor: Self.at(2026, 10, 21, 0).addingTimeInterval(1800))
        #expect(early.anchorString(calendar: Self.calendar) == "2026-10-21")
        #expect(period(.all, 2026, 1, 5).anchorString(calendar: Self.calendar) == "2026-01-05")
    }

    @Test func steppingMovesByOnePeriod() {
        let cal = Self.calendar
        #expect(period(.month, 2026, 1, 31).stepped(1, calendar: cal).label(calendar: cal) == "February 2026")
        #expect(period(.year, 2026, 6, 1).stepped(-1, calendar: cal).label(calendar: cal) == "2025")
        #expect(period(.day, 2026, 10, 21).stepped(1, calendar: cal).label(calendar: cal) == "Thu 22 Oct 2026")
        #expect(period(.week, 2026, 10, 21).stepped(-1, calendar: cal).label(calendar: cal) == "12\u{2013}18 Oct 2026")
        let all = period(.all, 2026, 10, 21)
        #expect(all.stepped(1, calendar: cal) == all)
        #expect(all.stepped(-1, calendar: cal) == all)
    }

    @Test func cannotStepPastTheCurrentPeriod() {
        let cal = Self.calendar
        let now = Self.at(2026, 10, 21)
        for kind in [StatsPeriodKind.day, .week, .month, .year] {
            let current = StatsPeriod.current(kind, now: now)
            #expect(!current.canStepForward(now: now, calendar: cal), "\(kind) current")
            #expect(current.stepped(-1, calendar: cal).canStepForward(now: now, calendar: cal), "\(kind) previous")
        }
        #expect(!StatsPeriod.current(.all, now: now).canStepForward(now: now, calendar: cal))
    }

    @Test func drillDownNarrowsByOneLevel() {
        let cal = Self.calendar
        let bucket = Self.at(2026, 3, 9, 0)
        let fromAll = period(.all, 2026, 10, 21).drillDown(bucketStart: bucket, calendar: cal)
        #expect(fromAll?.kind == .year && fromAll?.label(calendar: cal) == "2026")
        let fromYear = period(.year, 2026, 10, 21).drillDown(bucketStart: bucket, calendar: cal)
        #expect(fromYear?.kind == .month && fromYear?.label(calendar: cal) == "March 2026")
        let fromMonth = period(.month, 2026, 3, 21).drillDown(bucketStart: bucket, calendar: cal)
        #expect(fromMonth?.kind == .day && fromMonth?.label(calendar: cal) == "Mon 9 Mar 2026")
        let fromWeek = period(.week, 2026, 3, 11).drillDown(bucketStart: bucket, calendar: cal)
        #expect(fromWeek?.kind == .day && fromWeek?.anchorString(calendar: cal) == "2026-03-09")
        #expect(period(.day, 2026, 3, 9).drillDown(bucketStart: bucket, calendar: cal) == nil)
    }
}

@Suite struct StatsWireTests {
    /// Serialised by the Rust `Response` type: the Rust side round-trips this exact text.
    private static let statsReply = #"""
    {"kind":"stats","stats_revision":3,"report":{"period_start":1792188000,"period_end":1792792800,"plays":12,"listened_ms":2400000,"untimed_plays":1,"artists":3,"tracks":9,"skip_rate":null,"top_artists_by_plays":[{"name":"Miles Davis","plays":5,"listened_ms":900000,"artist_id":7}],"top_artists_by_time":[{"name":"Unknown","plays":5,"listened_ms":900000,"artist_id":null}],"top_tracks_by_plays":[{"title":"So What","artist":"Miles Davis","plays":4,"listened_ms":800000,"track_id":11}],"top_tracks_by_time":[{"title":"Loose","artist":"","plays":4,"listened_ms":800000,"track_id":null}],"over_time":[{"start":1792188000,"listened_ms":600000}],"heatmap":[[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,3600000,0,0],[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],[0,0,0,0,0,0,0,0,0,120000,0,0,0,0,0,0,0,0,0,0,0,0,0,0]],"new_artists":[{"name":"Nina","plays":5,"listened_ms":900000,"artist_id":null}],"most_skipped":[{"title":"Skip","artist":"Them","started":6,"skips":4,"track_id":null}],"stations":[{"name":"Jazz","listened_ms":300000}],"lapsed_loves":[{"name":"Old","plays":40,"last_played":1700000000,"artist_id":2}],"shallow_dives":[{"name":"Thin","plays":10,"top_share":0.8,"top_tracks":["A","B"],"library_tracks":null,"artist_id":null}],"history_span_days":200}}
    """#

    private static let wrappedReply = #"""
    {"kind":"wrapped","stats_revision":3,"report":{"year":2025,"through":1767225599,"listened_ms":9000000,"plays":50,"artists":8,"tracks":30,"top_artists":[{"name":"Miles Davis","plays":20,"listened_ms":4000000,"share":0.44}],"top_tracks":[{"title":"So What","artist":"Miles Davis","plays":4,"listened_ms":800000,"track_id":11}],"discovery":{"name":"Nina","first_played":1750000000,"plays":6,"listened_ms":700000},"peak_time":{"weekday":2,"band_start_hour":21},"most_skipped":{"title":"Skip","artist":"Them","started":6,"skips":4,"track_id":null},"comeback":{"name":"Old","plays":9,"gap_days":120},"top_station":{"name":"Jazz","listened_ms":300000},"months":[{"month":3,"listened_ms":100000,"top_artist":"Miles Davis"},{"month":7,"listened_ms":50000,"top_artist":null}]}}
    """#

    @Test func decodesAStatsReply() throws {
        let reply = try JSONDecoder().decode(StatsResponse.self, from: Data(Self.statsReply.utf8))
        #expect(reply.stats_revision == 3)
        let report = reply.report
        #expect(report.plays == 12 && report.untimed_plays == 1)
        #expect(report.skip_rate == nil)
        #expect(report.heatmap.count == 7 && report.heatmap.allSatisfy { $0.count == 24 })
        #expect(report.heatmap[2][21] == 3_600_000)
        #expect(report.top_artists_by_plays == [
            RankedArtist(name: "Miles Davis", plays: 5, listened_ms: 900_000, artist_id: 7)])
        #expect(report.top_artists_by_time.first?.artist_id == nil)
        #expect(report.top_tracks_by_plays.first?.track_id == 11)
        #expect(report.over_time == [Bucket(start: 1_792_188_000, listened_ms: 600_000)])
        #expect(report.most_skipped.first?.skips == 4)
        #expect(report.stations.first?.name == "Jazz")
        #expect(report.lapsed_loves.first?.last_played == 1_700_000_000)
        #expect(report.shallow_dives.first?.top_tracks == ["A", "B"])
        #expect(report.shallow_dives.first?.library_tracks == nil)
        #expect(report.history_span_days == 200)
    }

    @Test func decodesAWrappedReply() throws {
        let reply = try JSONDecoder().decode(WrappedResponse.self, from: Data(Self.wrappedReply.utf8))
        #expect(reply.stats_revision == 3)
        let report = reply.report
        #expect(report.year == 2025 && report.through == 1_767_225_599)
        #expect(report.top_artists.first?.share == 0.44)
        #expect(report.discovery?.first_played == 1_750_000_000)
        #expect(report.peak_time == PeakTime(weekday: 2, band_start_hour: 21))
        #expect(report.most_skipped?.track_id == nil)
        #expect(report.comeback == Comeback(name: "Old", plays: 9, gap_days: 120))
        #expect(report.top_station?.name == "Jazz")
        #expect(report.months == [
            MonthTop(month: 3, listened_ms: 100_000, top_artist: "Miles Davis"),
            MonthTop(month: 7, listened_ms: 50_000, top_artist: nil),
        ])
    }

    @Test func wrappedOptionalsMayBeNullOrAbsent() throws {
        let text = #"""
        {"kind":"wrapped","stats_revision":0,"report":{"year":2024,"through":0,"listened_ms":0,"plays":0,
        "artists":0,"tracks":0,"top_artists":[],"top_tracks":[],"discovery":null,"peak_time":null,
        "most_skipped":null,"comeback":null,"top_station":null}}
        """#
        let report = try JSONDecoder().decode(WrappedResponse.self, from: Data(text.utf8)).report
        #expect(report.discovery == nil && report.months == nil && report.top_station == nil)
    }

    @Test func decodesTheStatsYears() throws {
        let reply = try JSONDecoder().decode(
            StatsYearsResponse.self, from: Data(#"{"kind":"stats_years","years":[2025,2026]}"#.utf8))
        #expect(reply.years == [2025, 2026])
    }

    @Test func periodKindsUseTheWireNames() {
        #expect(StatsPeriodKind.allCases.map(\.rawValue) == ["day", "week", "month", "year", "all"])
    }
}

@Test func controlStateDecodesStatsRevision() throws {
    let text = """
        {"sequence":1,"queue_revision":4,"playlists_revision":1,"stations_revision":2,"queue_length":0,
         "current_id":null,"can_next":false,"can_previous":false,"now_playing":null,
         "position_frames":0,"sample_rate":48000,"transport":"paused",
         "settings":{"gain_mode":"off","resume_on_start":false,"volume":100,"repeat":"off","shuffle":false,
                    "equalizer":{"enabled":false,"preamp_db":0,"bands_db":[0,0,0,0,0,0,0,0,0,0]}},
         "error":null,"underruns":0,
         "library":{"revision":1,"roots":0,"tracks":0,"duration_ms":0,"size_bytes":0,
                    "scan":{"root_id":null,"phase":"idle","discovered":0,"processed":0,"total":null,"updated":0,"unchanged":0,"failures":0,"skipped":0,"last_error":null}},
         "history_revision":5,"stats_revision":9,"current_index":null,"upcoming_count":0}
        """
    let state = try JSONDecoder().decode(ControlState.self, from: Data(text.utf8))
    #expect(state.history_revision == 5 && state.stats_revision == 9)
}
