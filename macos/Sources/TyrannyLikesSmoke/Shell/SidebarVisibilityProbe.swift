import AppKit
import SwiftUI

/// Reports whether the sidebar is on screen. It sits in the detail column, which the split view
/// always loads, and watches the enclosing split view controller's sidebar item. So it hears
/// every collapse: the toolbar button, the View menu, a drag, a narrow window and restored
/// window state. A SwiftUI visibility binding misses some of these. A probe in the sidebar
/// itself would miss a launch with the sidebar collapsed, because a collapsed item's view is
/// never loaded.
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

        /// The sidebar item of the nearest enclosing split view controller.
        func sidebarItem() -> NSSplitViewItem? {
            var view = superview
            while let current = view {
                if let split = current as? NSSplitView, let controller = split.delegate as? NSSplitViewController {
                    return controller.splitViewItems.first { $0.behavior == .sidebar }
                }
                view = current.superview
            }
            return nil
        }
    }
}
