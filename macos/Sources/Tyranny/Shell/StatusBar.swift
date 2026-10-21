import SwiftUI
import TyrannyCore

/// The one-line status bar: connection on the left, totals or a brief operation
/// message in the centre, scan progress on the right.
struct StatusBar: View {
    @Environment(\.themeColours) private var colours
    let model: PlayerModel
    /// The current view's totals, and the unfiltered count when it is filtered.
    /// Nil shows the library totals.
    var totals: ListTotals?
    var filteredFrom: UInt64?
    let openConnectionSettings: () -> Void
    /// The operation message on show; cleared from `model.message` after 4 s.
    @State private var shownMessage: String?

    var body: some View {
        ZStack {
            Text(centreText).lineLimit(1).truncationMode(.middle)
                .foregroundStyle(shownMessage == nil ? .secondary : .primary)
                .frame(maxWidth: 520)
                .help(shownMessage ?? "")
                .accessibilityIdentifier("status-totals")
            HStack(spacing: 8) {
                connection
                Spacer(minLength: 12)
                scan
            }
        }
        .font(.system(size: 11))
        .padding(.horizontal, 10)
        .frame(height: 24)
        .frame(maxWidth: .infinity)
        .background(.bar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("status-bar")
        .task(id: model.message) {
            guard let message = model.message else { shownMessage = nil; return }
            shownMessage = message
            do { try await Task.sleep(for: .seconds(4)) } catch { return }
            if model.message == message { model.message = nil }
        }
    }

    private var connection: some View {
        Button(action: openConnectionSettings) {
            HStack(spacing: 5) {
                Circle().fill(dotColour).frame(width: 7, height: 7)
                Text(connectionLabel).lineLimit(1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .help(model.connectionDetail)
        .accessibilityLabel(connectionLabel)
        .accessibilityIdentifier("status-connection")
    }

    @ViewBuilder private var scan: some View {
        if let state = model.snapshot?.library.scan,
           let label = DisplayFormat.scanLabel(state, root: rootPath(state.root_id)) {
            HStack(spacing: 6) {
                Text(label).lineLimit(1).truncationMode(.middle)
                if let total = state.total, total > 0, state.phase == "indexing" {
                    ProgressView(value: Double(min(state.processed, total)), total: Double(total))
                        .progressViewStyle(.linear).frame(width: 80)
                } else {
                    ProgressView().controlSize(.mini)
                }
            }
            .foregroundStyle(.secondary)
            .accessibilityElement(children: .combine)
            .accessibilityLabel(label)
            .accessibilityIdentifier("status-scan")
        }
    }

    private var connectionLabel: String {
        switch model.connection {
        case "connected": return "Server · local · protocol v\(WireProtocol.version)"
        case "connecting": return "Connecting…"
        default: return model.startupFailed ? "Server unavailable" : "Disconnected"
        }
    }

    private var dotColour: Color {
        switch model.connection {
        case "connected": colours.success
        case "connecting": .yellow
        default: colours.error
        }
    }

    private var centreText: String {
        if let shownMessage { return shownMessage }
        guard model.connected, let library = model.snapshot?.library else { return "" }
        if let totals { return DisplayFormat.totals(totals, of: filteredFrom) }
        return DisplayFormat.totals(ListTotals(tracks: UInt64(library.tracks), duration_ms: library.duration_ms,
                                               size_bytes: library.size_bytes))
    }

    private func rootPath(_ id: UInt64?) -> String? {
        guard let id, let path = model.roots?.entries.first(where: { $0.id == id })?.path else { return nil }
        return DisplayFormat.abbreviatedPath(path, home: NSHomeDirectory())
    }
}
