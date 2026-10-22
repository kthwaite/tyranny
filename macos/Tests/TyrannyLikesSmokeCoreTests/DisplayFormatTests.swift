import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

private func scan(_ phase: String, discovered: Int = 0, processed: Int = 0, total: Int? = nil) -> ScanStatus {
    ScanStatus(root_id: 1, phase: phase, discovered: discovered, processed: processed, total: total,
               updated: 0, unchanged: 0, failures: 0, skipped: 0, last_error: nil)
}

@Suite struct DisplayFormatTests {
    @Test func clockFormatsMinutesAndHours() {
        #expect(DisplayFormat.clock(0) == "0:00")
        #expect(DisplayFormat.clock(83.9) == "1:23")
        #expect(DisplayFormat.clock(3_723) == "1:02:03")
        #expect(DisplayFormat.clock(-1) == "—")
        #expect(DisplayFormat.clock(.nan) == "—")
    }

    @Test func remainingUsesMinusSignAndClampsAtZero() {
        #expect(DisplayFormat.remaining(elapsed: 30, duration: 184) == "\u{2212}2:34")
        #expect(DisplayFormat.remaining(elapsed: 200, duration: 184) == "\u{2212}0:00")
        #expect(DisplayFormat.remaining(elapsed: 30, duration: nil) == "\u{2212}—")
    }

    @Test func gainLabelShowsModeAndSignedValue() {
        #expect(DisplayFormat.gainLabel(mode: "album", gainDB: -6.2) == "RG album \u{2212}6.2 dB")
        #expect(DisplayFormat.gainLabel(mode: "track", gainDB: 1.26) == "RG track +1.3 dB")
        #expect(DisplayFormat.gainLabel(mode: "track", gainDB: 0) == "RG track 0.0 dB")
        #expect(DisplayFormat.gainLabel(mode: "off", gainDB: nil) == nil)
        #expect(DisplayFormat.gainLabel(mode: "album", gainDB: nil) == nil)
    }

    @Test func formatLineJoinsFormatAndGain() {
        #expect(DisplayFormat.formatLine(format: "FLAC 16/44.1", mode: "album", gainDB: -6.2)
                == "FLAC 16/44.1 · RG album \u{2212}6.2 dB")
        #expect(DisplayFormat.formatLine(format: "FLAC 16/44.1", mode: "off", gainDB: nil) == "FLAC 16/44.1")
        #expect(DisplayFormat.formatLine(format: "", mode: "track", gainDB: -1) == "RG track \u{2212}1.0 dB")
    }

    @Test func countGroupsThousands() {
        #expect(DisplayFormat.count(0) == "0")
        #expect(DisplayFormat.count(999) == "999")
        #expect(DisplayFormat.count(1_129) == "1,129")
        #expect(DisplayFormat.count(50_000_000) == "50,000,000")
    }

    @Test func durationPicksDaysHoursOrMinutes() {
        #expect(DisplayFormat.duration(ms: 276_480_000) == "3.2 days")
        #expect(DisplayFormat.duration(ms: 9_360_000) == "2.6 hours")
        #expect(DisplayFormat.duration(ms: 2_520_000) == "42 minutes")
        #expect(DisplayFormat.duration(ms: 60_000) == "1 minute")
        #expect(DisplayFormat.duration(ms: 0) == "0 minutes")
    }

    @Test func bytesUseDecimalUnits() {
        #expect(DisplayFormat.bytes(41_700_000_000) == "41.7 GB")
        #expect(DisplayFormat.bytes(2_500_000) == "2.5 MB")
        #expect(DisplayFormat.bytes(1_200_000_000_000) == "1.2 TB")
        #expect(DisplayFormat.bytes(512) == "512 bytes")
    }

    @Test func totalsForWholeAndFilteredLists() {
        let whole = ListTotals(tracks: 1_129, duration_ms: 276_480_000, size_bytes: 41_700_000_000)
        #expect(DisplayFormat.totals(whole) == "1,129 songs · 3.2 days · 41.7 GB")
        let filtered = ListTotals(tracks: 22, duration_ms: 9_360_000, size_bytes: 900_000_000)
        #expect(DisplayFormat.totals(filtered, of: 1_129) == "22 of 1,129 songs · 2.6 hours")
        #expect(DisplayFormat.totals(ListTotals(tracks: 1, duration_ms: 60_000, size_bytes: 1_000)) == "1 song · 1 minute · 1.0 KB")
    }

    @Test func scanLabelOnlyWhileActive() {
        #expect(DisplayFormat.scanLabel(scan("indexing", processed: 412, total: 1_129), root: "~/Music")
                == "Scanning ~/Music · 412 of 1,129")
        #expect(DisplayFormat.scanLabel(scan("indexing", processed: 412), root: nil) == "Scanning · 412 processed")
        #expect(DisplayFormat.scanLabel(scan("discovering", discovered: 80), root: "~/Music")
                == "Scanning ~/Music · 80 found")
        #expect(DisplayFormat.scanLabel(scan("complete", processed: 5, total: 5), root: "~/Music") == nil)
        #expect(DisplayFormat.scanLabel(scan("idle"), root: nil) == nil)
    }

    @Test func abbreviatedPathReplacesHome() {
        #expect(DisplayFormat.abbreviatedPath("/Users/kit/Music", home: "/Users/kit") == "~/Music")
        #expect(DisplayFormat.abbreviatedPath("/Users/kit", home: "/Users/kit") == "~")
        #expect(DisplayFormat.abbreviatedPath("/Users/kitty/Music", home: "/Users/kit") == "/Users/kitty/Music")
        #expect(DisplayFormat.abbreviatedPath("/Volumes/Music", home: "/Users/kit") == "/Volumes/Music")
    }

    @Test func repeatCyclesOffAllOne() {
        #expect(RepeatMode.off.cycled == .all)
        #expect(RepeatMode.all.cycled == .one)
        #expect(RepeatMode.one.cycled == .off)
    }
}
