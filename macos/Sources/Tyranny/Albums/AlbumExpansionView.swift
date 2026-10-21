import AppKit
import SwiftUI
import TyrannyCore

/// The album panel's sizes. The grid sizes the panel item before its tracks load, from the
/// album's track count.
enum AlbumExpansionMetrics {
    static let caretHeight: CGFloat = 10
    static let padding: CGFloat = 16
    static let cover: CGFloat = 150
    static let trackRowHeight: CGFloat = 20
    /// "and N more…" under the track table.
    static let moreHeight: CGFloat = 20
    /// Title 22 + artist 18 + facts 16 + gap 10 + gap 12 + buttons 24.
    static let fixedTextHeight: CGFloat = 102

    /// `AlbumPanel.visibleRows` rows. Longer tracklists scroll inside the table.
    static func tableHeight(trackCount: UInt64) -> CGFloat {
        CGFloat(AlbumPanel.visibleRows(trackCount: Int(clamping: trackCount))) * trackRowHeight
    }

    /// The table, and the "and N more…" line when `AlbumPanel.showsMore`.
    static func tracksHeight(trackCount: UInt64) -> CGFloat {
        tableHeight(trackCount: trackCount)
            + (AlbumPanel.showsMore(trackCount: Int(clamping: trackCount)) ? moreHeight : 0)
    }

    static func height(trackCount: UInt64) -> CGFloat {
        let text = fixedTextHeight + tracksHeight(trackCount: trackCount)
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

/// The panel's content: the cover, the title, artist and facts, the track table, and the
/// album actions.
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
        let dimension = AlbumExpansionMetrics.cover - (artwork.requiresAttribution ? 28 : 0)
        return VStack(spacing: 4) {
            ZStack {
                RoundedRectangle(cornerRadius: 6).fill(.quaternary)
                if let image = artwork.image {
                    Image(decorative: image, scale: 1).resizable().interpolation(.high).scaledToFit()
                } else {
                    Image(systemName: "music.note")
                        .font(.system(size: 40)).foregroundStyle(.secondary)
                }
            }
            .frame(width: dimension, height: dimension)
            .clipShape(RoundedRectangle(cornerRadius: 6))
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Color(nsColor: .separatorColor)))
            .accessibilityElement(children: .ignore)
            .accessibilityAddTraits(.isImage)
            .accessibilityLabel("Album artwork")
            .accessibilityValue(artwork.state)
            .accessibilityIdentifier("album-expansion-cover")
            if artwork.requiresAttribution {
                ArtworkAttributionView(artwork: artwork).frame(height: 24)
            }
        }
        .frame(width: AlbumExpansionMetrics.cover)
    }

    // MARK: Tracks

    /// Sized from the album's track count, as the grid sizes the item.
    @ViewBuilder private var tracks: some View {
        VStack(alignment: .leading, spacing: 0) {
            table
            if AlbumPanel.showsMore(trackCount: Int(clamping: album.track_count)) {
                Text(expansion.moreText ?? "").font(.system(size: 12)).foregroundStyle(.secondary)
                    .padding(.leading, 6)
                    .frame(height: AlbumExpansionMetrics.moreHeight, alignment: .bottomLeading)
                    .accessibilityIdentifier("album-expansion-more")
            }
        }
    }

    /// `album-expansion-tracks` in Accessibility: the table's scroll view, valued with the track
    /// count, or the placeholder while the tracks load or fail.
    @ViewBuilder private var table: some View {
        Group {
            if let failure = expansion.failure {
                Text(failure).font(.callout).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .accessibilityLabel("Tracks")
                    .accessibilityValue(failure)
                    .accessibilityIdentifier(AlbumTrackTable.tracksIdentifier)
            } else if !expansion.isLoaded {
                ProgressView().controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .accessibilityLabel("Tracks")
                    .accessibilityValue("loading")
                    .accessibilityIdentifier(AlbumTrackTable.tracksIdentifier)
            } else {
                AlbumTrackTable(expansion: expansion, grid: grid, player: player,
                                tracks: expansion.tracks, selected: expansion.selectedRowIDs, total: expansion.total)
            }
        }
        .frame(height: AlbumExpansionMetrics.tableHeight(trackCount: album.track_count))
    }

    // MARK: Actions

    /// Each acts on the selected tracks, or the whole album when none are selected.
    private var actions: some View {
        HStack(spacing: 8) {
            Button { grid.addToUpNext(expansion) } label: { Label("Add to Up Next", systemImage: "text.append") }
                .accessibilityIdentifier("album-add-up-next")
            Menu("Add to Playlist…") {
                let playlists = grid.regularPlaylists
                if playlists.isEmpty {
                    Text("No Playlists")
                } else {
                    ForEach(playlists) { playlist in
                        Button(playlist.name) { grid.addToPlaylist(expansion, playlist: playlist) }
                    }
                }
            }
            .fixedSize()
            .accessibilityIdentifier("album-add-playlist")
        }
        .controlSize(.small)
        .disabled(!player.canMutate)
        .onAppear { player.savedPlaylists.loadAll() }
    }
}
