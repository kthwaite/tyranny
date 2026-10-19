import AppKit
import TyrannyCore

/// The full-width header row of an artist group (spec 4.3): the album artist's name in
/// semibold 13 pt, then "n albums · m tracks" in secondary text. 28 pt high.
final class ArtistHeaderCell: NSTableCellView {
    static let reuse = NSUserInterfaceItemIdentifier("artist-header")
    static let height: CGFloat = 28

    private let name = NSTextField(labelWithString: "")
    private let counts = NSTextField(labelWithString: "")

    init() {
        super.init(frame: .zero)
        identifier = Self.reuse
        name.font = .systemFont(ofSize: 13, weight: .semibold)
        name.lineBreakMode = .byTruncatingTail
        name.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        counts.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        counts.textColor = .secondaryLabelColor
        counts.setContentCompressionResistancePriority(.required, for: .horizontal)
        for field in [name, counts] {
            field.translatesAutoresizingMaskIntoConstraints = false
            field.cell?.usesSingleLineMode = true
            addSubview(field)
        }
        textField = name
        NSLayoutConstraint.activate([
            name.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            name.lastBaselineAnchor.constraint(equalTo: bottomAnchor, constant: -8),
            counts.leadingAnchor.constraint(equalTo: name.trailingAnchor, constant: 10),
            counts.firstBaselineAnchor.constraint(equalTo: name.firstBaselineAnchor),
            counts.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -14),
        ])
        setAccessibilityElement(true)
        setAccessibilityRole(.staticText)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// `artist` is nil until the group's first row loads.
    func show(artistKey: UInt64, artist: String?, albums: Int?, tracks: Int) {
        let title = artistKey == 0 ? "Compilations" : artist.map { $0.isEmpty ? "Unknown Artist" : $0 } ?? ""
        name.stringValue = title
        counts.stringValue = DisplayFormat.artistHeaderCounts(albums: albums, tracks: tracks)
        setAccessibilityIdentifier("artist-header-\(artistKey)")
        setAccessibilityLabel(title)
        setAccessibilityValue(counts.stringValue)
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        NSColor.separatorColor.setFill()
        // The bottom edge: the view is not flipped.
        NSRect(x: 0, y: 0, width: bounds.width, height: 1).fill()
    }
}
