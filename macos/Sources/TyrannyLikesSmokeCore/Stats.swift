import Foundation

// Wire models for the stats replies. Property names are the server's (`src/stats_types.rs`).

/// An artist in a ranking. `artist_id` is set when the name matches exactly one library artist.
public struct RankedArtist: Codable, Sendable, Equatable {
    public let name: String
    public let plays: UInt64
    public let listened_ms: UInt64
    public let artist_id: UInt64?
}

/// A track in a ranking. `track_id` is set when the title and artist match exactly one library track.
public struct RankedTrack: Codable, Sendable, Equatable {
    public let title: String
    public let artist: String
    public let plays: UInt64
    public let listened_ms: UInt64
    public let track_id: UInt64?
}

/// Listened time in one sub-bucket of the period. `start` is UTC seconds.
public struct Bucket: Codable, Sendable, Equatable {
    public let start: Int64
    public let listened_ms: UInt64
}

public struct SkippedTrack: Codable, Sendable, Equatable {
    public let title: String
    public let artist: String
    public let started: UInt64
    public let skips: UInt64
    public let track_id: UInt64?
}

public struct StationTime: Codable, Sendable, Equatable {
    public let name: String
    public let listened_ms: UInt64
}

/// An artist with many plays and none for a long while. `last_played` is UTC seconds.
public struct LapsedLove: Codable, Sendable, Equatable {
    public let name: String
    public let plays: UInt64
    public let last_played: Int64
    public let artist_id: UInt64?
}

/// An artist whose plays sit in a few tracks. `library_tracks` is the artist's library track count.
public struct ShallowDive: Codable, Sendable, Equatable {
    public let name: String
    public let plays: UInt64
    public let top_share: Double
    public let top_tracks: [String]
    public let library_tracks: UInt64?
    public let artist_id: UInt64?

    /// The counted plays an artist needs, as the server's thresholds scale them:
    /// `max(20, 150 × min(S, 5 years) ÷ 5 years)` for a history of S days.
    public static func minimumPlays(historySpanDays: UInt64) -> UInt64 {
        let fullSpan: UInt64 = 1_826
        return max(20, 150 * min(historySpanDays, fullSpan) / fullSpan)
    }
}

/// Everything the Stats page shows for one period.
public struct StatsReport: Codable, Sendable, Equatable {
    /// UTC seconds, `[period_start, period_end)`.
    public let period_start: Int64
    public let period_end: Int64
    public let plays: UInt64
    public let listened_ms: UInt64
    public let untimed_plays: UInt64
    public let artists: UInt64
    public let tracks: UInt64
    public let skip_rate: Double?
    public let top_artists_by_plays: [RankedArtist]
    public let top_artists_by_time: [RankedArtist]
    public let top_tracks_by_plays: [RankedTrack]
    public let top_tracks_by_time: [RankedTrack]
    public let over_time: [Bucket]
    /// 7 rows, Monday first, of 24 local hours.
    public let heatmap: [[UInt64]]
    public let new_artists: [RankedArtist]
    public let most_skipped: [SkippedTrack]
    public let stations: [StationTime]
    public let lapsed_loves: [LapsedLove]
    public let shallow_dives: [ShallowDive]
    public let history_span_days: UInt64
}

public struct StatsResponse: Codable, Sendable, Equatable {
    public let stats_revision: UInt64
    public let report: StatsReport
}

/// An artist in the Wrapped top list. `share` is the artist's part of the year's listened time.
public struct WrappedArtist: Codable, Sendable, Equatable {
    public let name: String
    public let plays: UInt64
    public let listened_ms: UInt64
    public let share: Double
}

/// The artist first heard this year with the most listened time. `first_played` is UTC seconds.
public struct Discovery: Codable, Sendable, Equatable {
    public let name: String
    public let first_played: Int64
    public let plays: UInt64
    public let listened_ms: UInt64
}

/// The busiest weekday (0 = Monday) and the busiest 3-hour band, which starts at 0, 3, ... 21.
public struct PeakTime: Codable, Sendable, Equatable {
    public let weekday: UInt8
    public let band_start_hour: UInt8
}

/// A lapsed love that returned this year after `gap_days` of silence.
public struct Comeback: Codable, Sendable, Equatable {
    public let name: String
    public let plays: UInt64
    public let gap_days: UInt64
}

/// One month of the year. `month` is 1 to 12. Only months with a counted play are listed.
public struct MonthTop: Codable, Sendable, Equatable {
    public let month: UInt8
    public let listened_ms: UInt64
    public let top_artist: String?
}

/// Every candidate data point for one calendar year. `through` is UTC seconds.
public struct WrappedReport: Codable, Sendable, Equatable {
    public let year: Int32
    public let through: Int64
    public let listened_ms: UInt64
    public let plays: UInt64
    public let artists: UInt64
    public let tracks: UInt64
    public let top_artists: [WrappedArtist]
    public let top_tracks: [RankedTrack]
    public let discovery: Discovery?
    public let peak_time: PeakTime?
    public let most_skipped: SkippedTrack?
    public let comeback: Comeback?
    public let top_station: StationTime?
    /// Sparse: only months with a counted play, not zero-filled.
    public let months: [MonthTop]?
}

public struct WrappedResponse: Codable, Sendable, Equatable {
    public let stats_revision: UInt64
    public let report: WrappedReport
}

public struct StatsYearsResponse: Codable, Sendable, Equatable {
    public let years: [Int]
}

// MARK: - Periods

public enum StatsPeriodKind: String, CaseIterable, Sendable {
    case day, week, month, year, all
}

public extension Calendar {
    /// Gregorian with Monday first: the calendar the server's periods use.
    static func statsCalendar(timeZone: TimeZone = .current) -> Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        calendar.firstWeekday = 2
        return calendar
    }
}

/// A period kind and a date inside it. `.all` ignores the anchor.
public struct StatsPeriod: Equatable, Sendable {
    public var kind: StatsPeriodKind
    public var anchor: Date

    public init(kind: StatsPeriodKind, anchor: Date) {
        self.kind = kind
        self.anchor = anchor
    }

    public static func current(_ kind: StatsPeriodKind, now: Date) -> StatsPeriod {
        StatsPeriod(kind: kind, anchor: now)
    }

    private var component: Calendar.Component? {
        switch kind {
        case .day: .day
        case .week: .weekOfYear
        case .month: .month
        case .year: .year
        case .all: nil
        }
    }

    public func stepped(_ delta: Int, calendar: Calendar) -> StatsPeriod {
        guard let component, let date = calendar.date(byAdding: component, value: delta, to: anchor) else {
            return self
        }
        return StatsPeriod(kind: kind, anchor: date)
    }

    /// False when the next period starts after `now`, and always for `.all`.
    public func canStepForward(now: Date, calendar: Calendar) -> Bool {
        guard let component,
              let next = calendar.dateInterval(of: component, for: stepped(1, calendar: calendar).anchor)
        else { return false }
        return next.start <= now
    }

    /// The period to show when the page returns at `now`. A period that was current at
    /// `shownAt` follows now, so a page left on this week shows the new week. Any other period
    /// stays where the user left it.
    public func refreshed(shownAt: Date, now: Date, calendar: Calendar) -> StatsPeriod {
        guard !canStepForward(now: shownAt, calendar: calendar),
              canStepForward(now: now, calendar: calendar) else { return self }
        return .current(kind, now: now)
    }

    /// The local date, `yyyy-MM-dd`: the server's period anchor.
    public func anchorString(calendar: Calendar) -> String {
        let parts = calendar.dateComponents([.year, .month, .day], from: anchor)
        return String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0)
    }

    public func label(calendar: Calendar) -> String {
        switch kind {
        case .all: return "All time"
        case .year: return String(calendar.component(.year, from: anchor))
        case .month:
            return "\(Self.months[calendar.component(.month, from: anchor) - 1]) \(calendar.component(.year, from: anchor))"
        case .day:
            let weekday = Self.weekdays[calendar.component(.weekday, from: anchor) - 1]
            return "\(weekday) \(Self.dayMonthYear(anchor, calendar: calendar))"
        case .week:
            guard let week = calendar.dateInterval(of: .weekOfYear, for: anchor),
                  let last = calendar.date(byAdding: .day, value: 6, to: week.start)
            else { return Self.dayMonthYear(anchor, calendar: calendar) }
            let first = calendar.dateComponents([.year, .month, .day], from: week.start)
            let end = calendar.dateComponents([.year, .month, .day], from: last)
            if first.year == end.year && first.month == end.month {
                return "\(first.day ?? 0)\u{2013}\(Self.dayMonthYear(last, calendar: calendar))"
            }
            let start = first.year == end.year
                ? "\(first.day ?? 0) \(Self.months[(first.month ?? 1) - 1].prefix(3))"
                : Self.dayMonthYear(week.start, calendar: calendar)
            return "\(start) \u{2013} \(Self.dayMonthYear(last, calendar: calendar))"
        }
    }

    /// The period one level down that holds `bucketStart`; nil for a day.
    public func drillDown(bucketStart: Date, calendar: Calendar) -> StatsPeriod? {
        switch kind {
        case .all: StatsPeriod(kind: .year, anchor: bucketStart)
        case .year: StatsPeriod(kind: .month, anchor: bucketStart)
        case .month, .week: StatsPeriod(kind: .day, anchor: bucketStart)
        case .day: nil
        }
    }

    // English names are fixed, so labels do not follow the system locale.
    private static let months = [
        "January", "February", "March", "April", "May", "June",
        "July", "August", "September", "October", "November", "December",
    ]
    private static let weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]

    private static func dayMonthYear(_ date: Date, calendar: Calendar) -> String {
        let parts = calendar.dateComponents([.year, .month, .day], from: date)
        let month = months[(parts.month ?? 1) - 1].prefix(3)
        return "\(parts.day ?? 0) \(month) \(parts.year ?? 0)"
    }
}
