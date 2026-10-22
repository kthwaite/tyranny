import Foundation

public struct LyricsPayload: Decodable, Sendable, Equatable {
    public enum Status: String, Decodable, Sendable { case ready, missing, unavailable, instrumental }
    public enum Source: String, Decodable, Sendable { case embedded, lrclib }
    public let status: Status
    public let source: Source?
    public let plain_text: String?
    public let synced_lrc: String?
    public let attribution_url: String?
    public let message: String?

    /// Only provider links are actionable; arbitrary embedded text never becomes a URL.
    public var attributionURL: URL? {
        guard source == .lrclib, let attribution_url, let url = URL(string: attribution_url),
              url.scheme == "https", url.host == "lrclib.net", url.user == nil, url.password == nil else { return nil }
        return url
    }
}

public struct LyricsResponse: Decodable, Sendable {
    public let lyrics: LyricsPayload
    private enum CodingKeys: String, CodingKey { case kind, lyrics }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        guard try values.decode(String.self, forKey: .kind) == "lyrics" else {
            throw DecodingError.dataCorruptedError(forKey: .kind, in: values, debugDescription: "Expected lyrics reply")
        }
        lyrics = try values.decode(LyricsPayload.self, forKey: .lyrics)
    }
}

/// A parsed LRC document, sorted once. Playback reads server position directly: no wall-clock
/// extrapolation can advance a paused lyric or retain a highlight after a backwards seek.
public struct LyricsTimeline: Sendable, Equatable {
    public struct Line: Identifiable, Sendable, Equatable {
        public let id: Int
        public let seconds: Double
        public let text: String
    }
    public let lines: [Line]

    public init(_ lrc: String) {
        var parsed: [(seconds: Double, text: String, order: Int)] = []
        var offset = 0.0
        for raw in lrc.components(separatedBy: .newlines) {
            var remainder = raw.trimmingCharacters(in: .whitespacesAndNewlines)[...]
            var times: [Double] = []
            while remainder.first == "[", let end = remainder.firstIndex(of: "]") {
                let tag = remainder[remainder.index(after: remainder.startIndex)..<end]
                if let time = Self.timestamp(tag) {
                    times.append(time)
                } else if tag.lowercased().hasPrefix("offset:"),
                          let milliseconds = Double(tag.dropFirst(7).trimmingCharacters(in: .whitespaces)),
                          milliseconds.isFinite {
                    offset = milliseconds / 1000
                }
                remainder = remainder[remainder.index(after: end)...]
            }
            let text = remainder.trimmingCharacters(in: .whitespaces)
            for time in times { parsed.append((time, text, parsed.count)) }
        }
        // LRC's positive offset advances the lyrics. Apply the final document-wide offset
        // even when its metadata appears after the timed lines; preserve negative start times.
        lines = parsed.map { Line(id: $0.order, seconds: $0.seconds - offset, text: $0.text) }
            .sorted { $0.seconds == $1.seconds ? $0.id < $1.id : $0.seconds < $1.seconds }
    }

    /// All lines at the most recent timestamp, including translations/duets at identical times.
    /// Empty timed lines intentionally clear the previous lyric during an instrumental break.
    public func activeRange(at seconds: Double) -> Range<Int> {
        guard seconds.isFinite, !lines.isEmpty else { return 0..<0 }
        var low = 0
        var high = lines.count
        while low < high {
            let middle = low + (high - low) / 2
            if lines[middle].seconds <= seconds { low = middle + 1 } else { high = middle }
        }
        let end = low
        guard end > 0 else { return 0..<0 }
        let timestamp = lines[end - 1].seconds
        low = 0
        high = end
        while low < high {
            let middle = low + (high - low) / 2
            if lines[middle].seconds < timestamp { low = middle + 1 } else { high = middle }
        }
        return low..<end
    }

    private static func timestamp(_ tag: Substring) -> Double? {
        let parts = tag.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 2 || parts.count == 3 else { return nil }
        func integer(_ value: Substring) -> Double? {
            guard !value.isEmpty, value.utf8.allSatisfy({ $0 >= 48 && $0 <= 57 }),
                  let number = Double(value), number.isFinite else { return nil }
            return number
        }
        let final = parts[parts.count - 1].split(separator: ".", omittingEmptySubsequences: false)
        guard final.count <= 2, let seconds = integer(final[0]), seconds < 60,
              let minutes = integer(parts[parts.count - 2]) else { return nil }
        var fraction = 0.0
        if final.count == 2 {
            guard final[1].count <= 9, let digits = integer(final[1]) else { return nil }
            fraction = digits / pow(10, Double(final[1].count))
        }
        var result = minutes * 60 + seconds + fraction
        if parts.count == 3 {
            guard minutes < 60, let hours = integer(parts[0]) else { return nil }
            result += hours * 3600
        }
        return result.isFinite ? result : nil
    }
}
