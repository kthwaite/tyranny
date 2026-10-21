import AppKit
import SwiftUI
import TyrannyCore

enum TrackDragSource: Equatable {
    case library
    case queue
    case playlist(UInt64)
}

struct TrackDrag {
    static let type = "org.tyranny.track-occurrences"
    let token: String
    let connection: UInt64
    let source: TrackDragSource
    let ids: [UInt64]
    let positions: [Int]
    let revision: UInt64
    let libraryRevision: UInt64
    /// A track-list drag: the rows as a selection the server resolves. `ids` is then empty.
    var selection: TrackSelection?
    /// The selection's row count and the list it came from, for the drop's source label.
    var count = 0
    var label = ""
    /// The track IDs of the dragged rows, when all were loaded and there are at most 500.
    var trackIDs: [UInt64]?
    /// Captured only when every selected row is loaded and locally available.
    var paths: [String]?
}

/// Public native destinations also work in toolbar items and during AppKit tracking.
private struct TrackDragHover: ViewModifier {
    @Environment(\.themeColours) private var colours
    let model: PlayerModel
    let enabled: Bool
    let destination: TrackDragSource?
    let hover: () -> Void
    let drop: ((String) -> Bool)?
    @State private var targeted = false

    func body(content: Content) -> some View {
        content
            .background(targeted ? colours.accent.opacity(0.2) : Color.clear)
            .overlay(Region(model: model, enabled: enabled, destination: destination,
                hover: hover, drop: drop, targeted: $targeted))
    }

    private struct Region: NSViewRepresentable {
        let model: PlayerModel
        let enabled: Bool
        let destination: TrackDragSource?
        let hover: () -> Void
        let drop: ((String) -> Bool)?
        @Binding var targeted: Bool
        func makeNSView(context: Context) -> Destination {
            let view = Destination()
            view.registerForDraggedTypes([NSPasteboard.PasteboardType(TrackDrag.type)])
            view.region = self
            return view
        }
        func updateNSView(_ view: Destination, context: Context) { view.region = self }
        static func dismantleNSView(_ view: Destination, coordinator: ()) {
            // SwiftUI may be invalidating this binding's graph during teardown.
            view.region = nil
            view.stop()
        }

        final class Destination: NSView {
            var region: Region?
            private var timer: Timer?
            override func hitTest(_ point: NSPoint) -> NSView? { nil }
            private func token(_ sender: any NSDraggingInfo) -> String? {
                guard let region, region.enabled,
                      let token = sender.draggingPasteboard.string(forType: NSPasteboard.PasteboardType(TrackDrag.type)),
                      region.model.validateTrackDrop(token: token, destination: region.destination) else { return nil }
                return token
            }
            override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
                guard token(sender) != nil, let region else { return [] }
                region.targeted = true
                timer?.invalidate()
                let timer = Timer(timeInterval: 0.65, repeats: false) { [weak self] _ in
                    MainActor.assumeIsolated {
                        guard let self, let region = self.region, region.enabled,
                              region.model.validateTrackHover(destination: region.destination) else { return }
                        region.hover()
                    }
                }
                self.timer = timer
                RunLoop.main.add(timer, forMode: .common)
                return region.model.trackDragIsCopy ? .copy : .move
            }
            override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
                guard token(sender) != nil, let region else { return [] }
                return region.model.trackDragIsCopy ? .copy : .move
            }
            override func draggingExited(_ sender: (any NSDraggingInfo)?) { stop() }
            override func prepareForDragOperation(_ sender: any NSDraggingInfo) -> Bool {
                token(sender) != nil && region?.drop != nil
            }
            override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
                defer { stop() }
                guard let token = token(sender), let drop = region?.drop else { return false }
                return drop(token)
            }
            func stop() {
                timer?.invalidate(); timer = nil
                region?.targeted = false
            }
            isolated deinit { timer?.invalidate() }
        }
    }
}

extension View {
    func trackDragHover(model: PlayerModel, enabled: Bool = true, destination: TrackDragSource? = nil,
                        hover: @escaping () -> Void, drop: ((String) -> Bool)? = nil) -> some View {
        modifier(TrackDragHover(model: model, enabled: enabled, destination: destination, hover: hover, drop: drop))
    }
}
