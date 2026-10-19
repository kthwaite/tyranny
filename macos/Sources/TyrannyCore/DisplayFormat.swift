import Foundation

/// Text for the toolbar display and the status bar. Locale-independent, so the
/// strings match the spec examples exactly.
public enum DisplayFormat {
    static let minus = "\u{2212}"

    /// "m:ss", or "h:mm:ss" from one hour. "—" for an invalid value.
    public static func clock(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0, seconds < Double(Int.max) else { return "—" }
        let value = Int(seconds)
        if value >= 3600 {
            return String(format: "%d:%02d:%02d", value / 3600, (value / 60) % 60, value % 60)
        }
        return String(format: "%d:%02d", value / 60, value % 60)
    }

    /// Time left, with a minus sign. Clamped at zero.
    public static func remaining(elapsed: Double, duration: Double?) -> String {
        guard let duration else { return minus + "—" }
        return minus + clock(max(0, duration - max(0, elapsed)))
    }

    /// "RG album −6.2 dB". Nil when gain is off (the server sends no applied gain).
    public static func gainLabel(mode: String, gainDB: Double?) -> String? {
        guard mode != "off", let gainDB, gainDB.isFinite else { return nil }
        let magnitude = String(format: "%.1f", abs(gainDB))
        let sign = magnitude == "0.0" ? "" : gainDB < 0 ? minus : "+"
        return "RG \(mode) \(sign)\(magnitude) dB"
    }

    /// "FLAC 16/44.1 · RG album −6.2 dB", leaving out empty parts.
    public static func formatLine(format: String, mode: String, gainDB: Double?) -> String {
        [format.isEmpty ? nil : format, gainLabel(mode: mode, gainDB: gainDB)]
            .compactMap { $0 }.joined(separator: " · ")
    }

    /// Thousands grouped with commas.
    public static func count<N: BinaryInteger>(_ value: N) -> String {
        let digits = String(value.magnitude)
        var result = ""
        for (index, digit) in digits.enumerated() {
            if index > 0, (digits.count - index) % 3 == 0 { result.append(",") }
            result.append(digit)
        }
        return (value < 0 ? "-" : "") + result
    }

    /// "3.2 days", "2.6 hours", or "42 minutes".
    public static func duration(ms: UInt64) -> String {
        let seconds = Double(ms) / 1000
        if seconds >= 86_400 { return String(format: "%.1f days", seconds / 86_400) }
        if seconds >= 3_600 { return String(format: "%.1f hours", seconds / 3_600) }
        let minutes = Int(seconds / 60)
        return minutes == 1 ? "1 minute" : "\(minutes) minutes"
    }

    /// Decimal units: "41.7 GB".
    public static func bytes(_ value: UInt64) -> String {
        let units: [(Double, String)] = [(1e12, "TB"), (1e9, "GB"), (1e6, "MB"), (1e3, "KB")]
        let number = Double(value)
        for (scale, unit) in units where number >= scale {
            return String(format: "%.1f %@", number / scale, unit)
        }
        return "\(value) bytes"
    }

    /// "1,129 songs · 3.2 days · 41.7 GB", or "22 of 1,129 songs · 2.6 hours" when
    /// `total` (the unfiltered count) is given.
    public static func totals(_ totals: ListTotals, of total: UInt64? = nil) -> String {
        let noun = (total ?? totals.tracks) == 1 ? "song" : "songs"
        if let total {
            return "\(count(totals.tracks)) of \(count(total)) \(noun) · \(duration(ms: totals.duration_ms))"
        }
        return "\(count(totals.tracks)) \(noun) · \(duration(ms: totals.duration_ms)) · \(bytes(totals.size_bytes))"
    }

    /// "Scanning ~/Music · 412 of 1,129" while a scan is active, else nil.
    public static func scanLabel(_ scan: ScanStatus, root: String?) -> String? {
        let head = root.map { "Scanning \($0)" } ?? "Scanning"
        switch scan.phase {
        case "discovering":
            return "\(head) · \(count(scan.discovered)) found"
        case "indexing":
            if let total = scan.total, total > 0 {
                return "\(head) · \(count(scan.processed)) of \(count(total))"
            }
            return "\(head) · \(count(scan.processed)) processed"
        default:
            return nil
        }
    }

    /// Replaces the home directory prefix with "~".
    public static func abbreviatedPath(_ path: String, home: String) -> String {
        guard !home.isEmpty else { return path }
        if path == home { return "~" }
        let prefix = home.hasSuffix("/") ? home : home + "/"
        return path.hasPrefix(prefix) ? "~/" + path.dropFirst(prefix.count) : path
    }

    /// A column browser's first row: "All (12 Genres)".
    public static func browserAll(_ value: UInt64, singular: String, plural: String) -> String {
        "All (\(count(value)) \(value == 1 ? singular : plural))"
    }

    /// "1 song", "1,129 songs".
    public static func songs<N: BinaryInteger>(_ value: N) -> String {
        "\(count(value)) \(value == 1 ? "song" : "songs")"
    }

    /// "1,411 kbps", or empty when unknown.
    public static func kilobits(_ value: UInt32?) -> String {
        value.map { "\(count($0)) kbps" } ?? ""
    }

    /// "44.1 kHz", "48 kHz", or empty when unknown.
    public static func sampleRate(_ hertz: UInt32) -> String {
        guard hertz > 0 else { return "" }
        let kilohertz = Double(hertz) / 1000
        return kilohertz == kilohertz.rounded()
            ? "\(Int(kilohertz)) kHz" : String(format: "%.1f kHz", kilohertz)
    }
}

extension DisplayFormat {
    /// An album block's facts lines: title; year · genre; tracks · length; format.
    /// Empty lines are left out.
    public static func albumFacts(_ album: AlbumSummary) -> [String] {
        let when = [album.year.map(String.init), album.genre.isEmpty ? nil : album.genre]
            .compactMap { $0 }.joined(separator: " · ")
        let tracks = "\(count(album.track_count)) \(album.track_count == 1 ? "track" : "tracks")"
        let length = "\(tracks) · \(clock(Double(album.duration_ms) / 1000))"
        return [album.title, when, length, album.format].enumerated()
            .filter { $0.offset == 0 || !$0.element.isEmpty }.map(\.element)
    }

    /// An artist header's counts: "1 album · 5 tracks". Without an album count, the tracks only.
    public static func artistHeaderCounts(albums: Int?, tracks: Int) -> String {
        let trackText = "\(count(tracks)) \(tracks == 1 ? "track" : "tracks")"
        guard let albums else { return trackText }
        return "\(count(albums)) \(albums == 1 ? "album" : "albums") · \(trackText)"
    }
}

extension RepeatMode {
    /// The toolbar toggle order: off → all → one → off.
    public var cycled: RepeatMode {
        switch self {
        case .off: .all
        case .all: .one
        case .one: .off
        }
    }
}
