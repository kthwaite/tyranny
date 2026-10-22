import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct WrappedDeckTests {
    private static let artist = WrappedArtist(name: "Autechre", plays: 40, listened_ms: 7_200_000, share: 0.2)
    private static let track = RankedTrack(title: "Gantz Graf", artist: "Autechre", plays: 12, listened_ms: 100, track_id: 1)

    private static func report(
        plays: UInt64 = 500,
        discovery: Discovery? = Discovery(name: "Burial", first_played: 1_700_000_000, plays: 9, listened_ms: 5),
        comeback: Comeback? = Comeback(name: "Aphex Twin", plays: 11, gap_days: 400)
    ) -> WrappedReport {
        WrappedReport(
            year: 2026, through: 1_790_000_000, listened_ms: 36_000_000, plays: plays, artists: 80, tracks: 300,
            top_artists: [artist], top_tracks: [track], discovery: discovery,
            peak_time: PeakTime(weekday: 1, band_start_hour: 18),
            most_skipped: SkippedTrack(title: "Intro", artist: "X", started: 6, skips: 5, track_id: nil),
            comeback: comeback, top_station: StationTime(name: "SomaFM", listened_ms: 4_000_000),
            months: [MonthTop(month: 1, listened_ms: 1, top_artist: "A")]
        )
    }

    @Test func fewerThanFiftyPlaysGiveTheNotEnoughSlide() {
        #expect(WrappedDeck.slides(Self.report(plays: 49)) == [.notEnough])
        #expect(WrappedDeck.slides(Self.report(plays: 50)) != [.notEnough])
    }

    @Test func fullReportYieldsTwelveSlidesInOrder() {
        let slides = WrappedDeck.slides(Self.report())
        #expect(slides.count == 12)
        guard case .year(let ms, let plays) = slides[0] else { Issue.record("slide 1"); return }
        #expect(ms == 36_000_000 && plays == 500)
        guard case .summary = slides[11] else { Issue.record("slide 12"); return }
        guard case .topStation = slides[10] else { Issue.record("slide 11"); return }
        guard case .comeback = slides[9] else { Issue.record("slide 10"); return }
        guard case .mostSkipped = slides[8] else { Issue.record("slide 9"); return }
        guard case .months = slides[7] else { Issue.record("slide 8"); return }
        guard case .peakTime = slides[6] else { Issue.record("slide 7"); return }
        guard case .discovery = slides[5] else { Issue.record("slide 6"); return }
        guard case .topTracks = slides[4] else { Issue.record("slide 5"); return }
        guard case .topTrack = slides[3] else { Issue.record("slide 4"); return }
        guard case .topArtists = slides[2] else { Issue.record("slide 3"); return }
        guard case .topArtist = slides[1] else { Issue.record("slide 2"); return }
    }

    @Test func absentPointsAreLeftOut() {
        let slides = WrappedDeck.slides(Self.report(discovery: nil, comeback: nil))
        #expect(slides.count == 10)
        #expect(!slides.contains { if case .discovery = $0 { true } else { false } })
        #expect(!slides.contains { if case .comeback = $0 { true } else { false } })
    }
}

@Suite struct WrappedOfferTests {
    private static let calendar = Calendar.statsCalendar(timeZone: TimeZone(secondsFromGMT: 2 * 3600)!)

    private static func at(_ year: Int, _ month: Int, _ day: Int, _ hour: Int = 15) -> Date {
        calendar.date(from: DateComponents(year: year, month: month, day: day, hour: hour))!
    }

    @Test func callToActionOnlyInDecember() {
        #expect(WrappedOffer.callToActionYear(now: Self.at(2026, 11, 30, 23), calendar: Self.calendar) == nil)
        #expect(WrappedOffer.callToActionYear(now: Self.at(2026, 12, 1, 0), calendar: Self.calendar) == 2026)
        #expect(WrappedOffer.callToActionYear(now: Self.at(2026, 12, 31, 23), calendar: Self.calendar) == 2026)
        #expect(WrappedOffer.callToActionYear(now: Self.at(2027, 1, 1, 0), calendar: Self.calendar) == nil)
    }

    @Test func dismissedBannerStaysDismissedForThatYear() {
        #expect(WrappedOffer.showsBanner(year: 2026, dismissed: []))
        #expect(!WrappedOffer.showsBanner(year: 2026, dismissed: [2026]))
        #expect(WrappedOffer.showsBanner(year: 2027, dismissed: [2026]))
    }
}

@Suite struct WrappedPhraseTests {
    @Test func peakPhraseNamesDayAndBand() {
        #expect(WrappedPhrase.peak(PeakTime(weekday: 1, band_start_hour: 18)) == "Tuesday evenings")
        #expect(WrappedPhrase.peak(PeakTime(weekday: 0, band_start_hour: 0)) == "Monday late nights")
        #expect(WrappedPhrase.peak(PeakTime(weekday: 6, band_start_hour: 21)) == "Sunday late evenings")
    }

    @Test func everyBandHasItsPhrase() {
        let phrases = [0, 3, 6, 9, 12, 15, 18, 21].map { WrappedPhrase.peak(PeakTime(weekday: 2, band_start_hour: UInt8($0))) }
        #expect(phrases == [
            "Wednesday late nights", "Wednesday small hours", "Wednesday early mornings", "Wednesday mornings",
            "Wednesday afternoons", "Wednesday late afternoons", "Wednesday evenings", "Wednesday late evenings",
        ])
    }
}

@Suite struct SlideshowClockTests {
    @Test func clockAutoAdvancesEverySixSeconds() {
        var clock = SlideshowClock(count: 3)
        clock.tick(5.9)
        #expect(clock.index == 0)
        clock.tick(0.2)
        #expect(clock.index == 1)
        #expect(abs(clock.progress - 0.1 / 6) < 1e-9)
    }

    @Test func clockPausesAndStopsAtTheLastSlide() {
        var clock = SlideshowClock(count: 2)
        clock.togglePause()
        #expect(clock.paused)
        clock.tick(10)
        #expect(clock.index == 0 && clock.progress == 0)
        clock.togglePause()
        #expect(!clock.paused)
        clock.tick(7)
        #expect(clock.index == 1)
        clock.tick(100)
        #expect(clock.index == 1 && clock.progress == 1)
        clock.next()
        #expect(clock.index == 1)
    }

    @Test func nextAndPreviousResetProgressAndClampAtTheEnds() {
        var clock = SlideshowClock(count: 3)
        clock.previous()
        #expect(clock.index == 0)
        clock.tick(3)
        clock.next()
        #expect(clock.index == 1 && clock.progress == 0)
        clock.tick(3)
        clock.previous()
        #expect(clock.index == 0 && clock.progress == 0)
    }

    @Test func clockIsIdleWhilePausedOrHoldingTheLastSlide() {
        var clock = SlideshowClock(count: 2)
        #expect(!clock.idle)
        clock.togglePause()
        #expect(clock.idle)
        clock.togglePause()
        clock.tick(7)
        // The last slide is still filling.
        #expect(clock.index == 1 && !clock.idle)
        clock.tick(6)
        #expect(clock.progress == 1 && clock.idle)
        clock.previous()
        #expect(!clock.idle)
    }

    @Test func aLongTickCanCrossSeveralSlides() {
        var clock = SlideshowClock(count: 5)
        clock.tick(13)
        #expect(clock.index == 2)
        #expect(abs(clock.progress - 1.0 / 6) < 1e-9)
    }
}
