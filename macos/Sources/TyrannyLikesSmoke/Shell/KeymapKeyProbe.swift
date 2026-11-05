import AppKit
import SwiftUI
import TyrannyLikesSmokeCore

/// Runs the command bound to a key press in the main window.
///
/// A local monitor sees the key before menu key equivalents and before the focused field.
struct KeymapKeyProbe: NSViewRepresentable {
    let keymap: KeymapStore
    /// True while an overlay (Wrapped, the command palette) covers the window.
    let overlayOpen: () -> Bool
    /// Receives the bound command, for a key press that was consumed.
    let run: (String) -> Void

    func makeNSView(context: Context) -> ProbeView {
        let view = ProbeView()
        update(view)
        return view
    }

    func updateNSView(_ view: ProbeView, context: Context) { update(view) }

    private func update(_ view: ProbeView) {
        view.keymap = keymap
        view.overlayOpen = overlayOpen
        view.run = run
    }

    final class ProbeView: NSView {
        var keymap: KeymapStore?
        var overlayOpen: (() -> Bool)?
        var run: ((String) -> Void)?
        private var monitor: Any?

        isolated deinit { if let monitor { NSEvent.removeMonitor(monitor) } }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                guard let self, self.runsCommand(event) else { return event }
                return nil
            }
        }

        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        private func runsCommand(_ event: NSEvent) -> Bool {
            guard let chord = event.keyChord, let command = keymap?.command(for: chord) else { return false }
            guard KeyDispatch.shouldRun(
                chord,
                isMainWindow: event.window === window,
                hasSheet: window?.attachedSheet != nil,
                overlayOpen: overlayOpen?() ?? false,
                isEditingText: window?.firstResponder is NSText)
            else { return false }
            run?(command)
            return true
        }
    }
}

extension NSEvent {
    /// The chord this key press makes, or nil for a key the keymap cannot name.
    var keyChord: KeyChord? {
        var modifiers: KeyChord.Modifiers = []
        if modifierFlags.contains(.control) { modifiers.insert(.control) }
        if modifierFlags.contains(.option) { modifiers.insert(.option) }
        if modifierFlags.contains(.shift) { modifiers.insert(.shift) }
        if modifierFlags.contains(.command) { modifiers.insert(.command) }
        return KeyChord.event(
            characters: charactersIgnoringModifiers, keyCode: keyCode,
            modifiers: modifiers, capsLock: modifierFlags.contains(.capsLock))
    }
}
