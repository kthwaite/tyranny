import AppKit
import SwiftUI
import TyrannyCore

/// The album panel's sizes. The grid sizes the panel item before its tracks load, from the
/// album's track count.
enum AlbumExpansionMetrics {
    static let caretHeight: CGFloat = 10
    static let padding: CGFloat = 16
    static let cover: CGFloat = 150
    static let trackRowHeight: CGFloat = 18
    /// Longer tracklists scroll inside the panel.
    static let maximumTrackRows = 12
    /// Title 22 + artist 18 + facts 16 + gap 10 + gap 12 + buttons 24.
    static let fixedTextHeight: CGFloat = 102

    static func trackRows(_ count: Int) -> Int { max(1, (count + 1) / 2) }

    static func height(trackCount: UInt64) -> CGFloat {
        let rows = min(trackRows(Int(clamping: trackCount)), maximumTrackRows)
        let text = fixedTextHeight + CGFloat(rows) * trackRowHeight
        return caretHeight + 2 * padding + max(cover, text)
    }
}

/// The inline album panel (spec 4.4), shown as a full-width grid item after the last cell
/// of the expanded album's row, with a caret pointing at its cover. It retains the cover's
/// artwork while displayed.
final class AlbumExpansionItem: NSCollectionViewItem {
    static let reuse = NSUserInterfaceItemIdentifier("album-expansion-item")

    private var container: AlbumExpansionContainer { view as! AlbumExpansionContainer }
    private var hosting: NSHostingView<AlbumExpansionContent>?
    private weak var store: ArtworkStore?
    private var retained: ArtworkRequest?

    override func loadView() {
        view = AlbumExpansionContainer()
    }

    func show(_ expansion: AlbumExpansion, grid: AlbumGridModel, player: PlayerModel, caretX: CGFloat) {
        let store = player.artwork
        self.store = store
        let request = store.albumRequest(expansion.albumID)
        if request != retained {
            if let retained { store.release(retained) }
            retained = request
            if let request { store.retain(request) }
        }
        container.caretX = caretX
        let content = AlbumExpansionContent(expansion: expansion, grid: grid, player: player)
        if let hosting {
            hosting.rootView = content
        } else {
            let hosting = NSHostingView(rootView: content)
            hosting.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(hosting)
            NSLayoutConstraint.activate([
                hosting.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                hosting.trailingAnchor.constraint(equalTo: container.trailingAnchor),
                hosting.topAnchor.constraint(equalTo: container.topAnchor, constant: AlbumExpansionMetrics.caretHeight),
                hosting.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            ])
            self.hosting = hosting
        }
    }

    func releaseArtwork() {
        if let retained { store?.release(retained) }
        retained = nil
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        releaseArtwork()
    }
}

/// Paints the panel and its caret as one shape. `album-expansion` in Accessibility.
final class AlbumExpansionContainer: NSView {
    var caretX: CGFloat = 0 { didSet { if caretX != oldValue { needsDisplay = true } } }

    init() {
        super.init(frame: .zero)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Album")
        setAccessibilityIdentifier("album-expansion")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        let caret = AlbumExpansionMetrics.caretHeight
        let panel = NSRect(x: 0, y: caret, width: bounds.width, height: bounds.height - caret)
        let shape = NSBezierPath(roundedRect: panel, xRadius: 8, yRadius: 8)
        let x = min(max(caretX, 16), bounds.width - 16)
        let tip = NSBezierPath()
        tip.move(to: NSPoint(x: x - caret, y: caret + 0.5))
        tip.line(to: NSPoint(x: x, y: 0))
        tip.line(to: NSPoint(x: x + caret, y: caret + 0.5))
        tip.close()
        shape.append(tip)
        shape.windingRule = .nonZero
        NSColor.quaternarySystemFill.setFill()
        shape.fill()
    }

    /// Clicks on the panel's background neither select nor close anything.
    override func mouseDown(with event: NSEvent) {}
}

/// The panel's content: the cover, the title, artist and facts, the tracks in two columns,
/// and the album actions.
struct AlbumExpansionContent: View {
    let expansion: AlbumExpansion
    let grid: AlbumGridModel
    let player: PlayerModel

    private var album: AlbumSummary { expansion.album }

    var body: some View {
        HStack(alignment: .top, spacing: 20) {
            cover
            VStack(alignment: .leading, spacing: 0) {
                Text(album.title.isEmpty ? "Unknown Album" : album.title)
                    .font(.system(size: 17, weight: .semibold)).lineLimit(1)
                    .frame(height: 22, alignment: .leading)
                    .accessibilityIdentifier("album-expansion-title")
                Text(album.album_artist.isEmpty ? "Unknown Artist" : album.album_artist)
                    .font(.system(size: 13)).lineLimit(1)
                    .frame(height: 18, alignment: .leading)
                Text(DisplayFormat.albumExpansionFacts(album))
                    .font(.system(size: 11)).monospacedDigit().foregroundStyle(.secondary).lineLimit(1)
                    .frame(height: 16, alignment: .leading)
                    .accessibilityIdentifier("album-expansion-facts")
                tracks
                    .padding(.top, 10).padding(.bottom, 12)
                actions.frame(height: 24)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(AlbumExpansionMetrics.padding)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onExitCommand { grid.collapse() }
    }

    private var cover: some View {
        let artwork = player.artwork.value(for: player.artwork.albumRequest(expansion.albumID))
        return ZStack {
            RoundedRectangle(cornerRadius: 6).fill(.quaternary)
            if let image = artwork.image {
                Image(decorative: image, scale: 1).resizable().interpolation(.high)
            } else {
                Image(systemName: artwork.state == "loading" ? "photo" : "music.note")
                    .font(.system(size: 40)).foregroundStyle(.secondary)
            }
        }
        .frame(width: AlbumExpansionMetrics.cover, height: AlbumExpansionMetrics.cover)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Color(nsColor: .separatorColor)))
        .accessibilityLabel("Album artwork")
        .accessibilityValue(artwork.state)
        .accessibilityIdentifier("album-expansion-cover")
    }

    // MARK: Tracks

    @ViewBuilder private var tracks: some View {
        let rows = expansion.tracks
        let shown = min(AlbumExpansionMetrics.trackRows(Int(clamping: album.track_count)),
                        AlbumExpansionMetrics.maximumTrackRows)
        let height = CGFloat(shown) * AlbumExpansionMetrics.trackRowHeight
        Group {
            if let failure = expansion.failure {
                Text(failure).font(.callout).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else if !expansion.isLoaded {
                ProgressView().controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else {
                let split = (rows.count + 1) / 2
                let columns = HStack(alignment: .top, spacing: 24) {
                    column(rows[0..<split])
                    column(rows[split...])
                }
                if AlbumExpansionMetrics.trackRows(rows.count) > AlbumExpansionMetrics.maximumTrackRows {
                    ScrollView(.vertical) { columns }
                } else {
                    columns.frame(maxHeight: .infinity, alignment: .top)
                }
            }
        }
        .frame(height: height)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Tracks")
        .accessibilityValue(expansion.isLoaded ? String(rows.count) : "loading")
        .accessibilityIdentifier("album-expansion-tracks")
    }

    private func column(_ rows: ArraySlice<ListRow>) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(rows) { row in
                HStack(spacing: 6) {
                    Text(number(row.track)).monospacedDigit().foregroundStyle(.secondary)
                        .frame(width: 30, alignment: .trailing)
                    Text(row.track.title).lineLimit(1)
                        .foregroundStyle(row.available ? .primary : .secondary)
                    Spacer(minLength: 8)
                    Text(row.track.duration_ms.map { DisplayFormat.clock(Double($0) / 1000) } ?? "")
                        .monospacedDigit().foregroundStyle(.secondary)
                }
                .font(.system(size: 12))
                .frame(height: AlbumExpansionMetrics.trackRowHeight)
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("album-track-\(row.row_id)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    private func number(_ track: TrackSummary) -> String {
        let number = track.track_number > 0 ? String(track.track_number) : ""
        return album.disc_count > 1 && track.disc_number > 0 ? "\(track.disc_number)-\(number)" : number
    }

    // MARK: Actions

    private var actions: some View {
        HStack(spacing: 8) {
            Button { grid.play(album) } label: { Label("Play", systemImage: "play.fill") }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("album-play")
            Button { grid.play(album, shuffle: true) } label: { Label("Shuffle", systemImage: "shuffle") }
                .accessibilityIdentifier("album-shuffle")
            Button { grid.addToUpNext(album) } label: { Label("Add to Up Next", systemImage: "text.append") }
                .accessibilityIdentifier("album-add-up-next")
            Menu("Add to Playlist…") {
                let playlists = grid.regularPlaylists
                if playlists.isEmpty {
                    Text("No Playlists")
                } else {
                    ForEach(playlists) { playlist in
                        Button(playlist.name) { grid.addToPlaylist(album, playlist: playlist) }
                    }
                }
            }
            .fixedSize()
            .accessibilityIdentifier("album-add-playlist")
        }
        .controlSize(.small)
        .disabled(!player.canMutate)
    }
}
