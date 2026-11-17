import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct DiagnosticsLogTests {
    @Test func redactsCredentialsWithoutLosingSQLiteContext() {
        let sql = """
            no such function: search_matches
            SELECT id FROM tracks
            WHERE search_matches(title, ?1)
            /Users/listener/Music/Unreleased [2026]/a.flac
            """
        let input = """
            \(sql)
            HTTPS://listener:pa%3Ass@radio.example/live?%74oken=url-secret&format=mp3#fragment-secret
            https://other.example/live#access_token=fragment-token
            Authorization: Bearer header-secret
            proxy-authorization = Basic cHJveHktc2VjcmV0
            {"password": "quoted secret with spaces", "accessToken": "access-secret"}
            refresh_token='refresh secret' api-key=api-secret client_secret: client-secret
            Cookie: session=private-cookie; other=also-private
            Set-Cookie: session=another-cookie; HttpOnly
            """
        let sanitized = DiagnosticsLog.sanitize(input)
        #expect(sanitized.hasPrefix(sql))
        #expect(sanitized.contains("HTTPS://[REDACTED]@radio.example/live?[REDACTED]"))
        #expect(sanitized.contains("https://other.example/live#[REDACTED]"))
        for secret in [
            "listener:pa", "url-secret", "fragment-secret", "fragment-token", "header-secret",
            "cHJveHktc2VjcmV0", "quoted secret with spaces", "access-secret", "refresh secret",
            "api-secret", "client-secret", "private-cookie", "also-private", "another-cookie",
        ] {
            #expect(!sanitized.contains(secret), "Leaked \(secret)")
        }
    }

    @Test func redactsEscapedQuotedValuesAndStandaloneBearer() {
        let text = #"password="first\"still-secret" token='one\'more-secret' Bearer eyJ.private.signature"#
        let sanitized = DiagnosticsLog.sanitize(text)
        #expect(!sanitized.contains("still-secret"))
        #expect(!sanitized.contains("more-secret"))
        #expect(!sanitized.contains("eyJ.private.signature"))
        #expect(DiagnosticsLog.sanitize(sanitized) == sanitized)
    }

    @Test func redactsUnterminatedQuotedCredentials() {
        for input in [
            "password=\"unfinished-secret",
            "token='unfinished-secret\\",
            "\"apiKey\": \"unfinished-secret\ncontinued-secret",
        ] {
            let sanitized = DiagnosticsLog.sanitize(input)
            #expect(sanitized.contains("[REDACTED]"))
            #expect(!sanitized.contains("unfinished-secret"))
            #expect(!sanitized.contains("continued-secret"))
        }
    }

    @MainActor @Test func retainsOnlySanitizedEntriesAndExportsMultilineContext() throws {
        let log = DiagnosticsLog()
        let sql = "no such function: search_matches\nSELECT id\nFROM tracks;"
        log.record(level: .error, source: "transport token=source-secret", message: sql + "\npassword=message-secret")
        let entry = try #require(log.entries.first)
        #expect(!entry.source.contains("source-secret"))
        #expect(!entry.message.contains("message-secret"))
        #expect(log.text.contains(sql))
        #expect(log.text.contains("[ERROR] [transport token=[REDACTED]]"))
        let timestamp = String(log.text.prefix(20))
        #expect(ISO8601DateFormatter().date(from: timestamp) != nil)
        #expect(log.text.hasSuffix("\n"))
    }

    @MainActor @Test func truncatesAtUTF8ScalarBoundariesIncludingLargeGraphemes() throws {
        let log = DiagnosticsLog()
        let message = "Unicode 🎧\n" + String(repeating: "é🎵a\u{301}", count: 15_000)
        log.record(level: .info, source: String(repeating: "源", count: 1000), message: message)
        let entry = try #require(log.entries.first)
        #expect(entry.source.utf8.count <= 1024)
        #expect(entry.source.hasSuffix("[Entry truncated]"))
        #expect(entry.message.hasPrefix("Unicode 🎧\n"))
        #expect(entry.message.hasSuffix("[Entry truncated]"))
        #expect(!log.text.contains("\u{FFFD}"))
        #expect(log.text.utf8.count <= 65_536)
        #expect(log.text.utf8.count > 65_530)

        // Grapheme-based truncation must not drop an entire otherwise useful message.
        log.clear()
        log.record(level: .info, source: "scan", message: "a" + String(repeating: "\u{301}", count: 40_000))
        #expect(log.text.utf8.count <= 65_536)
        #expect(log.text.utf8.count > 65_530)
        #expect(!log.text.contains("\u{FFFD}"))
    }

    @MainActor @Test func sanitizesBeforeTruncatingCredentials() throws {
        let log = DiagnosticsLog()
        log.record(
            level: .info, source: "transport",
            message: "password=\"" + String(repeating: "private", count: 20_000) + "\"\nSQL context survives"
        )
        let entry = try #require(log.entries.first)
        #expect(!entry.message.contains("private"))
        #expect(entry.message.contains("SQL context survives"))
        #expect(!entry.message.contains("Entry truncated"))
    }

    @MainActor @Test func evictsOldestEntriesAtCountLimitAndKeepsIDsAcrossClear() throws {
        let log = DiagnosticsLog()
        for index in 0..<510 {
            log.record(level: .info, source: "queue", message: "event \(index)")
        }
        #expect(log.entries.count == 500)
        #expect(log.entries.map(\.message) == (10..<510).map { "event \($0)" })
        #expect(Set(log.entries.map(\.id)).count == 500)
        let lastID = try #require(log.entries.last?.id)
        log.clear()
        #expect(log.entries.isEmpty)
        #expect(log.text.isEmpty)
        log.record(level: .info, source: "queue", message: "after clear")
        #expect(try #require(log.entries.first).id > lastID)
    }

    @MainActor @Test func byteLimitEvictsWholeEventsInOrder() {
        let log = DiagnosticsLog()
        let payload = String(repeating: "x", count: 70_000)
        for index in 0..<20 {
            log.record(level: .error, source: "decoder", message: "event \(index)\n" + payload)
        }
        #expect(log.entries.count == 14)
        #expect(log.text.utf8.count <= 1_048_576)
        #expect(log.entries.enumerated().allSatisfy { offset, entry in
            entry.message == "event \(offset + 6)\n" + payload
        })
        log.clear()
        for _ in 0..<16 { log.record(level: .info, source: "scan", message: payload) }
        #expect(log.entries.count == 16)
        #expect(log.text.utf8.count == 1_048_576)
    }

    @MainActor @Test func oversizedErrorKeepsItsFinalLineThroughLaterStatusUpdates() {
        let log = DiagnosticsLog()
        let detail = "filtered playback failed\n" + String(repeating: "長", count: 360_000) + "\nfinal SQL detail"
        log.record(level: .info, source: "App", message: "before failure")
        log.record(level: .error, source: "Command", message: detail)
        log.record(level: .info, source: "App message", message: "Connected")
        #expect(log.entries.map(\.message) == [detail])
        #expect(log.text.hasSuffix(detail + "\n"))
        log.record(level: .error, source: "Command", message: "another failure")
        #expect(log.entries.map(\.message) == ["another failure"])
    }

    @Test func tailReopensAfterAppendTruncationAndRotation() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("daemon.log")
        try Data("initial\n".utf8).write(to: file)
        #expect(try DiagnosticsLogFile.tail(atPath: file.path) == "initial\n")
        let writer = try FileHandle(forWritingTo: file)
        defer { try? writer.close() }
        try writer.seekToEnd()
        try writer.write(contentsOf: Data("password=append-secret\nappended\n".utf8))
        let appended = try DiagnosticsLogFile.tail(atPath: file.path)
        #expect(appended.hasPrefix("initial\n"))
        #expect(appended.hasSuffix("appended\n"))
        #expect(!appended.contains("append-secret"))
        try writer.truncate(atOffset: 0)
        try writer.seek(toOffset: 0)
        try writer.write(contentsOf: Data("after truncate\n".utf8))
        #expect(try DiagnosticsLogFile.tail(atPath: file.path) == "after truncate\n")
        try FileManager.default.moveItem(at: file, to: directory.appendingPathComponent("daemon.log.1"))
        try Data("after rotation\n".utf8).write(to: file)
        #expect(try DiagnosticsLogFile.tail(atPath: file.path) == "after rotation\n")
    }

    @Test func tailDropsPartialLineAndSplitUnicodeRatherThanLeakingUnlabelledSecrets() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: file) }
        let suffix = "\nretained SQL: search_matches\n🎵\n"
        let contents = "password=" + String(repeating: "private", count: 1000) + "🎧" + suffix
        try Data(contents.utf8).write(to: file)
        // The read starts inside the four-byte headphone scalar, after the password label.
        let tail = try DiagnosticsLogFile.tail(atPath: file.path, maxBytes: suffix.utf8.count + 2)
        #expect(tail.contains("partial line omitted"))
        #expect(tail.hasSuffix("retained SQL: search_matches\n🎵\n"))
        #expect(!tail.contains("private"))
        #expect(!tail.contains("\u{FFFD}"))
        #expect(tail.utf8.count < 200)

        try Data(String(repeating: "private", count: 1000).utf8).write(to: file)
        let noCompleteLine = try DiagnosticsLogFile.tail(atPath: file.path, maxBytes: 32)
        #expect(noCompleteLine.contains("omitted"))
        #expect(!noCompleteLine.contains("private"))
    }

    @Test func tailMarksInvalidUTF8AndHandlesEmptyFilesAndZeroLimit() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: file) }
        try Data([0x61, 0xF0, 0x9F]).write(to: file)
        let invalid = try DiagnosticsLogFile.tail(atPath: file.path)
        #expect(invalid.contains("Invalid or incomplete UTF-8 replaced"))
        #expect(invalid.hasSuffix("a\u{FFFD}"))
        #expect(try DiagnosticsLogFile.tail(atPath: file.path, maxBytes: 0).isEmpty)
        #expect(try DiagnosticsLogFile.tail(atPath: file.path, maxBytes: -1).isEmpty)
        try Data().write(to: file)
        #expect(try DiagnosticsLogFile.tail(atPath: file.path).isEmpty)
        try FileManager.default.removeItem(at: file)
        #expect(throws: (any Error).self) { try DiagnosticsLogFile.tail(atPath: file.path) }
    }
}
