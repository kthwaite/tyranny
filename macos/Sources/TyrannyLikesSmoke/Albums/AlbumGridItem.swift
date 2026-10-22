import AppKit
import Observation
import TyrannyLikesSmokeCore

/// One album in the grid (spec 4.4): the cover, then the title, then "artist · year".
///
/// The item retains its cover's `ArtworkRequest` while it is displayed. It releases it when
/// it shows another album, when the collection view ends displaying it, and on dismantle.
final class AlbumGridItem: NSCollectionViewItem {
    static let reuse = NSUserInterfaceItemIdentifier("album-grid-item")

    private var cell: AlbumGridCellView { view as! AlbumGridCellView }
    private weak var store: ArtworkStore?
    private var retained: ArtworkRequest?
    /// Bumps per `show`; an artwork observation from an older show is ignored.
    private var showing: UInt64 = 0

    override func loadView() {
        view = AlbumGridCellView()
    }

    override var isSelected: Bool {
        didSet { cell.selected = isSelected }
    }

    /// `album` is nil for a row not loaded yet: a blank placeholder of the same size.
    func show(_ album: AlbumSummary?, position: Int, coverSize: CGFloat, store: ArtworkStore, retainArtwork: Bool,
              selectionColour: NSColor) {
        cell.selectionColour = selectionColour
        showing &+= 1
        self.store = store
        let request = retainArtwork ? album.flatMap { store.albumRequest($0.id) } : nil
        if request != retained {
            if let retained { store.release(retained) }
            retained = request
            if let request { store.retain(request) }
        }
        cell.show(album, position: position, coverSize: coverSize, artwork: store.value(for: request))
        if let request { observe(request, ticket: showing) }
    }

    /// Redraws the cover when its image arrives.
    private func observe(_ request: ArtworkRequest, ticket: UInt64) {
        guard let store else { return }
        withObservationTracking {
            _ = store.value(for: request)
        } onChange: { [weak self] in
            Task { @MainActor in
                guard let self, self.showing == ticket, self.retained == request, let store = self.store else { return }
                self.cell.showArtwork(store.value(for: request))
                self.observe(request, ticket: ticket)
            }
        }
    }

    /// Runs on an Accessibility press of the cell.
    var onPress: (() -> Void)? {
        get { cell.onPress }
        set { cell.onPress = newValue }
    }

    /// Runs on "Look Up on MusicBrainz…" in the cell's menu. Nil disables the item.
    var onLookUp: (() -> Void)? {
        get { cell.onLookUp }
        set { cell.onLookUp = newValue }
    }

    func releaseArtwork() {
        showing &+= 1
        if let retained { store?.release(retained) }
        retained = nil
        cell.showArtwork(.missing)
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        releaseArtwork()
    }
}

/// Draws the cover and the two text lines. The view is one Accessibility button,
/// `album-cell-<id>`, whose value is the title and "artist · year" on two lines.
final class AlbumGridCellView: NSView {
    private static let gap: CGFloat = 6
    private let title = NSTextField(labelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")
    private var coverSize: CGFloat = 140
    private var artwork: ArtworkImage = .missing
    private let attribution = ArtworkAttributionButton()
    private var hasAlbum = false
    var onLookUp: (() -> Void)?
    /// The selected cover's border. Set by `show`.
    var selectionColour = NSColor.clear { didSet { if selectionColour != oldValue { needsDisplay = true } } }
    var selected = false { didSet { if selected != oldValue { needsDisplay = true } } }

    init() {
        super.init(frame: .zero)
        for (field, font, color) in [(title, NSFont.systemFont(ofSize: 12, weight: .medium), NSColor.labelColor),
                                     (subtitle, NSFont.systemFont(ofSize: 11), NSColor.secondaryLabelColor)] {
            field.font = font
            field.textColor = color
            field.lineBreakMode = .byTruncatingTail
            field.cell?.usesSingleLineMode = true
            field.setAccessibilityElement(false)
            addSubview(field)
        }
        setAccessibilityElement(true)
        addSubview(attribution)
        setAccessibilityRole(.button)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override var isFlipped: Bool { true }

    func show(_ album: AlbumSummary?, position: Int, coverSize: CGFloat, artwork: ArtworkImage) {
        self.coverSize = coverSize
        hasAlbum = album != nil
        title.stringValue = album.map { $0.title.isEmpty ? "Unknown Album" : $0.title } ?? ""
        subtitle.stringValue = album.map(DisplayFormat.albumSubtitle) ?? ""
        toolTip = album == nil ? nil : "\(title.stringValue)\n\(subtitle.stringValue)"
        if let album {
            setAccessibilityIdentifier("album-cell-\(album.id)")
            setAccessibilityLabel(title.stringValue)
            setAccessibilityValue("\(title.stringValue)\n\(subtitle.stringValue)")
        } else {
            setAccessibilityIdentifier("album-placeholder-\(position)")
            setAccessibilityLabel("Loading")
            setAccessibilityValue(nil)
        }
        showArtwork(artwork)
        needsLayout = true
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        guard hasAlbum else { return nil }
        let menu = NSMenu(title: "Album")
        let item = NSMenuItem(title: "Look Up on MusicBrainz…", action: onLookUp == nil ? nil : #selector(lookUp), keyEquivalent: "")
        item.target = self
        item.setAccessibilityIdentifier("album-menu-musicbrainz")
        menu.addItem(item)
        return menu
    }

    @objc private func lookUp() { onLookUp?() }

    func showArtwork(_ artwork: ArtworkImage) {
        self.artwork = artwork
        attribution.show(artwork)
        needsLayout = true
        needsDisplay = true
    }

    override func layout() {
        super.layout()
        let top = coverSize + Self.gap
        title.frame = NSRect(x: 0, y: top, width: bounds.width, height: 16)
        subtitle.frame = NSRect(x: 0, y: top + 17, width: bounds.width, height: 15)
        attribution.frame = NSRect(x: 0, y: coverSize - 28, width: bounds.width, height: 28)
    }

    override func draw(_ dirtyRect: NSRect) {
        let imageSize = artwork.requiresAttribution ? coverSize - 28 : coverSize
        let rect = NSRect(x: (coverSize - imageSize) / 2, y: 0, width: imageSize, height: imageSize)
        guard hasAlbum else {
            NSColor.quaternaryLabelColor.withAlphaComponent(0.08).setFill()
            NSBezierPath(roundedRect: rect, xRadius: 4, yRadius: 4).fill()
            return
        }
        NSGraphicsContext.saveGraphicsState()
        NSBezierPath(roundedRect: rect, xRadius: 4, yRadius: 4).addClip()
        if let image = artwork.image {
            NSImage(cgImage: image, size: rect.size)
                .draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
        } else {
            NSColor.quaternaryLabelColor.setFill()
            rect.fill()
            let configuration = NSImage.SymbolConfiguration(pointSize: coverSize / 4, weight: .regular)
                .applying(.init(paletteColors: [.secondaryLabelColor]))
            // Loading and missing art share the neutral note, so nothing flashes as art arrives.
            if let symbol = NSImage(systemSymbolName: "music.note",
                                    accessibilityDescription: nil)?.withSymbolConfiguration(configuration) {
                let size = symbol.size
                symbol.draw(in: NSRect(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2,
                                       width: size.width, height: size.height),
                            from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
            }
        }
        NSGraphicsContext.restoreGraphicsState()
        let border = NSBezierPath(roundedRect: rect.insetBy(dx: selected ? 1.5 : 0.5, dy: selected ? 1.5 : 0.5),
                                  xRadius: 4, yRadius: 4)
        border.lineWidth = selected ? 3 : 1
        (selected ? selectionColour : NSColor.separatorColor).setStroke()
        border.stroke()
    }

    /// AX press opens the panel, as a click does.
    var onPress: (() -> Void)?
    override func accessibilityPerformPress() -> Bool {
        guard let onPress else { return false }
        onPress()
        return true
    }
}
