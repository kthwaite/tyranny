import Charts
import SwiftUI
import TyrannyCore

/// Stats (spec "Client: Stats page"): one scrolling page for the chosen period. The header steps
/// and picks the period, and in December offers the year's Wrapped. Below it: tiles, top
/// artists and tracks, listening over time, the weekday × hour heatmap, new artists, most
/// skipped and stations, and two all-history panels.
struct StatsView: View {
    @Environment(\.themeColours) private var colours
    let model: StatsModel
    let player: PlayerModel
    let navigation: NavigationModel
    /// The year of the Wrapped banner, while this visit shows it.
    @State private var bannerYear: Int?

    private var calendar: Calendar { model.calendar }

    /// The year the header offers a Wrapped for, from 1 to 31 December.
    private var offerYear: Int? {
        #if DEBUG
        let now = NativeStatsSmoke.offerDate ?? Date()
        #else
        let now = Date()
        #endif
        return WrappedOffer.callToActionYear(now: now, calendar: calendar)
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if let year = bannerYear {
                WrappedBanner(year: year) {
                    navigation.wrappedYear = year
                } dismiss: {
                    model.dismissBanner(year: year)
                    bannerYear = nil
                }
                Divider()
            }
            ScrollView {
                if let report = model.report {
                    StatsReportView(report: report, period: model.loadedPeriod ?? model.period, model: model,
                                    player: player, navigation: navigation)
                        .padding(16)
                } else if model.error == nil {
                    ProgressView()
                        .frame(maxWidth: .infinity)
                        .padding(.top, 80)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-stats")
        .onAppear {
            model.appeared()
            if let year = offerYear, model.takeBannerOffer(year: year) { bannerYear = year }
            #if DEBUG
            NativeStatsSmoke.stats = model
            #endif
        }
        .onDisappear { model.disappeared() }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Text("Stats")
                .font(.title2.weight(.semibold))
                .accessibilityAddTraits(.isHeader)
            HStack(spacing: 6) {
                Button { model.period = model.period.stepped(-1, calendar: calendar) } label: {
                    Image(systemName: "chevron.left")
                }
                .disabled(model.period.kind == .all)
                .help("Previous period")
                .accessibilityLabel("Previous period")
                .accessibilityIdentifier("stats-previous")
                Picker("Period", selection: kind) {
                    ForEach(StatsPeriodKind.allCases, id: \.self) { kind in
                        Text(kind.title).tag(kind)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                .accessibilityIdentifier("stats-period-kind")
                Button { model.period = model.period.stepped(1, calendar: calendar) } label: {
                    Image(systemName: "chevron.right")
                }
                .disabled(!model.period.canStepForward(now: Date(), calendar: calendar))
                .help("Next period")
                .accessibilityLabel("Next period")
                .accessibilityIdentifier("stats-next")
            }
            Text(model.period.label(calendar: calendar))
                .font(.headline)
                .monospacedDigit()
                .accessibilityIdentifier("stats-period-label")
            if let error = model.error {
                Text(error)
                    .font(.callout).foregroundStyle(colours.error)
                    .lineLimit(2)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("stats-error")
            }
            Spacer(minLength: 0)
            if let year = offerYear {
                Button("Your \(String(year)) Wrapped ▶") { navigation.wrappedYear = year }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("stats-wrapped")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    /// Changing the kind keeps the anchor, so a past week becomes the month around it.
    private var kind: Binding<StatsPeriodKind> {
        Binding(get: { model.period.kind },
                set: { model.period = StatsPeriod(kind: $0, anchor: model.period.anchor) })
    }
}

private extension StatsPeriodKind {
    var title: String {
        switch self {
        case .day: "Day"
        case .week: "Week"
        case .month: "Month"
        case .year: "Year"
        case .all: "All"
        }
    }
}

// MARK: - Report

private struct StatsReportView: View {
    let report: StatsReport
    let period: StatsPeriod
    let model: StatsModel
    let player: PlayerModel
    let navigation: NavigationModel

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            tiles
            HStack(alignment: .top, spacing: 12) {
                TopArtistsPanel(report: report, open: openArtist)
                TopTracksPanel(report: report, open: openTrack)
            }
            OverTimePanel(report: report, period: period, calendar: model.calendar) { model.period = $0 }
            HeatmapPanel(heatmap: report.heatmap)
            HStack(alignment: .top, spacing: 12) {
                NewArtistsPanel(artists: report.new_artists, open: openArtist)
                MostSkippedPanel(tracks: report.most_skipped, open: openTrack)
                StationsPanel(stations: report.stations)
            }
            VStack(alignment: .leading, spacing: 8) {
                Text("Across all your history")
                    .font(.title3.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                HStack(alignment: .top, spacing: 12) {
                    LapsedLovesPanel(loves: report.lapsed_loves, spanDays: report.history_span_days, plays: report.plays,
                                     open: openArtist)
                    ShallowDivesPanel(dives: report.shallow_dives, spanDays: report.history_span_days, open: openArtist)
                }
            }
        }
    }

    private var tiles: some View {
        HStack(alignment: .top, spacing: 12) {
            StatsTile(title: "Plays", value: DisplayFormat.count(report.plays), identifier: "stats-tile-plays")
            StatsTile(title: "Listened", value: StatsFormat.listened(report.listened_ms), identifier: "stats-tile-listened",
                      footnote: report.untimed_plays > 0
                        ? "\(DisplayFormat.count(report.untimed_plays)) earlier \(report.untimed_plays == 1 ? "play has" : "plays have") no listening time"
                        : nil)
            StatsTile(title: "Artists", value: DisplayFormat.count(report.artists), identifier: "stats-tile-artists")
            StatsTile(title: "Skip rate", value: report.skip_rate.map(StatsFormat.percent) ?? "—",
                      identifier: "stats-tile-skip-rate")
        }
    }

    /// Shows the artist in Artists, as Show in Artist does.
    private func openArtist(_ id: UInt64) {
        player.endImport()
        navigation.reveal = .artist(id)
    }

    /// Plays the track once, as a double-click on a History row does.
    private func openTrack(_ id: UInt64) {
        guard let revision = player.snapshot?.library.revision else { return }
        player.activation.start(EnqueueJob(
            action: .play, source: QueueSource(kind: .songs, name: "Stats"),
            request: .source(HistoryPlayback.selection(trackID: id, libraryRevision: revision), player: player),
            status: nil))
    }
}

// MARK: - Building blocks

private enum StatsFormat {
    /// "2h 05m", "42m", or "<1m".
    static func listened(_ ms: UInt64) -> String {
        let minutes = ms / 60_000
        if minutes == 0 { return ms == 0 ? "0m" : "<1m" }
        if minutes < 60 { return "\(minutes)m" }
        return "\(DisplayFormat.count(minutes / 60))h \(String(format: "%02d", minutes % 60))m"
    }

    static func percent(_ fraction: Double) -> String { "\(Int((fraction * 100).rounded()))%" }

    static func date(_ seconds: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(seconds)).formatted(.dateTime.day().month(.abbreviated).year())
    }
}

private struct Card<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        content
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .background(RoundedRectangle(cornerRadius: 8).fill(.quinary))
    }
}

private struct StatsTile: View {
    let title: String
    let value: String
    let identifier: String
    var footnote: String?

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                Text(value)
                    .font(.system(size: 28, weight: .semibold, design: .rounded))
                    .monospacedDigit()
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)
                    .accessibilityIdentifier(identifier)
                if let footnote {
                    Text(footnote)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("\(identifier)-footnote")
                }
            }
        }
    }
}

/// A titled card. `accessory` sits at the trailing edge of the title row.
private struct Panel<Accessory: View, Content: View>: View {
    let title: String
    let identifier: String
    @ViewBuilder var accessory: Accessory
    @ViewBuilder var content: Content

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text(title)
                        .font(.headline)
                        .accessibilityAddTraits(.isHeader)
                    Spacer(minLength: 8)
                    accessory
                }
                content
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("stats-\(identifier)")
    }
}

extension Panel where Accessory == EmptyView {
    init(title: String, identifier: String, @ViewBuilder content: () -> Content) {
        self.init(title: title, identifier: identifier, accessory: { EmptyView() }, content: content)
    }
}

private struct EmptyPanel: View {
    let identifier: String
    let title: String
    var detail: String?

    var body: some View {
        VStack(spacing: 4) {
            Text(title)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .accessibilityIdentifier("stats-\(identifier)-empty-title")
            if let detail {
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .multilineTextAlignment(.center)
                    .accessibilityIdentifier("stats-\(identifier)-empty-detail")
            }
        }
        .frame(maxWidth: .infinity, minHeight: 72)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("stats-\(identifier)-empty")
    }
}

/// One ranked row: name, detail, value, and a bar for `fraction` of the panel's largest value.
/// Double-click opens the row when it has a library ID.
private struct RankRow: View {
    @Environment(\.themeColours) private var colours
    let rank: Int?
    let name: String
    var detail: String?
    let value: String
    let fraction: Double
    var open: (() -> Void)?
    var help: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            if let rank {
                Text("\(rank)")
                    .font(.callout)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                    .frame(width: 20, alignment: .trailing)
            }
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(name).lineLimit(1)
                    if let detail {
                        Text(detail).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    Text(value)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                GeometryReader { geometry in
                    Capsule()
                        .fill(colours.accent.opacity(0.8))
                        .frame(width: max(3, geometry.size.width * min(max(fraction, 0), 1)))
                }
                .frame(height: 4)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2) { open?() }
        .help(open == nil ? "" : (help ?? ""))
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(open == nil ? [] : .isButton)
        .accessibilityAction { open?() }
    }
}

/// Shows the first `limit` rows, with a button for the rest.
private struct Ranked<Row, RowView: View>: View {
    let rows: [Row]
    var limit = 10
    @ViewBuilder let row: (Int, Row) -> RowView
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(rows.prefix(expanded ? rows.count : limit).enumerated()), id: \.offset) { index, item in
                row(index, item)
            }
            if rows.count > limit {
                Button(expanded ? "Show Fewer" : "Show All \(rows.count)") { expanded.toggle() }
                    .buttonStyle(.link)
                    .font(.callout)
            }
        }
    }
}

private enum Measure: String, CaseIterable {
    case plays = "Plays", time = "Time"
}

private struct MeasurePicker: View {
    @Binding var measure: Measure
    let identifier: String

    var body: some View {
        Picker("Rank by", selection: $measure) {
            ForEach(Measure.allCases, id: \.self) { Text($0.rawValue).tag($0) }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .controlSize(.small)
        .fixedSize()
        .accessibilityIdentifier(identifier)
    }
}

// MARK: - Panels

private struct TopArtistsPanel: View {
    let report: StatsReport
    let open: (UInt64) -> Void
    @State private var measure = Measure.plays

    var body: some View {
        let rows = measure == .plays ? report.top_artists_by_plays : report.top_artists_by_time
        let largest = rows.map { value($0) }.max() ?? 0
        Panel(title: "Top artists", identifier: "top-artists") {
            MeasurePicker(measure: $measure, identifier: "stats-top-artists-measure")
        } content: {
            if rows.isEmpty {
                EmptyPanel(identifier: "top-artists", title: "No plays in this period")
            } else {
                Ranked(rows: rows) { index, artist in
                    RankRow(rank: index + 1, name: artist.name, value: label(artist),
                            fraction: largest == 0 ? 0 : Double(value(artist)) / Double(largest),
                            open: artist.artist_id.map { id in { open(id) } }, help: "Double-click to show in Artists")
                }
            }
        }
    }

    private func value(_ artist: RankedArtist) -> UInt64 { measure == .plays ? artist.plays : artist.listened_ms }
    private func label(_ artist: RankedArtist) -> String {
        measure == .plays ? DisplayFormat.count(artist.plays) : StatsFormat.listened(artist.listened_ms)
    }
}

private struct TopTracksPanel: View {
    let report: StatsReport
    let open: (UInt64) -> Void
    @State private var measure = Measure.plays

    var body: some View {
        let rows = measure == .plays ? report.top_tracks_by_plays : report.top_tracks_by_time
        let largest = rows.map { value($0) }.max() ?? 0
        Panel(title: "Top tracks", identifier: "top-tracks") {
            MeasurePicker(measure: $measure, identifier: "stats-top-tracks-measure")
        } content: {
            if rows.isEmpty {
                EmptyPanel(identifier: "top-tracks", title: "No plays in this period")
            } else {
                Ranked(rows: rows) { index, track in
                    RankRow(rank: index + 1, name: track.title, detail: track.artist, value: label(track),
                            fraction: largest == 0 ? 0 : Double(value(track)) / Double(largest),
                            open: track.track_id.map { id in { open(id) } }, help: "Double-click to play")
                }
            }
        }
    }

    private func value(_ track: RankedTrack) -> UInt64 { measure == .plays ? track.plays : track.listened_ms }
    private func label(_ track: RankedTrack) -> String {
        measure == .plays ? DisplayFormat.count(track.plays) : StatsFormat.listened(track.listened_ms)
    }
}

/// `over_time` as bars. Clicking a bar narrows the period to that bucket.
private struct OverTimePanel: View {
    @Environment(\.themeColours) private var colours
    let report: StatsReport
    let period: StatsPeriod
    let calendar: Calendar
    let narrow: (StatsPeriod) -> Void
    @State private var hovered: Int64?

    private var unit: Calendar.Component {
        switch period.kind {
        case .all: .year
        case .year: .month
        case .month, .week: .day
        case .day: .hour
        }
    }

    /// Hours, or minutes when no bucket reaches two hours.
    private var inHours: Bool { (report.over_time.map(\.listened_ms).max() ?? 0) >= 7_200_000 }

    var body: some View {
        Panel(title: "Listening over time", identifier: "over-time") {
            if report.over_time.allSatisfy({ $0.listened_ms == 0 }) {
                EmptyPanel(identifier: "over-time", title: "No listening in this period")
            } else {
                chart
                    .frame(height: 180)
            }
        }
    }

    private var chart: some View {
        Chart(report.over_time, id: \.start) { bucket in
            BarMark(x: .value("Start", date(bucket.start), unit: unit),
                    y: .value(inHours ? "Hours" : "Minutes", amount(bucket.listened_ms)))
                .foregroundStyle(colours.accent.opacity(hovered == nil || hovered == bucket.start ? 0.85 : 0.4))
                .clipShape(UnevenRoundedRectangle(topLeadingRadius: 3, topTrailingRadius: 3))
                .annotation(position: .top, alignment: .center, spacing: 4) {
                    if hovered == bucket.start {
                        Text(StatsFormat.listened(bucket.listened_ms))
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                }
        }
        .chartYAxisLabel(inHours ? "Hours" : "Minutes")
        .chartXAxis {
            AxisMarks(values: .automatic(desiredCount: 8)) { _ in
                AxisGridLine().foregroundStyle(.clear)
                AxisTick()
                AxisValueLabel(format: axisFormat)
            }
        }
        .chartOverlay { proxy in
            GeometryReader { geometry in
                Rectangle()
                    .fill(.clear)
                    .contentShape(Rectangle())
                    .onContinuousHover { phase in
                        switch phase {
                        case .active(let location): hovered = bucket(at: location, proxy: proxy, geometry: geometry)?.start
                        case .ended: hovered = nil
                        }
                    }
                    .onTapGesture { location in
                        guard let bucket = bucket(at: location, proxy: proxy, geometry: geometry),
                              let next = period.drillDown(bucketStart: date(bucket.start), calendar: calendar)
                        else { return }
                        hovered = nil
                        narrow(next)
                    }
            }
        }
        .accessibilityIdentifier("stats-over-time-chart")
    }

    private var axisFormat: Date.FormatStyle {
        switch period.kind {
        case .all: .dateTime.year()
        case .year: .dateTime.month(.abbreviated)
        case .month: .dateTime.day()
        case .week: .dateTime.weekday(.abbreviated)
        case .day: .dateTime.hour()
        }
    }

    private func date(_ seconds: Int64) -> Date { Date(timeIntervalSince1970: TimeInterval(seconds)) }

    private func amount(_ ms: UInt64) -> Double { Double(ms) / (inHours ? 3_600_000 : 60_000) }

    /// The last bucket that starts at or before the pointer.
    private func bucket(at location: CGPoint, proxy: ChartProxy, geometry: GeometryProxy) -> Bucket? {
        guard let plot = proxy.plotFrame else { return nil }
        let frame = geometry[plot]
        guard frame.contains(location),
              let pointer: Date = proxy.value(atX: location.x - frame.origin.x) else { return nil }
        let seconds = Int64(pointer.timeIntervalSince1970)
        return report.over_time.last { $0.start <= seconds }
    }
}

/// Listened time by weekday (Monday first) and local hour, in one hue.
private struct HeatmapPanel: View {
    @Environment(\.themeColours) private var colours
    let heatmap: [[UInt64]]

    private static let weekdays = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]

    var body: some View {
        let largest = heatmap.joined().max() ?? 0
        Panel(title: "When you listen", identifier: "heatmap") {
            if largest == 0 {
                EmptyPanel(identifier: "heatmap", title: "No listening in this period")
            } else {
                grid(largest: largest)
            }
        }
    }

    private func grid(largest: UInt64) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(Array(heatmap.prefix(7).enumerated()), id: \.offset) { day, hours in
                HStack(spacing: 3) {
                    Text(Self.weekdays[day])
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .frame(width: 30, alignment: .leading)
                    ForEach(Array(hours.prefix(24).enumerated()), id: \.offset) { hour, ms in
                        RoundedRectangle(cornerRadius: 3)
                            .fill(ms == 0 ? AnyShapeStyle(.quaternary)
                                  : AnyShapeStyle(colours.accent.opacity(0.15 + 0.85 * Double(ms) / Double(largest))))
                            .frame(height: 18)
                            .frame(maxWidth: .infinity)
                            .help("\(Self.weekdays[day]) \(String(format: "%02d:00", hour)) · \(StatsFormat.listened(ms))")
                    }
                }
            }
            HStack(spacing: 3) {
                Color.clear.frame(width: 30, height: 1)
                ForEach(0..<24, id: \.self) { hour in
                    Text(hour % 6 == 0 ? "\(hour)" : "")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Listening by weekday and hour")
    }
}

private struct NewArtistsPanel: View {
    let artists: [RankedArtist]
    let open: (UInt64) -> Void

    var body: some View {
        let largest = artists.map(\.plays).max() ?? 0
        Panel(title: "New artists", identifier: "new-artists") {
            if artists.isEmpty {
                EmptyPanel(identifier: "new-artists", title: "No new artists in this period")
            } else {
                Ranked(rows: artists) { _, artist in
                    RankRow(rank: nil, name: artist.name, value: DisplayFormat.count(artist.plays),
                            fraction: largest == 0 ? 0 : Double(artist.plays) / Double(largest),
                            open: artist.artist_id.map { id in { open(id) } }, help: "Double-click to show in Artists")
                }
            }
        }
    }
}

private struct MostSkippedPanel: View {
    let tracks: [SkippedTrack]
    let open: (UInt64) -> Void

    var body: some View {
        Panel(title: "Most skipped", identifier: "most-skipped") {
            if tracks.isEmpty {
                EmptyPanel(identifier: "most-skipped", title: "Nothing skipped often in this period")
            } else {
                Ranked(rows: tracks) { _, track in
                    RankRow(rank: nil, name: track.title, detail: track.artist,
                            value: "\(DisplayFormat.count(track.skips)) of \(DisplayFormat.count(track.started))",
                            fraction: track.started == 0 ? 0 : Double(track.skips) / Double(track.started),
                            open: track.track_id.map { id in { open(id) } }, help: "Double-click to play")
                }
            }
        }
    }
}

private struct StationsPanel: View {
    let stations: [StationTime]

    var body: some View {
        let largest = stations.map(\.listened_ms).max() ?? 0
        Panel(title: "Stations", identifier: "stations") {
            if stations.isEmpty {
                EmptyPanel(identifier: "stations", title: "No radio in this period")
            } else {
                Ranked(rows: stations) { _, station in
                    RankRow(rank: nil, name: station.name, value: StatsFormat.listened(station.listened_ms),
                            fraction: largest == 0 ? 0 : Double(station.listened_ms) / Double(largest))
                }
            }
        }
    }
}

/// Lapsed loves fill in once history spans this many days.
private let historyDaysNeeded: UInt64 = 90

/// "Not enough history yet": the empty state of lapsed loves while history is young.
/// With no plays at all, there is no first play to date the fill-in from.
private struct YoungHistory: View {
    let identifier: String
    let spanDays: UInt64
    let plays: UInt64
    let fallback: String

    var body: some View {
        if spanDays < historyDaysNeeded {
            EmptyPanel(identifier: identifier, title: "Not enough history yet — fills in after 90 days",
                       detail: "From \(fillsIn)")
        } else {
            EmptyPanel(identifier: identifier, title: fallback)
        }
    }

    private var fillsIn: String {
        guard spanDays > 0 || plays > 0 else { return "90 days after your first play" }
        let date = Calendar.statsCalendar().date(
            byAdding: .day, value: Int(historyDaysNeeded - spanDays), to: Date()) ?? Date()
        return StatsFormat.date(Int64(date.timeIntervalSince1970))
    }
}

private struct LapsedLovesPanel: View {
    let loves: [LapsedLove]
    let spanDays: UInt64
    let plays: UInt64
    let open: (UInt64) -> Void

    var body: some View {
        let largest = loves.map(\.plays).max() ?? 0
        Panel(title: "Lapsed loves", identifier: "lapsed-loves") {
            if loves.isEmpty {
                YoungHistory(identifier: "lapsed-loves", spanDays: spanDays, plays: plays, fallback: "No lapsed loves")
            } else {
                Ranked(rows: loves) { _, love in
                    RankRow(rank: nil, name: love.name, detail: "last played \(StatsFormat.date(love.last_played))",
                            value: "\(DisplayFormat.count(love.plays)) plays",
                            fraction: largest == 0 ? 0 : Double(love.plays) / Double(largest),
                            open: love.artist_id.map { id in { open(id) } }, help: "Double-click to show in Artists")
                }
            }
        }
    }
}

private struct ShallowDivesPanel: View {
    let dives: [ShallowDive]
    let spanDays: UInt64
    let open: (UInt64) -> Void

    var body: some View {
        Panel(title: "Shallow dives", identifier: "shallow-dives") {
            if dives.isEmpty {
                // No 90-day wait: shallow dives need only enough plays of one artist.
                EmptyPanel(identifier: "shallow-dives", title: "No shallow dives",
                           detail: "An artist shows here once it has \(DisplayFormat.count(ShallowDive.minimumPlays(historySpanDays: spanDays))) plays, with 60% on its top three tracks")
            } else {
                Ranked(rows: dives) { _, dive in
                    RankRow(rank: nil, name: dive.name, detail: detail(dive),
                            value: "\(StatsFormat.percent(dive.top_share)) in top 3",
                            fraction: dive.top_share,
                            open: dive.artist_id.map { id in { open(id) } },
                            help: dive.top_tracks.joined(separator: " · "))
                }
            }
        }
    }

    /// "3 of 42 tracks" for a library artist, else the play count.
    private func detail(_ dive: ShallowDive) -> String {
        if let total = dive.library_tracks {
            return "\(min(UInt64(dive.top_tracks.count), total)) of \(DisplayFormat.count(total)) tracks"
        }
        return "\(DisplayFormat.count(dive.plays)) plays"
    }
}
