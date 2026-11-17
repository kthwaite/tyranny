import Foundation
import Observation

/// Session-only, oldest-first diagnostics. Only sanitized text is retained.
/// The latest error is always retained in full, even when it alone exceeds the byte budget.
@MainActor @Observable public final class DiagnosticsLog {
    public enum Level: String, Sendable {
        case info
        case error
    }

    public struct Entry: Identifiable, Sendable {
        public let id: UInt64
        public let timestamp: Date
        public let level: Level
        public let source: String
        public let message: String
        fileprivate let byteCount: Int

        fileprivate var text: String {
            "\(timestamp.ISO8601Format()) [\(level.rawValue.uppercased())] [\(source)]\n\(message)\n"
        }
    }

    public private(set) var entries: [Entry] = []
    @ObservationIgnored private var nextID: UInt64 = 0
    @ObservationIgnored private var retainedBytes = 0
    private static let byteBudget = 1_048_576

    public init() {}

    public var text: String {
        var result = ""
        result.reserveCapacity(retainedBytes)
        for entry in entries { result += entry.text }
        return result
    }

    public func record(level: Level, source: String, message: String) {
        let timestamp = Date()
        // Sources are labels, not payloads. Reserve the rest of the entry for its message.
        let safeSource = DiagnosticsText.bounded(Self.sanitize(source), maxBytes: 1024)
        let header = "\(timestamp.ISO8601Format()) [\(level.rawValue.uppercased())] [\(safeSource)]\n"
        let sanitized = Self.sanitize(message)
        let safeMessage = level == .error ? sanitized
            : DiagnosticsText.bounded(sanitized, maxBytes: 65_536 - header.utf8.count - 1)
        let byteCount = header.utf8.count + safeMessage.utf8.count + 1
        let entry = Entry(
            id: nextID, timestamp: timestamp, level: level, source: safeSource,
            message: safeMessage, byteCount: byteCount
        )
        nextID += 1
        if entries.count == 500 || retainedBytes + byteCount > Self.byteBudget {
            let protected = level == .info ? entries.last(where: { $0.level == .error }) : nil
            if let protected, protected.byteCount + byteCount > Self.byteBudget { return }
            var count = entries.count
            var bytes = retainedBytes
            entries.removeAll { old in
                guard count >= 500 || bytes + byteCount > Self.byteBudget,
                      old.id != protected?.id else { return false }
                count -= 1
                bytes -= old.byteCount
                return true
            }
            retainedBytes = bytes
        }
        entries.append(entry)
        retainedBytes += byteCount
    }

    public func clear() {
        entries.removeAll()
        retainedBytes = 0
    }

    /// Redacts URL user info, queries/fragments, and common named authentication values.
    /// This is not a reason to log raw protocol requests or arbitrary secret-bearing payloads.
    public nonisolated static func sanitize(_ text: String) -> String {
        DiagnosticsText.sanitize(text)
    }
}

/// Reopens on each call so append, truncation, and rotation require no persistent reader state.
public enum DiagnosticsLogFile {
    /// Reads at most `maxBytes` final bytes. Notices/redaction can increase the returned size.
    /// A partial first line is discarded: its missing prefix might have identified a secret.
    /// Nonpositive limits return empty text. Invalid UTF-8 is replaced and explicitly marked.
    public static func tail(atPath path: String, maxBytes: Int = 262_144) throws -> String {
        guard maxBytes > 0 else { return "" }
        let handle = try FileHandle(forReadingFrom: URL(fileURLWithPath: path))
        defer { try? handle.close() }
        let size = try handle.seekToEnd()
        let start = size > UInt64(maxBytes) ? size - UInt64(maxBytes) : 0
        try handle.seek(toOffset: start)
        var data = Data()
        // Stop at the snapshotted end, even if the daemon appends while we read.
        let count = Int(size - start)
        while data.count < count {
            guard let chunk = try handle.read(upToCount: count - data.count), !chunk.isEmpty else { break }
            data.append(chunk)
        }
        var notices = ""
        if start > 0 {
            notices += "[Earlier log bytes and the first potentially partial line omitted]\n"
            if let newline = data.firstIndex(of: 0x0A) {
                data = Data(data[data.index(after: newline)...])
            } else {
                data.removeAll()
            }
        }
        let decoded: String
        if let valid = String(data: data, encoding: .utf8) {
            decoded = valid
        } else {
            notices += "[Invalid or incomplete UTF-8 replaced]\n"
            decoded = String(decoding: data, as: UTF8.self)
        }
        return notices + DiagnosticsLog.sanitize(decoded)
    }
}

private enum DiagnosticsText {
    private static let truncation = "\n[Entry truncated]"
    private static let url = try! NSRegularExpression(pattern: #"(?i)https?://[^\s<>\"']+"#)
    private static let userInfo = try! NSRegularExpression(pattern: #"(?i)^(https?://)[^/?#]*@"#)
    private static let headers = try! NSRegularExpression(
        pattern: #"(?im)(\b(?:authorization|proxy[-_ ]authorization|cookie|set[-_]cookie)\b[\"']?[ \t]*[:=][ \t]*)(?:\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'|[^\r\n]+)"#
    )
    private static let values = try! NSRegularExpression(
        pattern: #"(?i)(\b(?:password|passwd|pwd|(?:access[-_ ]?|refresh[-_ ]?|id[-_ ]?|auth[-_ ]?)?token|api[-_ ]?key|api[-_ ]?token|client[-_ ]?secret|secret|session[-_ ]?id)\b[\"']?[ \t]*[:=][ \t]*)(?:\[REDACTED\]|\"(?:\\.|[^\"\\])*(?:\"|\\?\z)|'(?:\\.|[^'\\])*(?:'|\\?\z)|[^\s,;&\}\]\"']+)"#
    )
    private static let bearer = try! NSRegularExpression(
        pattern: #"(?i)\b(?:bearer|basic)[ \t]+[A-Za-z0-9._~+/=-]+"#
    )

    static func sanitize(_ text: String) -> String {
        var result = text
        // Work backwards so replacement lengths cannot invalidate the remaining ranges.
        for match in url.matches(in: text, range: NSRange(text.startIndex..., in: text)).reversed() {
            guard let range = Range(match.range, in: result) else { continue }
            var safeURL = String(result[range])
            safeURL = userInfo.stringByReplacingMatches(
                in: safeURL, range: NSRange(safeURL.startIndex..., in: safeURL),
                withTemplate: "$1[REDACTED]@"
            )
            // Redact the entire query/fragment, including custom and percent-encoded secret keys.
            if let delimiter = safeURL.firstIndex(where: { $0 == "?" || $0 == "#" }) {
                safeURL = String(safeURL[...delimiter]) + "[REDACTED]"
            }
            result.replaceSubrange(range, with: safeURL)
        }
        for pattern in [headers, values] {
            result = pattern.stringByReplacingMatches(
                in: result, range: NSRange(result.startIndex..., in: result),
                withTemplate: "$1[REDACTED]"
            )
        }
        return bearer.stringByReplacingMatches(
            in: result, range: NSRange(result.startIndex..., in: result), withTemplate: "[REDACTED]"
        )
    }

    static func bounded(_ text: String, maxBytes: Int) -> String {
        let bytes = text.utf8
        guard bytes.count > maxBytes else { return text }
        let prefixCount = max(0, maxBytes - truncation.utf8.count)
        var end = bytes.index(bytes.startIndex, offsetBy: prefixCount)
        // Retreat to a scalar boundary, not a grapheme boundary (one grapheme can be enormous).
        while end != bytes.startIndex && bytes[end] & 0xC0 == 0x80 {
            end = bytes.index(before: end)
        }
        return String(decoding: bytes[..<end], as: UTF8.self) + truncation
    }
}
