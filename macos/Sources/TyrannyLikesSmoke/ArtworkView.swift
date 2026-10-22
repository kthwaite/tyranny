import SwiftUI
import TyrannyLikesSmokeCore

struct ArtworkView: View {
    let store: ArtworkStore
    let request: ArtworkRequest?
    let identifier: String
    let dimension: CGFloat
    var tracksVisibility = false
    @State private var visible = false
    @State private var retained: ArtworkRequest?

    private var artwork: ArtworkImage { store.value(for: request) }
    private var placeholder: String {
        switch artwork.state {
        case "loading": return "Loading artwork"
        case "unavailable": return "Artwork unavailable"
        default: return "No artwork"
        }
    }

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 5).fill(.quaternary)
            if let image = artwork.image {
                Image(decorative: image, scale: 1)
                    .resizable().scaledToFit()
            } else {
                VStack(spacing: 5) {
                    // Loading and missing art share the neutral note, so nothing flashes.
                    Image(systemName: "music.note")
                        .font(.system(size: dimension >= 100 ? 28 : 18))
                    if dimension >= 100 {
                        Text(placeholder).font(.caption2).multilineTextAlignment(.center)
                    }
                }
                .foregroundStyle(.secondary).padding(5)
            }
        }
        .frame(width: dimension, height: dimension)
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .accessibilityElement(children: .ignore)
        .accessibilityAddTraits(.isImage)
        .accessibilityLabel(identifier.hasPrefix("now-playing-art") ? "Now Playing artwork" : "Album artwork")
        .accessibilityValue(artwork.state)
        .accessibilityIdentifier(identifier)
        .help(artwork.image == nil ? placeholder : "Artwork: \(artwork.state.replacingOccurrences(of: "_", with: " "))")
        .onAppear {
            visible = true
            if tracksVisibility { updateDemand(request) }
        }
        .onChange(of: request) { _, next in
            if visible, tracksVisibility { updateDemand(next) }
        }
        .onDisappear {
            visible = false
            updateDemand(nil)
        }
    }

    private func updateDemand(_ next: ArtworkRequest?) {
        guard retained != next else { return }
        if let retained { store.release(retained) }
        retained = next
        if let next { store.retain(next) }
    }
}
