import AppKit
import SwiftUI

@MainActor
final class TyrannyAppDelegate: NSObject, NSApplicationDelegate {
    var model: PlayerModel?
    var reopenWindow: (() -> Void)?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        // Keep system media controls available after closing the window.
        false
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if !hasVisibleWindows { reopenWindow?() }
        return true
    }

    func applicationWillTerminate(_ notification: Notification) {
        // The daemon owns audio and outlives this controller.
        model?.shutdown()
    }
}

@main
@MainActor
struct TyrannyApp: App {
    @NSApplicationDelegateAdaptor(TyrannyAppDelegate.self) private var appDelegate
    @State private var model = PlayerModel()
    @State private var started = false

    init() {
        #if DEBUG
        NativeScale.markLaunch()
        #endif
    }

    var body: some Scene {
        Window("Tyranny", id: "main") {
            MainWindow(model: model, appDelegate: appDelegate, started: $started)
        }
        .defaultSize(width: 1100, height: 720)
        .windowResizability(.contentMinSize)
    }
}

private struct MainWindow: View {
    let model: PlayerModel
    let appDelegate: TyrannyAppDelegate
    @Binding var started: Bool
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        ContentView(model: model)
            .task {
                appDelegate.reopenWindow = { [openWindow] in openWindow(id: "main") }
                guard !started else { return }
                started = true
                appDelegate.model = model
                model.start()
                #if DEBUG
                NativeSmoke.startIfRequested(model)
                NativeLifecycle.startIfRequested(model)
                NativeScale.startIfRequested(model)
                NativePlaylistSmoke.startIfRequested(model)
                NativeDragSmoke.startIfRequested(model)
                NativeFilterSmoke.startIfRequested(model)
                NativeSmartPlaylistSmoke.startIfRequested(model)
                NativeArtworkSmoke.startIfRequested(model)
                #endif
            }
    }
}
