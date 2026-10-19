import AppKit
import SwiftUI
import TyrannyCore

@MainActor
final class TyrannyAppDelegate: NSObject, NSApplicationDelegate {
    var model: PlayerModel?
    var reopenWindow: (() -> Void)?
    private var keyGuard: Any?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate(ignoringOtherApps: true)
        installTextFieldKeyGuard()
    }

    /// Menu key equivalents beat text fields. ⌘← and ⌘→ (Previous and Next) must still move the
    /// cursor while a field editor has focus, so the editor gets those keys first. Space and
    /// Return are not menu shortcuts: only the focused track table sees them.
    private func installTextFieldKeyGuard() {
        keyGuard = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
                .subtracting([.numericPad, .function, .capsLock])
            guard flags == .command, event.keyCode == 123 || event.keyCode == 124,
                  let editor = (event.window ?? NSApp.keyWindow)?.firstResponder as? NSTextView,
                  editor.isFieldEditor || editor.isEditable else { return event }
            editor.keyDown(with: event)
            return nil
        }
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
    @State private var navigation = NavigationModel()
    @State private var started = false

    init() {
        #if DEBUG
        NativeScale.markLaunch()
        #endif
    }

    var body: some Scene {
        Window("Tyranny", id: "main") {
            MainWindow(model: model, navigation: navigation, appDelegate: appDelegate, started: $started)
        }
        .defaultSize(width: 1180, height: 740)
        .windowResizability(.contentMinSize)
        .windowToolbarStyle(.unified(showsTitle: false))
        .commands {
            // ⌘F focuses the toolbar filter; the window has no text find.
            CommandGroup(replacing: .textEditing) {
                Button("Filter") { navigation.focusFilter() }
                    .keyboardShortcut("f")
            }
            CommandGroup(after: .sidebar) {
                Button(navigation.columnBrowserVisible ? "Hide Column Browser" : "Show Column Browser") {
                    navigation.columnBrowserVisible.toggle()
                }
                .keyboardShortcut("b")
                Button(navigation.upNextDrawerVisible ? "Hide Up Next Drawer" : "Show Up Next Drawer") {
                    navigation.upNextDrawerVisible.toggle()
                }
                .keyboardShortcut("u", modifiers: [.command, .option])
            }
            // Space plays and pauses inside the track tables, where typing is not at stake.
            CommandMenu("Playback") {
                Button("Play/Pause") { model.transport.playPause() }
                    .disabled(!model.transport.canPlayPause)
                Button("Next") { model.transport.next() }
                    .keyboardShortcut(.rightArrow, modifiers: .command)
                    .disabled(!model.transport.canNext)
                Button("Previous") { model.transport.previous() }
                    .keyboardShortcut(.leftArrow, modifiers: .command)
                    .disabled(!model.transport.canPrevious)
                Divider()
                Button("Show Playing Track") { navigation.revealPlaying() }
                    .keyboardShortcut("l")
            }
            CommandMenu("Go") {
                ForEach(Array((Destination.library + Destination.playing).enumerated()), id: \.offset) { index, destination in
                    Button(destination.title) { navigation.destination = destination }
                        .keyboardShortcut(KeyEquivalent(Character(String(index + 1))), modifiers: .command)
                }
            }
        }
        Settings {
            SettingsView(model: model, navigation: navigation)
        }
    }
}

private struct MainWindow: View {
    let model: PlayerModel
    let navigation: NavigationModel
    let appDelegate: TyrannyAppDelegate
    @Binding var started: Bool
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        MainWindowView(model: model, navigation: navigation)
            .task {
                appDelegate.reopenWindow = { [openWindow] in openWindow(id: "main") }
                guard !started else { return }
                started = true
                appDelegate.model = model
                model.start()
                #if DEBUG
                NativeModelGate.startIfRequested(model)
                NativeSmoke.startIfRequested(model, navigation: navigation)
                NativeLifecycle.startIfRequested(model)
                NativeScale.startIfRequested(model, navigation: navigation)
                NativeFilterSmoke.startIfRequested(model, navigation: navigation)
                NativeArtworkSmoke.startIfRequested(model, navigation: navigation)
                NativeShellSmoke.startIfRequested(model, navigation: navigation)
                NativeSongsSmoke.startIfRequested(model, navigation: navigation)
                NativeAlbumsSmoke.startIfRequested(model, navigation: navigation)
                NativeArtistsSmoke.startIfRequested(model, navigation: navigation)
                NativeHistorySmoke.startIfRequested(model, navigation: navigation)
                NativeUpNextSmoke.startIfRequested(model, navigation: navigation)
                NativePlaylistSmoke.startIfRequested(model, navigation: navigation)
                NativeSmartPlaylistSmoke.startIfRequested(model, navigation: navigation)
                NativeRunsSmoke.startIfRequested(model, navigation: navigation)
                NativeRenderViews.startIfRequested(model, navigation: navigation)
                #endif
            }
    }
}
