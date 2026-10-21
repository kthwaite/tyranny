import AppKit
import TyrannyCore

/// A momentary action: the heart changes only when an authoritative row is reloaded.
final class LikedTrackCell: NSTableCellView {
    static let reuse = NSUserInterfaceItemIdentifier("liked-track-cell")
    private static let outline = NSImage(systemSymbolName: "heart", accessibilityDescription: nil)
    private static let filled = NSImage(systemSymbolName: "heart.fill", accessibilityDescription: nil)
    let button = NSButton()
    private var action: (() -> Void)?

    init() {
        super.init(frame: .zero)
        identifier = Self.reuse
        button.setButtonType(.momentaryChange)
        button.bezelStyle = .inline
        button.isBordered = false
        button.imagePosition = .imageOnly
        button.title = ""
        button.target = self
        button.action = #selector(pressed)
        button.translatesAutoresizingMaskIntoConstraints = false
        addSubview(button)
        NSLayoutConstraint.activate([
            button.leadingAnchor.constraint(equalTo: leadingAnchor),
            button.trailingAnchor.constraint(equalTo: trailingAnchor),
            button.topAnchor.constraint(equalTo: topAnchor),
            button.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func show(_ track: TrackSummary, enabled: Bool, identifier: String, action: @escaping () -> Void) {
        self.action = action
        button.image = track.liked ? Self.filled : Self.outline
        button.contentTintColor = track.liked ? .systemPink : .secondaryLabelColor
        button.isEnabled = enabled
        button.toolTip = track.liked ? "Unlike \(track.title)" : "Like \(track.title)"
        button.setAccessibilityIdentifier(identifier)
        button.setAccessibilityLabel(button.toolTip)
        button.setAccessibilityValue(track.liked ? "Liked" : "Not liked")
    }

    @objc private func pressed() { action?() }
}
