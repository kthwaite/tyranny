import AppKit
import SwiftUI
import TyrannyCore

@MainActor
final class TyrannyAppDelegate: NSObject, NSApplicationDelegate {
    var model: PlayerModel?
    var reopenWindow: (() -> Void)?
    private var keyGuard: Any?
    private enum QuitState { case idle, suspending, suspended }
    private var quitState = QuitState.idle
    /// Whether AppKit waits for `reply(toApplicationShouldTerminate:)`, and whether it got it.
    private enum QuitReply { case none, pending, replied }
    private var quitReply = QuitReply.none
    /// The backstop for a `.terminateLater` wait. It is longer than the pause's 1 s deadline.
    private static let quitWatchdog = 1.5

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

    /// Quit pauses the server first, so music stops with the app.
    /// The pause runs once; a second Quit while it runs starts nothing new.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        switch quitState {
        case .suspended:
            return .terminateNow
        case .suspending:
            return deferQuit()
        case .idle:
            guard let model, model.connected else { return .terminateNow }
            quitState = .suspending
            Task {
                await model.suspendForQuit()
                quitState = .suspended
                switch quitReply {
                case .none: NSApp.terminate(nil)
                case .pending: replyToQuit()
                case .replied: break
                }
            }
            return deferQuit()
        }
    }

    /// A quit Apple event (Dock Quit, logout) gets `.terminateLater`, so a logout is not cancelled.
    /// Any other Quit may run inside a main-queue job, for example a Task that calls `NSApp.terminate`.
    /// AppKit's `.terminateLater` wait does not drain the main queue from inside that job,
    /// so the pause task could never run, and Quit would hang.
    /// So that Quit is cancelled now, and the pause task calls `terminate` again when it is done.
    private func deferQuit() -> NSApplication.TerminateReply {
        guard let event = NSAppleEventManager.shared().currentAppleEvent,
              event.eventClass == AEEventClass(kCoreEventClass),
              event.eventID == AEEventID(kAEQuitApplication) else { return .terminateCancel }
        if quitReply == .none {
            quitReply = .pending
            startQuitWatchdog()
        }
        return .terminateLater
    }

    /// Replies once, whether the pause task or the watchdog gets here first.
    private func replyToQuit() {
        guard quitReply == .pending else { return }
        quitReply = .replied
        NSApp.reply(toApplicationShouldTerminate: true)
    }

    /// Quit must never hang, even if the pause task cannot run on the main queue.
    /// The timer runs on a global queue. Its reply goes to the main run loop as a run-loop block
    /// in the common modes. AppKit's modal wait runs those blocks without draining the main queue.
    /// If the watchdog wins while the pause is still in flight, the app exits before it writes
    /// the quit note. The pause may still reach the server, which is then left paused with no
    /// note, so the next launch does not resume it.
    private func startQuitWatchdog() {
        DispatchQueue.global().asyncAfter(deadline: .now() + Self.quitWatchdog) { [weak self] in
            let main = CFRunLoopGetMain()
            CFRunLoopPerformBlock(main, CFRunLoopMode.commonModes.rawValue) {
                MainActor.assumeIsolated { self?.replyToQuit() }
            }
            CFRunLoopWakeUp(main)
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        // The daemon outlives this controller; quit paused it first.
        model?.shutdown()
    }
}

@main
@MainActor
struct TyrannyApp: App {
    @NSApplicationDelegateAdaptor(TyrannyAppDelegate.self) private var appDelegate
    @State private var model = PlayerModel(
        quitSuspensions: .forLaunch(arguments: CommandLine.arguments),
        importNotes: .forLaunch(arguments: CommandLine.arguments))
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
                    .disabled(model.transport.nowPlaying?.track_id == nil)
            }
            CommandMenu("Go") {
                ForEach(Array((Destination.library + Destination.playing).enumerated()), id: \.offset) { index, destination in
                    Button(destination.title) {
                        model.endImport()
                        navigation.destination = destination
                    }
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
                NativeCommandSmoke.startIfRequested(model)
                NativeSmoke.startIfRequested(model, navigation: navigation)
                NativeLifecycle.startIfRequested(model)
                NativeScale.startIfRequested(model, navigation: navigation)
                NativeFilterSmoke.startIfRequested(model, navigation: navigation)
                NativeArtworkSmoke.startIfRequested(model, navigation: navigation)
                NativeLyricsSmoke.startIfRequested(model)
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
