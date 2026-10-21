import AppKit
import SwiftUI

@MainActor
enum ArtworkBrand {
    // Unmodified heart logo from the official deezerbrand.com brand site's icon resource.
    static let deezerLogo: NSImage? = {
        guard let url = Bundle.main.url(forResource: "DeezerLogo", withExtension: "png"),
              let image = NSImage(contentsOf: url) else { return nil }
        image.size = NSSize(width: 20, height: 20)
        return image
    }()
}

struct ArtworkAttributionView: View {
    let artwork: ArtworkImage

    var body: some View {
        if artwork.image != nil, let url = artwork.sourceURL {
            if artwork.state == "discogs" {
                Link("Data provided by Discogs.", destination: url)
                    .font(.system(size: 10))
                    .accessibilityIdentifier("artwork-discogs-attribution")
            } else if artwork.state == "deezer", let logo = ArtworkBrand.deezerLogo {
                Link(destination: url) {
                    HStack(spacing: 3) {
                        Image(nsImage: logo).resizable().scaledToFit().frame(width: 20, height: 20)
                        Text("Deezer").font(.system(size: 10))
                    }
                }
                .accessibilityLabel("Artwork from Deezer")
                .accessibilityIdentifier("artwork-deezer-attribution")
            }
        }
    }
}

/// A real link adjacent to the image, never an overlay on the cover.
@MainActor
final class ArtworkAttributionButton: NSButton {
    private var destination: URL?

    init() {
        super.init(frame: .zero)
        isBordered = false
        font = .systemFont(ofSize: 10)
        contentTintColor = .linkColor
        imagePosition = .imageLeft
        imageScaling = .scaleProportionallyDown
        target = self
        action = #selector(openSource)
        cell?.wraps = true
        cell?.lineBreakMode = .byWordWrapping
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func show(_ artwork: ArtworkImage) {
        destination = artwork.image == nil ? nil : artwork.sourceURL
        isHidden = !artwork.requiresAttribution || destination == nil
        title = artwork.state == "discogs" ? "Data provided by Discogs." : "Deezer"
        image = artwork.state == "deezer" ? ArtworkBrand.deezerLogo : nil
        setAccessibilityLabel(title)
        setAccessibilityRole(.link)
        setAccessibilityIdentifier("artwork-\(artwork.state)-attribution")
    }

    @objc private func openSource() {
        if let destination { NSWorkspace.shared.open(destination) }
    }
}
