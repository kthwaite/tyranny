import AppKit
import SwiftUI

/// Reports whether the sidebar is on screen. It watches the enclosing split view controller's
/// sidebar item, so it hears every collapse: the toolbar button, the View menu, a drag, a
/// narrow window and restored window state. A SwiftUI visibility binding misses some of these,
/// and the sidebar's views stay alive (no `onDisappear`) while collapsed.
struct SidebarVisibilityProbe: NSViewRepresentable {
    let changed: (Bool) -> Void

    func makeNSView(context: Context) -> ProbeView {
        let view = ProbeView()
        view.changed = changed
        return view
    }

    func updateNSView(_ view: ProbeView, context: Context) { view.changed = changed }

    final class ProbeView: NSView {
        var changed: ((Bool) -> Void)?
        private var observation: NSKeyValueObservation?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            observation = nil
            guard window != nil, let item = sidebarItem() else { return }
            observation = item.observe(\.isCollapsed, options: [.initial, .new]) { [weak self] item, _ in
                let visible = !item.isCollapsed
                Task { @MainActor in self?.changed?(visible) }
            }
        }

        private func sidebarItem() -> NSSplitViewItem? {
            var view = superview
            while let current = view {
                if let split = current as? NSSplitView, let controller = split.delegate as? NSSplitViewController {
                    return controller.splitViewItems.first { $0.behavior == .sidebar } ?? controller.splitViewItems.first
                }
                view = current.superview
            }
            return nil
        }
    }
}
