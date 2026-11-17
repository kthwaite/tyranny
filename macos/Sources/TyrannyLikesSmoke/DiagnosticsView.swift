import AppKit
import SwiftUI
import TyrannyLikesSmokeCore

@MainActor
struct DiagnosticsView: View {
    let model: PlayerModel
    @State private var visible = false
    @State private var daemonTail = "Waiting to read the managed server log…"
    @State private var daemonReadAt: Date?
    @State private var daemonAvailable = false
    @State private var copyStatus = ""

    private var sessionText: String {
        model.diagnostics.entries.isEmpty ? "No session events." : model.diagnostics.text
    }

    private let retention = "Session only: up to 500 events, normally 1 MiB. The latest error is kept in full; informational events are capped at 64 KiB."

    private var context: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "development"
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "unbundled"
        return DiagnosticsLog.sanitize("""
        tyranny likes smoke — Diagnostics
        App: \(version) (\(build)); protocol: \(WireProtocol.version)
        macOS: \(ProcessInfo.processInfo.operatingSystemVersionString)
        Connection: \(model.connection)
        Detail: \(model.connectionDetail)
        Socket: \(model.socketPath)
        Server instance: \(model.serverInstance ?? "unavailable")
        Server management: \(model.managesServer ? "managed" : "external / unmanaged")
        Daemon log: \(model.managedLogPath ?? "unavailable for this socket")
        """)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(context)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
                .accessibilityIdentifier("diagnostics-context")
            Text("\(retention) Common credentials are redacted; review before sharing.")
                .font(.caption)
                .foregroundStyle(.secondary)
            VSplitView {
                logSection("App events", text: sessionText, identifier: "diagnostics-session-log")
                VStack(alignment: .leading, spacing: 6) {
                    Text(daemonReadAt.map { "Managed server.log — read \($0.formatted(date: .abbreviated, time: .standard))" }
                         ?? "Managed server.log")
                        .font(.headline)
                    Text("Latest 256 KiB; refreshed every 2 seconds only while this window is visible. Clearing app events never changes this file.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    logText(daemonTail, identifier: "diagnostics-daemon-log")
                }
                .padding(.top, 8)
                .frame(minHeight: 130)
            }
            HStack {
                Button("Copy Diagnostics") { copyDiagnostics() }
                    .accessibilityIdentifier("diagnostics-copy")
                Button("Reveal Log File") {
                    guard let path = model.managedLogPath else { return }
                    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
                }
                .disabled(!daemonAvailable)
                .accessibilityIdentifier("diagnostics-reveal-log")
                Spacer()
                Text(copyStatus).font(.caption).foregroundStyle(.secondary)
                    .accessibilityIdentifier("diagnostics-copy-status")
                Button("Clear Session Events") { model.diagnostics.clear(); copyStatus = "" }
                    .accessibilityIdentifier("diagnostics-clear-session")
            }
        }
        .padding(16)
        .frame(minWidth: 640, minHeight: 500)
        .background(DiagnosticsVisibilityProbe { visible = $0 })
        .task(id: visible ? model.managedLogPath ?? "unmanaged" : nil) {
            guard visible else { return }
            repeat {
                await refreshDaemonTail()
                do { try await Task.sleep(for: .seconds(2)) } catch { return }
            } while !Task.isCancelled
        }
        .onDisappear { visible = false }
    }

    private func logSection(_ title: String, text: String, identifier: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline)
            logText(text, identifier: identifier)
        }
        .padding(.bottom, 8)
        .frame(minHeight: 130)
    }

    private func logText(_ text: String, identifier: String) -> some View {
        ScrollView([.horizontal, .vertical]) {
            Text(text)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
                .fixedSize(horizontal: true, vertical: true)
                .frame(maxWidth: .infinity, alignment: .topLeading)
                .padding(8)
                .accessibilityIdentifier(identifier)
        }
        .background(.background)
        .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color(nsColor: .separatorColor)))
    }

    private func refreshDaemonTail() async {
        guard visible, !Task.isCancelled else { return }
        guard let path = model.managedLogPath else {
            daemonTail = "No managed daemon log is available for this socket. An external server’s default log is not read."
            daemonAvailable = false
            daemonReadAt = nil
            return
        }
        let read = Task.detached(priority: .utility) { () -> (String, Bool) in
            guard !Task.isCancelled else { return ("", false) }
            do {
                let tail = try DiagnosticsLogFile.tail(atPath: path)
                return (tail.isEmpty ? "The managed server log is empty." : tail, true)
            } catch {
                return (DiagnosticsLog.sanitize("Managed server log unavailable:\n\(error.localizedDescription)"), false)
            }
        }
        let result = await withTaskCancellationHandler {
            await read.value
        } onCancel: {
            read.cancel()
        }
        guard !Task.isCancelled, visible, model.managedLogPath == path else { return }
        daemonTail = result.0
        daemonAvailable = result.1
        daemonReadAt = Date()
    }

    private func copyDiagnostics() {
        let report = """
        \(context)
        Captured: \(Date().formatted(.iso8601))
        \(retention)
        Daemon tail: latest 256 KiB. Common credentials redacted; review before sharing.

        === App events ===
        \(sessionText)

        === Managed server.log ===
        \(daemonTail)
        """
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        copyStatus = pasteboard.setString(report, forType: .string) ? "Copied" : "Copy failed"
    }
}

/// SwiftUI can retain a closed Window scene. Observe native visibility so retained views
/// never keep polling the disk while closed, minimized, hidden or fully occluded.
@MainActor
private struct DiagnosticsVisibilityProbe: NSViewRepresentable {
    let changed: (Bool) -> Void

    func makeNSView(context: Context) -> ProbeView { ProbeView(changed: changed) }
    func updateNSView(_ nsView: ProbeView, context: Context) { nsView.changed = changed }

    final class ProbeView: NSView {
        var changed: (Bool) -> Void
        private var observers: [NSObjectProtocol] = []

        init(changed: @escaping (Bool) -> Void) {
            self.changed = changed
            super.init(frame: .zero)
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            for observer in observers { NotificationCenter.default.removeObserver(observer) }
            observers.removeAll()
            guard let window else { changed(false); return }
            for name in [NSWindow.didChangeOcclusionStateNotification, NSWindow.didMiniaturizeNotification,
                         NSWindow.didDeminiaturizeNotification, NSWindow.willCloseNotification] {
                observers.append(NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) { [weak self] note in
                    let closing = note.name == NSWindow.willCloseNotification
                    MainActor.assumeIsolated {
                        guard let self else { return }
                        if closing { self.changed(false) }
                        else { self.updateVisibility() }
                    }
                })
            }
            DispatchQueue.main.async { [weak self] in self?.updateVisibility() }
        }

        private func updateVisibility() {
            changed(window.map { $0.isVisible && !$0.isMiniaturized && $0.occlusionState.contains(.visible) } ?? false)
        }

        isolated deinit {
            for observer in observers { NotificationCenter.default.removeObserver(observer) }
        }
    }
}
