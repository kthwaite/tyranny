import AppKit
import TyrannyCore

/// One row's slice of an album block's gutter column (spec 4.3).
///
/// The gutter shows the cover (104 pt) from block row 0, overflowing downward, and the
/// facts lines from block row 5 (title; year · genre; tracks · length; format). The four lines
/// need about 50 pt below the cover, so they end at block row 7; blocks pad to
/// `GroupLayout.minimumBlockRows` (8), which keeps the gap row after a block clear. Sibling row
/// views do not keep a reliable drawing order, so content drawn by row 0 past its bottom could
/// be painted over by the next row's background. Instead every block row's gutter cell draws
/// the whole gutter shifted up by its block offset and clipped to its own 20 pt. The slices
/// line up into one picture. The gutter paints the table background, so neither the zebra nor
/// the selection highlight runs under the cover.
final class AlbumBlockCell: NSTableCellView {
    static let reuse = NSUserInterfaceItemIdentifier("album-block")
    static let columnID = "album-gutter"
    static let columnWidth: CGFloat = 120
    static let coverSize: CGFloat = 104
    /// Block rows with gutter content: the cover and the facts end within these rows.
    static let contentRows = GroupLayout.minimumBlockRows
    private static let coverTop: CGFloat = 2
    private static let factsTop: CGFloat = 110
    private static let inset: CGFloat = 8

    private var slice = 0
    private var rowHeight: CGFloat = 20
    private var artwork: ArtworkImage = .missing
    private var lines: [String] = []

    init() {
        super.init(frame: .zero)
        identifier = Self.reuse
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override var isFlipped: Bool { true }

    /// `slice` is the row's offset within its block. `facts` is empty until the summary loads.
    func show(albumKey: UInt64?, slice: Int, rowHeight: CGFloat, artwork: ArtworkImage, facts: [String]) {
        self.slice = slice
        self.rowHeight = rowHeight
        self.artwork = artwork
        lines = facts
        needsDisplay = true
        let coverRow = slice == 0, factsRow = slice == 5
        let attributionRow = artwork.requiresAttribution && artwork.image != nil && slice == 4
        setAccessibilityElement(albumKey != nil && (coverRow || factsRow || attributionRow))
        if attributionRow {
            setAccessibilityRole(.link)
            setAccessibilityLabel(artwork.state == "discogs" ? "Data provided by Discogs." : "Artwork from Deezer")
            setAccessibilityIdentifier("album-artwork-provider-\(albumKey ?? 0)")
        } else if let albumKey, coverRow {
            setAccessibilityRole(.image)
            setAccessibilityLabel("Album artwork")
            setAccessibilityValue(artwork.state)
            setAccessibilityIdentifier("album-cover-\(albumKey)")
        } else if let albumKey, factsRow {
            setAccessibilityRole(.staticText)
            setAccessibilityLabel(facts.first ?? "")
            setAccessibilityValue(facts.joined(separator: "\n"))
            setAccessibilityIdentifier("album-facts-\(albumKey)")
        } else {
            setAccessibilityIdentifier(nil)
        }
        toolTip = factsRow || coverRow ? (facts.isEmpty ? nil : facts.joined(separator: "\n")) : nil
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.controlBackgroundColor.setFill()
        bounds.fill()
        guard slice < Self.contentRows else { return }
        let shift = -CGFloat(slice) * rowHeight
        let size = artwork.requiresAttribution ? Self.coverSize - 28 : Self.coverSize
        let cover = NSRect(x: Self.inset + (Self.coverSize - size) / 2, y: Self.coverTop + shift, width: size, height: size)
        if cover.intersects(bounds) { drawCover(in: cover) }
        if artwork.requiresAttribution, artwork.image != nil {
            let caption = NSRect(x: Self.inset, y: Self.coverTop + Self.coverSize - 28 + shift,
                                 width: Self.coverSize, height: 28)
            if caption.intersects(bounds) {
                if artwork.state == "deezer", let logo = ArtworkBrand.deezerLogo {
                    logo.draw(in: NSRect(x: caption.minX + 18, y: caption.minY + 4, width: 20, height: 20),
                              from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
                    ("Deezer" as NSString).draw(in: NSRect(x: caption.minX + 41, y: caption.minY + 7, width: 50, height: 14),
                                               withAttributes: Self.attributionAttributes)
                } else {
                    ("Data provided by Discogs." as NSString).draw(with: caption, options: [.usesLineFragmentOrigin],
                                                                 attributes: Self.attributionAttributes)
                }
            }
        }
        var y = Self.factsTop + shift
        for (index, line) in lines.enumerated() {
            let title = index == 0
            let height: CGFloat = title ? 14 : 12
            let rect = NSRect(x: Self.inset, y: y, width: Self.coverSize, height: height)
            if rect.intersects(bounds) {
                (line as NSString).draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine],
                                        attributes: title ? Self.titleAttributes : Self.lineAttributes)
            }
            y += height
        }
    }

    private func drawCover(in rect: NSRect) {
        NSGraphicsContext.saveGraphicsState()
        let shape = NSBezierPath(roundedRect: rect, xRadius: 4, yRadius: 4)
        shape.addClip()
        if let image = artwork.image {
            NSImage(cgImage: image, size: rect.size)
                .draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
        } else {
            NSColor.quaternaryLabelColor.setFill()
            rect.fill()
            let configuration = NSImage.SymbolConfiguration(pointSize: 28, weight: .regular)
                .applying(.init(paletteColors: [.secondaryLabelColor]))
            if let symbol = NSImage(systemSymbolName: "music.note",
                                    accessibilityDescription: nil)?.withSymbolConfiguration(configuration) {
                let size = symbol.size
                symbol.draw(in: NSRect(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2,
                                       width: size.width, height: size.height),
                            from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
            }
        }
        NSGraphicsContext.restoreGraphicsState()
        NSColor.separatorColor.setStroke()
        let border = NSBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), xRadius: 4, yRadius: 4)
        border.lineWidth = 1
        border.stroke()
    }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let y = point.y + CGFloat(slice) * rowHeight
        if artwork.requiresAttribution, artwork.image != nil,
           y >= Self.coverTop + Self.coverSize - 28, y <= Self.coverTop + Self.coverSize,
           let url = artwork.sourceURL {
            NSWorkspace.shared.open(url)
        } else { super.mouseDown(with: event) }
    }

    override func accessibilityPerformPress() -> Bool {
        guard artwork.requiresAttribution, artwork.image != nil, let url = artwork.sourceURL else { return false }
        return NSWorkspace.shared.open(url)
    }

    private static let attributionAttributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 10), .foregroundColor: NSColor.linkColor]

    private static let truncating: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return style
    }()
    private static let titleAttributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 11, weight: .semibold),
        .foregroundColor: NSColor.labelColor, .paragraphStyle: truncating]
    private static let lineAttributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .regular),
        .foregroundColor: NSColor.secondaryLabelColor, .paragraphStyle: truncating]
}
