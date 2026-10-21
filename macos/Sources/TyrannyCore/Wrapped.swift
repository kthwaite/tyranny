import Foundation

/// One slide of the Wrapped slideshow, with the data it shows.
public enum WrappedSlide: Equatable, Sendable {
    case year(listenedMs: UInt64, plays: UInt64)
    case topArtist(WrappedArtist)
    case topArtists([WrappedArtist])
    case topTrack(RankedTrack)
    case topTracks([RankedTrack])
    case discovery(Discovery)
    case peakTime(PeakTime)
    case months([MonthTop])
    case mostSkipped(SkippedTrack)
    case comeback(Comeback)
    case topStation(StationTime)
    case summary(artists: [WrappedArtist], tracks: [RankedTrack], listenedMs: UInt64)
    case notEnough
}

public enum WrappedDeck {
    /// A year needs this many counted plays to get a Wrapped.
    static let minimumPlays: UInt64 = 50

    /// The slides for a report, in the spec's order. A slide whose data is absent or empty is left out.
    public static func slides(_ report: WrappedReport) -> [WrappedSlide] {
        guard report.plays >= minimumPlays else { return [.notEnough] }
        var slides: [WrappedSlide] = [.year(listenedMs: report.listened_ms, plays: report.plays)]
        if let artist = report.top_artists.first {
            slides.append(.topArtist(artist))
            slides.append(.topArtists(report.top_artists))
        }
        if let track = report.top_tracks.first {
            slides.append(.topTrack(track))
            slides.append(.topTracks(report.top_tracks))
        }
        if let discovery = report.discovery { slides.append(.discovery(discovery)) }
        if let peak = report.peak_time { slides.append(.peakTime(peak)) }
        if let months = report.months, !months.isEmpty { slides.append(.months(months)) }
        if let skipped = report.most_skipped { slides.append(.mostSkipped(skipped)) }
        if let comeback = report.comeback { slides.append(.comeback(comeback)) }
        if let station = report.top_station { slides.append(.topStation(station)) }
        slides.append(.summary(artists: report.top_artists, tracks: report.top_tracks, listenedMs: report.listened_ms))
        return slides
    }
}

public enum WrappedOffer {
    /// The year to offer a Wrapped for: the calendar's current year on 1 to 31 December, else nil.
    public static func callToActionYear(now: Date, calendar: Calendar) -> Int? {
        let parts = calendar.dateComponents([.year, .month], from: now)
        guard parts.month == 12 else { return nil }
        return parts.year
    }

    /// The banner shows until that year's dismissal is remembered.
    public static func showsBanner(year: Int, dismissed: Set<Int>) -> Bool {
        !dismissed.contains(year)
    }
}

public enum WrappedPhrase {
    private static let days = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]
    private static let bands = [
        "late nights", "small hours", "early mornings", "mornings",
        "afternoons", "late afternoons", "evenings", "late evenings",
    ]

    /// "Tuesday evenings". Weekday 0 is Monday. Bands start at 0, 3, ... 21.
    public static func peak(_ time: PeakTime) -> String {
        let day = days[Int(time.weekday) % days.count]
        let band = bands[Int(time.band_start_hour) / 3 % bands.count]
        return "\(day) \(band)"
    }
}

/// The slideshow's pace. It holds on the last slide.
public struct SlideshowClock: Equatable, Sendable {
    public let count: Int
    public let interval: TimeInterval
    public private(set) var index = 0
    public private(set) var paused = false
    private var elapsed: TimeInterval = 0

    public init(count: Int, interval: TimeInterval = 6) {
        self.count = count
        self.interval = interval
    }

    private var isLast: Bool { index >= count - 1 }

    /// The fraction of the current slide's interval that has passed, 0 to 1.
    public var progress: Double { min(elapsed / interval, 1) }

    public mutating func tick(_ dt: TimeInterval) {
        guard !paused else { return }
        elapsed += dt
        while elapsed >= interval, !isLast {
            elapsed -= interval
            index += 1
        }
        if isLast { elapsed = min(elapsed, interval) }
    }

    public mutating func next() {
        guard !isLast else { return }
        index += 1
        elapsed = 0
    }

    public mutating func previous() {
        guard index > 0 else { return }
        index -= 1
        elapsed = 0
    }

    public mutating func togglePause() { paused.toggle() }
}
