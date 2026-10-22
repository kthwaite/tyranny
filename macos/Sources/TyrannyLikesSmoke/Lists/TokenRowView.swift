import AppKit
import TyrannyLikesSmokeCore

/// A cell that shows the playing indicator. Its row calls it when selection or emphasis changes.
@MainActor protocol PlayingIndicatorCell: NSView {
    func rowChanged(selected: Bool, style: NSView.BackgroundStyle)
}

/// A table row that draws a custom selection colour. With no custom colour it is a stock row.
///
/// A custom selection fills at full opacity while the table is the key view, and at 0.45
/// otherwise. The cells' text colour follows the fill: light text on a dark fill.
class TokenRowView: NSTableRowView {
    private static let unfocusedOpacity = 0.45

    /// nil: "System", the default row selection.
    var selection: RGB? {
        didSet {
            guard selection != oldValue else { return }
            needsDisplay = true
            for case let cell as NSTableCellView in subviews { cell.backgroundStyle = interiorBackgroundStyle }
            notifyIndicators()
        }
    }

    override var isSelected: Bool {
        didSet { if isSelected != oldValue { notifyIndicators() } }
    }

    override var isEmphasized: Bool {
        didSet { if isEmphasized != oldValue { notifyIndicators() } }
    }

    override func drawSelection(in dirtyRect: NSRect) {
        guard let selection else {
            super.drawSelection(in: dirtyRect)
            return
        }
        NSColor(srgbRed: selection.red, green: selection.green, blue: selection.blue,
                alpha: opacity).setFill()
        bounds.fill()
    }

    override var interiorBackgroundStyle: NSView.BackgroundStyle {
        guard let selection, isSelected else { return super.interiorBackgroundStyle }
        let dark = effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        let fill = Contrast.composite(
            selection, opacity: opacity, over: dark ? Contrast.darkBackground : Contrast.lightBackground)
        return Contrast.prefersLightText(on: fill) ? .emphasized : .normal
    }

    override func didAddSubview(_ subview: NSView) {
        super.didAddSubview(subview)
        (subview as? PlayingIndicatorCell)?.rowChanged(selected: isSelected, style: interiorBackgroundStyle)
    }

    private var opacity: Double { isEmphasized ? 1 : Self.unfocusedOpacity }

    private func notifyIndicators() {
        let style = interiorBackgroundStyle
        for case let cell as PlayingIndicatorCell in subviews { cell.rowChanged(selected: isSelected, style: style) }
    }
}

extension NSImageView {
    /// The playing icon's tint: the theme's colour, or text colour on a selected row.
    func tintPlayingIcon(_ playing: NSColor?, selected: Bool, style: NSView.BackgroundStyle) {
        contentTintColor = selected
            ? (style == .emphasized ? .alternateSelectedControlTextColor : .labelColor) : playing
    }
}
