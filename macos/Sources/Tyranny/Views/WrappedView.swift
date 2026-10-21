import SwiftUI
import TyrannyCore

/// The Wrapped slideshow (spec "Client: Wrapped slideshow"). It covers the whole window, and
/// playback continues underneath. It fetches the year's `wrapped` report once and shows
/// `WrappedDeck.slides`, paced by a `SlideshowClock`. → or a click goes forward, ← goes back,
/// space pauses, and esc closes.
struct WrappedView: View {
    let year: Int
    let player: PlayerModel
    let close: () -> Void

    private enum Load {
        case loading
        case loaded([WrappedSlide])
        case failed(String)
    }

    @State private var load = Load.loading
    @State private var clock = SlideshowClock(count: 1)
    @FocusState private var focused: Bool

    var body: some View {
        GeometryReader { geometry in
            let scale = min(max(min(geometry.size.width / 1180, geometry.size.height / 740), 0.75), 1.6)
            ZStack {
                switch load {
                case .loading:
                    WrappedBackdrop(palette: .loading)
                    ProgressView()
                        .controlSize(.large)
                        .tint(.white)
                case .failed(let message):
                    WrappedBackdrop(palette: .loading)
                    SlideView(slide: nil, year: year, scale: scale, failure: message, close: close)
                case .loaded(let slides):
                    let slide = slides[min(clock.index, slides.count - 1)]
                    Group {
                        WrappedBackdrop(palette: slide.palette)
                        SlideView(slide: slide, year: year, scale: scale, close: close)
                    }
                    .id(clock.index)
                    .transition(.opacity)
                }
                chrome(scale: scale)
            }
            .animation(.easeInOut(duration: 0.5), value: clock.index)
        }
        .ignoresSafeArea()
        .contentShape(Rectangle())
        .onTapGesture { clock.next() }
        // Not `interactions: .edit`: edit focus spends esc on ending the edit, so esc would not close.
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onKeyPress(.rightArrow) { clock.next(); return .handled }
        .onKeyPress(.leftArrow) { clock.previous(); return .handled }
        .onKeyPress(.space) { clock.togglePause(); return .handled }
        .onKeyPress(.escape) { close(); return .handled }
        .onAppear {
            focused = true
            fetch()
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Wrapped \(String(year))")
        .accessibilityIdentifier("wrapped-overlay")
    }

    /// Progress bars, the title, pause and close along the top; the keys along the bottom.
    private func chrome(scale: CGFloat) -> some View {
        VStack(spacing: 14) {
            if case .loaded = load {
                TimelineView(.animation(minimumInterval: 1.0 / 30)) { context in
                    ProgressBars(clock: clock)
                        .onChange(of: context.date) { old, new in
                            // A hidden window stops the timeline: never jump more than a frame or two.
                            clock.tick(min(max(new.timeIntervalSince(old), 0), 0.25))
                        }
                }
            }
            HStack(spacing: 12) {
                Text("Tyranny Wrapped · \(String(year))")
                    .font(.system(size: 15 * scale, weight: .bold))
                    .opacity(0.8)
                Spacer()
                if case .loaded = load {
                    ChromeButton(symbol: clock.paused ? "play.fill" : "pause.fill",
                                 help: clock.paused ? "Resume (space)" : "Pause (space)",
                                 identifier: "wrapped-pause") { clock.togglePause() }
                }
                ChromeButton(symbol: "xmark", help: "Close (esc)", identifier: "wrapped-close", action: close)
            }
            Spacer()
            Text("← → to move · space to pause · esc to close")
                .font(.system(size: 12 * scale, weight: .medium))
                .opacity(0.55)
        }
        .foregroundStyle(.white)
        // Clear of the window's close, minimise and zoom buttons.
        .padding(.top, 40)
        .padding(.horizontal, 24)
        .padding(.bottom, 16)
    }

    private func fetch() {
        // A cancelled request would close the shared connection: fetch unstructured.
        Task { @MainActor in
            do {
                let reply: WrappedResponse = try await player.send("wrapped", ["year": .int(year)])
                let slides = WrappedDeck.slides(reply.report)
                clock = SlideshowClock(count: slides.count)
                load = .loaded(slides)
            } catch {
                load = .failed((error as? ServerFailure)?.message ?? error.localizedDescription)
            }
        }
    }
}

// MARK: - Chrome

/// One story-style bar per slide: past slides full, the current one filling, later ones empty.
private struct ProgressBars: View {
    let clock: SlideshowClock

    var body: some View {
        HStack(spacing: 5) {
            ForEach(0..<clock.count, id: \.self) { index in
                GeometryReader { geometry in
                    ZStack(alignment: .leading) {
                        Capsule().fill(.white.opacity(0.3))
                        Capsule().fill(.white)
                            .frame(width: geometry.size.width * fill(index))
                    }
                }
                .frame(height: 4)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Slide \(clock.index + 1) of \(clock.count)" + (clock.paused ? ", paused" : ""))
        .accessibilityIdentifier("wrapped-progress")
    }

    private func fill(_ index: Int) -> Double {
        index < clock.index ? 1 : index == clock.index ? clock.progress : 0
    }
}

private struct ChromeButton: View {
    let symbol: String
    let help: String
    let identifier: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 13, weight: .bold))
                .frame(width: 30, height: 30)
                .background(Circle().fill(.white.opacity(0.18)))
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
        .accessibilityIdentifier(identifier)
    }
}

// MARK: - Slides

/// One slide's text. Elements come in staggered: label, then headline, then detail, with
/// ranked rows one by one. Reduce Motion keeps the fades and drops the movement.
private struct SlideView: View {
    let slide: WrappedSlide?
    let year: Int
    let scale: CGFloat
    var failure: String?
    let close: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var shown = false

    var body: some View {
        content
            .foregroundStyle(.white)
            .padding(.horizontal, 80 * scale)
            .padding(.vertical, 90)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("wrapped-slide-\(slide?.key ?? "failed")")
            .onAppear { shown = true }
    }

    @ViewBuilder private var content: some View {
        switch slide {
        case nil:
            statement("Wrapped \(String(year))", "Your Wrapped could not load", failure, exit: true)
        case .year(let listenedMs, let plays):
            statement("In \(String(year)) you listened for", WrappedFormat.listened(listenedMs),
                      "across \(DisplayFormat.count(plays)) plays")
        case .topArtist(let artist):
            statement("Your top artist", artist.name,
                      "\(WrappedFormat.listened(artist.listened_ms)) · \(DisplayFormat.count(artist.plays)) plays · "
                        + "\(WrappedFormat.percent(artist.share)) of your listening")
        case .topArtists(let artists):
            ranked("Your top artists", artists.prefix(5).map {
                Row(name: $0.name, detail: nil, value: WrappedFormat.listened($0.listened_ms))
            })
        case .topTrack(let track):
            statement("Your top track", track.title,
                      WrappedFormat.by(track.artist) + "\(DisplayFormat.count(track.plays)) plays")
        case .topTracks(let tracks):
            ranked("Your top tracks", tracks.prefix(5).map {
                Row(name: $0.title, detail: $0.artist.isEmpty ? nil : $0.artist,
                    value: "\(DisplayFormat.count($0.plays)) plays")
            })
        case .discovery(let discovery):
            statement("Your biggest discovery", discovery.name,
                      "First heard on \(WrappedFormat.day(discovery.first_played)), "
                        + "then \(DisplayFormat.count(discovery.plays)) plays")
        case .peakTime(let peak):
            statement("You listen most on", WrappedPhrase.peak(peak), nil)
        case .months(let months):
            monthGrid(months)
        case .mostSkipped(let track):
            statement("Your most skipped", track.title,
                      WrappedFormat.by(track.artist)
                        + "skipped \(DisplayFormat.count(track.skips)) times. You kept giving it another chance.")
        case .comeback(let comeback):
            statement("A lapsed love returns", comeback.name,
                      "Back after \(DisplayFormat.count(comeback.gap_days)) days away, "
                        + "with \(DisplayFormat.count(comeback.plays)) plays this year")
        case .topStation(let station):
            statement("Your top station", station.name, "\(WrappedFormat.listened(station.listened_ms)) of radio")
        case .summary(let artists, let tracks, let listenedMs):
            summary(artists: artists, tracks: tracks, listenedMs: listenedMs)
        case .notEnough:
            statement("Wrapped \(String(year))", "Not enough listening yet for a Wrapped",
                      "A year needs 50 plays. Play some more, then come back.", exit: true)
        }
    }

    // MARK: Layouts

    private func statement(_ label: String, _ headline: String, _ detail: String?, exit: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 18 * scale) {
            Text(label)
                .font(.system(size: 28 * scale, weight: .bold))
                .opacity(0.9)
                .reveal(shown, delay: 0.1, reduceMotion: reduceMotion)
            Text(headline)
                .font(.system(size: 88 * scale, weight: .black))
                .lineLimit(3)
                .minimumScaleFactor(0.4)
                .fixedSize(horizontal: false, vertical: true)
                .reveal(shown, delay: 0.4, reduceMotion: reduceMotion)
            if let detail {
                Text(detail)
                    .font(.system(size: 28 * scale, weight: .semibold))
                    .opacity(0.85)
                    .fixedSize(horizontal: false, vertical: true)
                    .reveal(shown, delay: 0.75, reduceMotion: reduceMotion)
            }
            if exit {
                Button(action: close) {
                    Text("Close")
                        .font(.system(size: 18 * scale, weight: .bold))
                        .foregroundStyle(.black)
                        .padding(.horizontal, 28 * scale)
                        .padding(.vertical, 12 * scale)
                        .background(Capsule().fill(.white))
                }
                    .buttonStyle(.plain)
                    .padding(.top, 12)
                    .reveal(shown, delay: 1.0, reduceMotion: reduceMotion)
                    .accessibilityIdentifier("wrapped-exit")
            }
        }
        .frame(maxWidth: 980 * scale, alignment: .leading)
    }

    private struct Row {
        let name: String
        let detail: String?
        let value: String
    }

    private func ranked(_ label: String, _ rows: [Row]) -> some View {
        VStack(alignment: .leading, spacing: 22 * scale) {
            Text(label)
                .font(.system(size: 28 * scale, weight: .bold))
                .opacity(0.9)
                .reveal(shown, delay: 0.1, reduceMotion: reduceMotion)
            ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                HStack(alignment: .firstTextBaseline, spacing: 22 * scale) {
                    Text("\(index + 1)")
                        .font(.system(size: 44 * scale, weight: .black))
                        .monospacedDigit()
                        .opacity(0.55)
                        .frame(width: 44 * scale, alignment: .trailing)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(row.name)
                            .font(.system(size: 44 * scale, weight: .heavy))
                        if let detail = row.detail {
                            Text(detail)
                                .font(.system(size: 20 * scale, weight: .semibold))
                                .opacity(0.75)
                        }
                    }
                    .lineLimit(1)
                    Spacer(minLength: 16)
                    Text(row.value)
                        .font(.system(size: 24 * scale, weight: .bold))
                        .monospacedDigit()
                        .opacity(0.85)
                }
                .reveal(shown, delay: 0.45 + 0.18 * Double(index), reduceMotion: reduceMotion)
            }
        }
        .frame(maxWidth: 980 * scale, alignment: .leading)
    }

    private func monthGrid(_ months: [MonthTop]) -> some View {
        let names = Calendar.statsCalendar().monthSymbols
        return VStack(alignment: .leading, spacing: 22 * scale) {
            Text("Your year in months")
                .font(.system(size: 28 * scale, weight: .bold))
                .opacity(0.9)
                .reveal(shown, delay: 0.1, reduceMotion: reduceMotion)
            LazyVGrid(columns: [GridItem(.flexible(), spacing: 32 * scale, alignment: .leading),
                                GridItem(.flexible(), spacing: 32 * scale, alignment: .leading)],
                      alignment: .leading, spacing: 14 * scale) {
                ForEach(Array(months.prefix(12).enumerated()), id: \.offset) { index, month in
                    HStack(alignment: .firstTextBaseline, spacing: 14 * scale) {
                        Text(names[(Int(month.month) + 11) % 12])
                            .font(.system(size: 20 * scale, weight: .bold))
                            .opacity(0.7)
                            .frame(width: 120 * scale, alignment: .leading)
                        Text(month.top_artist ?? "—")
                            .font(.system(size: 30 * scale, weight: .heavy))
                            .lineLimit(1)
                    }
                    .reveal(shown, delay: 0.4 + 0.08 * Double(index), reduceMotion: reduceMotion)
                }
            }
        }
        .frame(maxWidth: 1040 * scale, alignment: .leading)
    }

    private func summary(artists: [WrappedArtist], tracks: [RankedTrack], listenedMs: UInt64) -> some View {
        VStack(alignment: .leading, spacing: 26 * scale) {
            Text("Wrapped \(String(year))")
                .font(.system(size: 52 * scale, weight: .black))
            HStack(alignment: .top, spacing: 48 * scale) {
                summaryColumn("Top artists", artists.prefix(5).map { ($0.name, nil) }, start: 0.3)
                summaryColumn("Top tracks", tracks.prefix(5).map { ($0.title, $0.artist.isEmpty ? nil : $0.artist) },
                              start: 0.5)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text("Listened")
                    .font(.system(size: 18 * scale, weight: .bold))
                    .opacity(0.7)
                Text(WrappedFormat.listened(listenedMs))
                    .font(.system(size: 44 * scale, weight: .black))
            }
            .reveal(shown, delay: 1.5, reduceMotion: reduceMotion)
        }
        .padding(40 * scale)
        .frame(maxWidth: 860 * scale, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 28 * scale).fill(.black.opacity(0.28)))
        .overlay(RoundedRectangle(cornerRadius: 28 * scale).strokeBorder(.white.opacity(0.15)))
        .scaleEffect(shown || reduceMotion ? 1 : 0.94)
        .opacity(shown ? 1 : 0)
        .animation(.easeOut(duration: 0.5), value: shown)
        .frame(maxWidth: .infinity)
    }

    /// Ranked names, each with an optional detail after it (a track's artist).
    private func summaryColumn(_ title: String, _ names: [(String, String?)], start: Double) -> some View {
        VStack(alignment: .leading, spacing: 10 * scale) {
            Text(title)
                .font(.system(size: 18 * scale, weight: .bold))
                .opacity(0.7)
            ForEach(Array(names.enumerated()), id: \.offset) { index, row in
                HStack(alignment: .firstTextBaseline, spacing: 10 * scale) {
                    Text("\(index + 1)")
                        .monospacedDigit()
                        .opacity(0.55)
                    Text(row.0)
                        .layoutPriority(1)
                    if let detail = row.1 {
                        Text(detail)
                            .font(.system(size: 17 * scale, weight: .semibold))
                            .opacity(0.65)
                    }
                }
                .lineLimit(1)
                .font(.system(size: 26 * scale, weight: .heavy))
                .reveal(shown, delay: start + 0.15 * Double(index), reduceMotion: reduceMotion)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private extension View {
    /// Fades in after `delay`, rising into place unless Reduce Motion is on.
    func reveal(_ shown: Bool, delay: Double, reduceMotion: Bool) -> some View {
        opacity(shown ? 1 : 0)
            .offset(y: shown || reduceMotion ? 0 : 30)
            .animation((reduceMotion ? Animation.easeOut(duration: 0.45)
                        : .spring(response: 0.6, dampingFraction: 0.82)).delay(delay), value: shown)
    }
}

private enum WrappedFormat {
    /// "123 hours", or minutes below two hours.
    static func listened(_ ms: UInt64) -> String {
        let minutes = ms / 60_000
        if minutes < 120 { return minutes == 1 ? "1 minute" : "\(DisplayFormat.count(minutes)) minutes" }
        return "\(DisplayFormat.count(minutes / 60)) hours"
    }

    static func percent(_ fraction: Double) -> String { "\(Int((fraction * 100).rounded()))%" }

    /// "3 March".
    static func day(_ seconds: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(seconds)).formatted(.dateTime.day().month(.wide))
    }

    /// "by Artist · ", or nothing for an empty artist.
    static func by(_ artist: String) -> String { artist.isEmpty ? "" : "by \(artist) · " }
}

// MARK: - Backgrounds

/// A slide's colours: a diagonal gradient from `start` to `end`, with soft blooms of `glow`.
private struct WrappedPalette {
    let start: Color
    let end: Color
    let glow: Color

    init(_ start: UInt32, _ end: UInt32, _ glow: UInt32) {
        self.start = Self.colour(start)
        self.end = Self.colour(end)
        self.glow = Self.colour(glow)
    }

    private static func colour(_ hex: UInt32) -> Color {
        Color(red: Double(hex >> 16 & 0xFF) / 255, green: Double(hex >> 8 & 0xFF) / 255, blue: Double(hex & 0xFF) / 255)
    }

    static let year = WrappedPalette(0x2E1065, 0xA21CAF, 0xF472B6)
    static let loading = WrappedPalette(0x111827, 0x374151, 0x6B7280)
}

private extension WrappedSlide {
    /// The slide's accessibility identifier suffix.
    var key: String {
        switch self {
        case .year: "year"
        case .topArtist: "top-artist"
        case .topArtists: "top-artists"
        case .topTrack: "top-track"
        case .topTracks: "top-tracks"
        case .discovery: "discovery"
        case .peakTime: "peak-time"
        case .months: "months"
        case .mostSkipped: "most-skipped"
        case .comeback: "comeback"
        case .topStation: "top-station"
        case .summary: "summary"
        case .notEnough: "not-enough"
        }
    }

    /// Each kind of slide has its own colours.
    var palette: WrappedPalette {
        switch self {
        case .year: .year
        case .topArtist: WrappedPalette(0x7C2D12, 0xEA580C, 0xFDBA74)
        case .topArtists: WrappedPalette(0x052E16, 0x15803D, 0x86EFAC)
        case .topTrack: WrappedPalette(0x172554, 0x1D4ED8, 0x67E8F9)
        case .topTracks: WrappedPalette(0x4C0519, 0xBE123C, 0xFDA4AF)
        case .discovery: WrappedPalette(0x042F2E, 0x0F766E, 0xBEF264)
        case .peakTime: WrappedPalette(0x1E1B4B, 0x6D28D9, 0xC4B5FD)
        case .months: WrappedPalette(0x422006, 0xA16207, 0xFDE047)
        case .mostSkipped: WrappedPalette(0x450A0A, 0xB91C1C, 0xFCA5A5)
        case .comeback: WrappedPalette(0x312E81, 0xBE185D, 0xF9A8D4)
        case .topStation: WrappedPalette(0x082F49, 0x0369A1, 0x7DD3FC)
        case .summary: WrappedPalette(0x0F172A, 0x7E22CE, 0xF0ABFC)
        case .notEnough: .loading
        }
    }
}

private struct WrappedBackdrop: View {
    let palette: WrappedPalette

    var body: some View {
        GeometryReader { geometry in
            let size = max(geometry.size.width, geometry.size.height)
            ZStack {
                LinearGradient(colors: [palette.start, palette.end], startPoint: .topLeading, endPoint: .bottomTrailing)
                Circle()
                    .fill(palette.glow.opacity(0.45))
                    .frame(width: size * 0.55)
                    .blur(radius: size * 0.12)
                    .position(x: geometry.size.width * 0.92, y: geometry.size.height * 0.08)
                Circle()
                    .fill(palette.glow.opacity(0.3))
                    .frame(width: size * 0.4)
                    .blur(radius: size * 0.1)
                    .position(x: geometry.size.width * 0.78, y: geometry.size.height * 1.02)
            }
        }
        .clipped()
        .accessibilityHidden(true)
    }
}

// MARK: - Menu and banner

/// Go › Wrapped: "<year> so far" for the current year, then each past year with plays, newest
/// first. It is disabled while no year has plays.
struct WrappedMenu: View {
    let years: [Int]
    let open: (Int) -> Void

    var body: some View {
        let current = Calendar.statsCalendar().component(.year, from: Date())
        let title = "\(current) so far"
        if years.isEmpty {
            // A disabled Menu disables only its items; the submenu item itself stays enabled.
            Button("Wrapped") {}
                .disabled(true)
        } else {
            Menu("Wrapped") {
                Button(title) { open(current) }
                ForEach(years.filter { $0 < current }.sorted(by: >), id: \.self) { year in
                    Button(String(year)) { open(year) }
                }
            }
        }
    }
}

/// The Stats page's December banner: it opens the year's Wrapped, or dismisses it for the year.
struct WrappedBanner: View {
    let year: Int
    let open: () -> Void
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "sparkles")
                .font(.title2)
            VStack(alignment: .leading, spacing: 2) {
                Text("Your \(String(year)) Wrapped is here")
                    .font(.headline)
                Text("Your year in music, one slide at a time.")
                    .font(.callout)
                    .opacity(0.85)
            }
            Spacer(minLength: 8)
            Button("Play", action: open)
                .buttonStyle(.borderedProminent)
                .tint(.white.opacity(0.25))
                .accessibilityIdentifier("stats-wrapped-banner-play")
            Button(action: dismiss) {
                Image(systemName: "xmark")
                    .font(.system(size: 11, weight: .bold))
                    .frame(width: 22, height: 22)
            }
            .buttonStyle(.plain)
            .help("Don’t show this again for \(String(year))")
            .accessibilityLabel("Dismiss")
            .accessibilityIdentifier("stats-wrapped-banner-dismiss")
        }
        .foregroundStyle(.white)
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(LinearGradient(colors: [WrappedPalette.year.start, WrappedPalette.year.end],
                                   startPoint: .leading, endPoint: .trailing))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("stats-wrapped-banner")
    }
}
